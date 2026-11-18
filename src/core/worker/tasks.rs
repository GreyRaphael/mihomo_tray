//! Guarded background latency & subscription tasks for [`super::Worker`].

use std::sync::mpsc::Sender;

use super::{ActionError, CommandOutcome, Worker};
use crate::config::mihomo;
use crate::core::api::Client;
use crate::state::{AsyncTaskKind, Provider, WorkerCommand};
use crate::text;

#[derive(Default)]
pub(super) struct TaskSlot {
    active_generation: Option<u64>,
    generation: u64,
}

impl TaskSlot {
    pub fn is_active(&self) -> bool {
        self.active_generation.is_some()
    }

    pub fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.active_generation = Some(self.generation);
        self.generation
    }

    fn bump_pending_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.active_generation = None;
    }

    pub fn complete(&mut self, task_gen: u64) -> bool {
        if self.active_generation != Some(task_gen) {
            return false;
        }
        self.active_generation = None;
        true
    }
}

#[derive(Default)]
pub(super) struct SubscriptionTaskState {
    slot: TaskSlot,
    pending_apply_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskCompletion {
    Stale,
    ChainedReload(u64),
    Current,
}

impl SubscriptionTaskState {
    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        self.slot.is_active()
    }

    fn begin_refresh(&mut self) -> Option<u64> {
        (!self.slot.is_active()).then(|| self.slot.begin())
    }

    fn begin_or_queue_apply(&mut self) -> (u64, bool) {
        if self.slot.is_active() {
            let generation = self.slot.bump_pending_generation();
            self.pending_apply_generation = Some(generation);
            (generation, false)
        } else {
            (self.slot.begin(), true)
        }
    }

    fn activate_chained_apply(&mut self, generation: u64) {
        self.slot.active_generation = Some(generation);
    }

    pub fn invalidate(&mut self) {
        self.slot.invalidate();
        self.pending_apply_generation = None;
    }

    fn complete(&mut self, task_gen: u64) -> TaskCompletion {
        if !self.slot.complete(task_gen) {
            return TaskCompletion::Stale;
        }
        if let Some(next_gen) = self.pending_apply_generation.take() {
            return TaskCompletion::ChainedReload(next_gen);
        }
        TaskCompletion::Current
    }
}

impl Worker {
    fn spawn_background_task<F>(
        &self,
        name: &str,
        fallback_kind: AsyncTaskKind,
        status_note: Option<&'static str>,
        task: F,
    ) -> bool
    where
        F: FnOnce() -> (AsyncTaskKind, Option<String>) + Send + 'static,
    {
        if let Some(note) = status_note {
            self.set_status_note(Some(note));
        }
        let tx = self.tx.clone();
        if std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                struct PanicGuard {
                    tx: Sender<WorkerCommand>,
                    kind: AsyncTaskKind,
                    sent: bool,
                }
                impl Drop for PanicGuard {
                    fn drop(&mut self) {
                        if !self.sent {
                            let _ = self.tx.send(WorkerCommand::AsyncDone {
                                kind: self.kind,
                                error: Some("background task panicked".to_string()),
                            });
                        }
                    }
                }
                let mut guard = PanicGuard {
                    tx: tx.clone(),
                    kind: fallback_kind,
                    sent: false,
                };
                let (kind, error) = task();
                guard.sent = true;
                let _ = tx.send(WorkerCommand::AsyncDone { kind, error });
            })
            .is_err()
        {
            if status_note.is_some() {
                self.set_status_note(None);
            }
            return false;
        }
        true
    }

    pub(super) fn start_test_group_latency(
        &mut self,
        group: String,
    ) -> Result<CommandOutcome, ActionError> {
        let client = self.require_client()?.clone();
        if self.latency_tasks.is_active() {
            return Ok(CommandOutcome::KeepError);
        }
        let generation = self.latency_tasks.begin();
        if self.spawn_background_task(
            "mihomo-latency",
            AsyncTaskKind::LatencyTest { generation },
            None,
            move || {
                (
                    AsyncTaskKind::LatencyTest { generation },
                    client.test_group_delay(&group).err(),
                )
            },
        ) {
            Ok(CommandOutcome::ClearError)
        } else {
            self.latency_tasks.complete(generation);
            Ok(CommandOutcome::KeepError)
        }
    }

    pub(super) fn start_refresh_subscriptions(&mut self) -> Result<CommandOutcome, ActionError> {
        let client = self.require_client()?.clone();
        let Some(generation) = self.sub_tasks.begin_refresh() else {
            return Ok(CommandOutcome::KeepError);
        };
        if self.spawn_background_task(
            "mihomo-sub-refresh",
            AsyncTaskKind::RefreshSubscriptions { generation },
            Some(text::STATUS_REFRESHING_SUBSCRIPTION),
            move || {
                let err = (|| -> Result<(), String> {
                    let providers = client.get_providers()?.providers;
                    let refreshable: Vec<String> = providers
                        .into_iter()
                        .filter(Provider::refreshable)
                        .map(|p| p.name)
                        .collect();
                    if refreshable.is_empty() {
                        return Err(text::ERR_NO_SUBSCRIPTION_PROVIDER.to_string());
                    }
                    for name in refreshable {
                        client.refresh_provider(&name)?;
                    }
                    Ok(())
                })()
                .err();
                (AsyncTaskKind::RefreshSubscriptions { generation }, err)
            },
        ) {
            Ok(CommandOutcome::ClearError)
        } else {
            self.sub_tasks.complete(generation);
            Ok(CommandOutcome::KeepError)
        }
    }

    fn spawn_apply_subscription_reload(&mut self, client: Client, generation: u64) {
        let sub_cache = self
            .cfg
            .home_dir()
            .join(mihomo::SUBSCRIPTION_LOCAL_PATH.trim_start_matches("./"));
        self.sub_tasks.activate_chained_apply(generation);
        if !self.spawn_background_task(
            "mihomo-sub-apply",
            AsyncTaskKind::ApplySubscription {
                generation,
                reload_ok: false,
            },
            Some(text::STATUS_UPDATING_SUBSCRIPTION),
            move || {
                let had_cached_sub = sub_cache.is_file();
                if let Err(err) = client.reload_configs() {
                    return (
                        AsyncTaskKind::ApplySubscription {
                            generation,
                            reload_ok: false,
                        },
                        Some(err),
                    );
                }
                // When no cache existed before, `reload_configs` already fetched the new provider;
                // only force a provider refresh if replacing an existing cache or if reload didn't create it.
                let err = if had_cached_sub || !sub_cache.is_file() {
                    client
                        .refresh_provider(mihomo::SUBSCRIPTION_PROVIDER_NAME)
                        .err()
                } else {
                    None
                };
                (
                    AsyncTaskKind::ApplySubscription {
                        generation,
                        reload_ok: true,
                    },
                    err,
                )
            },
        ) {
            self.sub_tasks.complete(generation);
            self.state_sync.resolve_deferred(generation, true);
        }
    }

    pub(super) fn set_subscription_url(&mut self, url: &str) -> Result<(), ActionError> {
        let saved = self.capture_live_or_persisted_state();
        mihomo::apply_subscription_url(self.cfg.config_file(), url)?;
        if let Some(client) = self.client.clone().filter(|c| c.probe().is_ok()) {
            let (generation, spawn_now) = self.sub_tasks.begin_or_queue_apply();
            self.state_sync.defer_restore(generation, saved);
            if spawn_now {
                self.spawn_apply_subscription_reload(client, generation);
            } else {
                self.set_status_note(Some(text::STATUS_UPDATING_SUBSCRIPTION));
            }
            return Ok(());
        }
        self.sub_tasks.invalidate();
        self.state_sync.arm_ready_restore(saved);
        self.start_core_internal()
    }

    pub(super) fn handle_async_done(
        &mut self,
        kind: AsyncTaskKind,
        error: Option<String>,
    ) -> Result<CommandOutcome, ActionError> {
        match kind {
            AsyncTaskKind::LatencyTest { generation } => {
                if !self.latency_tasks.complete(generation) {
                    if let Some(err) = error {
                        tracing::debug!("Ignoring stale latency task error: {err}");
                    }
                    return Ok(CommandOutcome::KeepError);
                }
                match error {
                    Some(err) => Err(ActionError::general(err)),
                    None => Ok(CommandOutcome::KeepError),
                }
            }
            AsyncTaskKind::RefreshSubscriptions { generation }
            | AsyncTaskKind::ApplySubscription { generation, .. } => {
                match self.sub_tasks.complete(generation) {
                    TaskCompletion::Stale => {
                        if let Some(err) = error {
                            tracing::debug!("Ignoring stale subscription task error: {err}");
                        }
                        Ok(CommandOutcome::KeepError)
                    }
                    TaskCompletion::ChainedReload(next_gen) => {
                        if let Some(err) = error {
                            tracing::warn!("Superseded subscription task failed: {err}");
                        }
                        self.chain_pending_sub_reload(next_gen);
                        Ok(CommandOutcome::KeepError)
                    }
                    TaskCompletion::Current => {
                        self.set_status_note(None);
                        if let AsyncTaskKind::ApplySubscription { reload_ok, .. } = kind {
                            self.state_sync.resolve_deferred(generation, reload_ok);
                        }
                        match error {
                            Some(err) => Err(ActionError::general(err)),
                            None => Ok(CommandOutcome::KeepError),
                        }
                    }
                }
            }
        }
    }

    fn chain_pending_sub_reload(&mut self, generation: u64) {
        if let Some(client) = self.client.clone().filter(|c| c.probe().is_ok()) {
            self.spawn_apply_subscription_reload(client, generation);
        } else {
            self.set_status_note(None);
            self.state_sync.resolve_deferred(generation, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::config::state_store::PersistedState;
    use crate::config::tray::TraySettings;
    use crate::core::api::{Client, CoreConfigs};
    use crate::core::worker::tests::{read_http_request, test_worker};
    use crate::state::ProxyMode;

    #[test]
    fn subscription_async_reload_preserves_pending_restore_until_async_done() {
        let root = crate::config::test_temp_dir("worker_sub_restore");
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            cfg_dir.join("config.yaml"),
            format!("mixed-port: 7890\nmode: rule\nexternal-controller: 127.0.0.1:{port}\n"),
        )
        .unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut mode = "rule".to_string();
            let mut tun_on = false;
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                let (status, body) = if req.starts_with("GET /version ") {
                    ("200 OK", r#"{"version":"v1.19.0"}"#.to_string())
                } else if req.starts_with("GET /configs ") {
                    (
                        "200 OK",
                        format!(
                            r#"{{"mode":"{mode}","mixed-port":7890,"tun":{{"enable":{tun_on}}}}}"#
                        ),
                    )
                } else if req.starts_with("PUT /configs?force=true ") {
                    mode = "rule".to_string();
                    tun_on = false;
                    ("204 No Content", String::new())
                } else if req.starts_with("PUT /providers/proxies/subscription ") {
                    ("204 No Content", String::new())
                } else if req.starts_with("PATCH /configs ") {
                    let (_, payload) = req.split_once("\r\n\r\n").unwrap();
                    let json: serde_json::Value = serde_json::from_str(payload).unwrap();
                    if let Some(m) = json.get("mode").and_then(|v| v.as_str()) {
                        mode = m.to_string();
                    }
                    if let Some(t) = json
                        .get("tun")
                        .and_then(|v| v.get("enable"))
                        .and_then(|v| v.as_bool())
                    {
                        tun_on = t;
                    }
                    ("204 No Content", String::new())
                } else if req.starts_with("GET /proxies ") {
                    ("200 OK", r#"{"proxies":{}}"#.to_string())
                } else if req.starts_with("GET /providers/proxies ") {
                    ("200 OK", r#"{"providers":{}}"#.to_string())
                } else if req.starts_with("POST /__stop ") {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    break;
                } else {
                    ("404 Not Found", String::new())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
                if req.starts_with("PATCH /configs ") && mode == "global" && !tun_on {
                    let _ = done_tx.send(());
                }
            }
        });

        let (mut worker, rx, _shared) = test_worker(&root, TraySettings::default(), None);
        worker.client = Some(Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap());
        worker.live.controller_ok = true;
        worker.live.mode = "global".to_string();
        worker.live.tun = false;
        worker.state_sync.set_persisted_for_test(PersistedState {
            system_proxy: false,
            mode: Some("global".to_string()),
            tun: Some(false),
        });

        assert_eq!(
            worker
                .handle_command(WorkerCommand::SetSubscriptionUrl(
                    "https://example.com/sub".to_string()
                ))
                .unwrap(),
            CommandOutcome::ClearError
        );
        assert!(!worker.state_sync.has_ready_restore());
        assert!(worker.state_sync.has_deferred_restore());

        let mut mid_cfg = CoreConfigs {
            mode: "rule".to_string(),
            mixed_port: 7890,
            tun_enable: false,
        };
        worker.apply_pending_restore_if_needed(&mut mid_cfg);
        worker
            .state_sync
            .sync_live_if_idle(worker.cfg.home_dir(), false, "rule", false);
        assert_eq!(
            worker
                .state_sync
                .persisted()
                .and_then(|s| s.mode.as_deref()),
            Some("global")
        );

        let async_cmd = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.handle_command(async_cmd).unwrap();
        assert!(worker.state_sync.has_ready_restore());

        let polled = worker.poll_controller_configs();
        assert!(polled.controller_ok);
        assert_eq!(polled.mode, "global");
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // Explicit SetMode during deferred reload must survive rather than be reverted.
        assert_eq!(
            worker
                .handle_command(WorkerCommand::SetSubscriptionUrl(
                    "https://example.com/sub2".to_string()
                ))
                .unwrap(),
            CommandOutcome::ClearError
        );
        assert!(worker.state_sync.has_deferred_restore());
        worker
            .handle_command(WorkerCommand::SetMode(ProxyMode::Direct))
            .unwrap();
        let async_cmd2 = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.handle_command(async_cmd2).unwrap();
        let polled2 = worker.poll_controller_configs();
        assert!(polled2.controller_ok);
        assert_eq!(polled2.mode, "direct");

        let _ = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .unwrap()
            .write_all(b"POST /__stop HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalidated_subscription_task_does_not_block_or_clear_new_task() {
        let root = crate::config::test_temp_dir("worker_sub_invalidate");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                let resp = if req.starts_with("GET /providers/proxies ") {
                    let body = r#"{"providers":{"subscription":{"vehicleType":"HTTP","updatedAt":"2025-01-01T08:00:00+08:00"}}}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string()
                };
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let (mut worker, rx, shared) = test_worker(&root, TraySettings::default(), None);
        worker.client = Some(Client::new(&format!("127.0.0.1:{port}"), "", 2000).unwrap());

        worker.sub_tasks.invalidate();
        let stale_gen = 0;
        worker
            .handle_command(WorkerCommand::RefreshSubscriptions)
            .unwrap();
        assert!(worker.sub_tasks.is_active());
        assert_eq!(
            shared.lock().unwrap().status_note.as_deref(),
            Some(text::STATUS_REFRESHING_SUBSCRIPTION)
        );

        worker
            .handle_command(WorkerCommand::AsyncDone {
                kind: AsyncTaskKind::RefreshSubscriptions {
                    generation: stale_gen,
                },
                error: Some("stale timeout".to_string()),
            })
            .unwrap();
        assert!(worker.sub_tasks.is_active());
        assert_eq!(
            shared.lock().unwrap().status_note.as_deref(),
            Some(text::STATUS_REFRESHING_SUBSCRIPTION)
        );
        assert_eq!(shared.lock().unwrap().action_error, None);

        let real_done = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.handle_command(real_done).unwrap();
        assert!(!worker.sub_tasks.is_active());
        assert_eq!(shared.lock().unwrap().status_note, None);

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalidated_latency_task_ignores_stale_completion_and_allows_retry() {
        let root = crate::config::test_temp_dir("worker_latency_invalidate");
        let (mut worker, _rx, _shared) = test_worker(&root, TraySettings::default(), None);

        let gen0 = worker.latency_tasks.begin();
        worker.latency_tasks.invalidate();
        assert!(!worker.latency_tasks.is_active());

        let gen1 = worker.latency_tasks.begin();

        // Stale gen0 failure must be ignored without clearing active gen1.
        let outcome = worker
            .handle_command(WorkerCommand::AsyncDone {
                kind: AsyncTaskKind::LatencyTest { generation: gen0 },
                error: Some("stale latency timeout".to_string()),
            })
            .unwrap();
        assert_eq!(outcome, CommandOutcome::KeepError);
        assert!(worker.latency_tasks.is_active());

        let err = worker
            .handle_command(WorkerCommand::AsyncDone {
                kind: AsyncTaskKind::LatencyTest { generation: gen1 },
                error: Some("real latency error".to_string()),
            })
            .unwrap_err();
        assert_eq!(err.message, "real latency error");
        assert!(!worker.latency_tasks.is_active());

        let _ = std::fs::remove_dir_all(&root);
    }
}

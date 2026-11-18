//! Guarded background latency & subscription tasks for [`super::Worker`].

use std::sync::mpsc::Sender;

use super::{CommandOutcome, Worker};
use crate::config::mihomo;
use crate::core::api::Client;
use crate::state::{AsyncTaskKind, Provider, WorkerCommand};
use crate::text;

#[derive(Default)]
pub(super) struct SubscriptionTaskState {
    active_generation: Option<u64>,
    pending_reload: bool,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskCompletion {
    Stale,
    ChainedReload,
    Current,
}

impl SubscriptionTaskState {
    pub fn is_active(&self) -> bool {
        self.active_generation.is_some()
    }

    fn mark_started(&mut self, generation: u64) {
        self.active_generation = Some(generation);
    }

    fn bump_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.active_generation = None;
        self.pending_reload = false;
    }

    fn complete(&mut self, task_gen: u64) -> TaskCompletion {
        if self.active_generation != Some(task_gen) {
            return TaskCompletion::Stale;
        }
        self.active_generation = None;
        if self.pending_reload {
            self.pending_reload = false;
            return TaskCompletion::ChainedReload;
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
    ) -> Result<CommandOutcome, String> {
        let client = self.require_client()?.clone();
        if self.testing_latency {
            return Ok(CommandOutcome::KeepError);
        }
        if self.spawn_background_task(
            "mihomo-latency",
            AsyncTaskKind::LatencyTest,
            None,
            move || {
                (
                    AsyncTaskKind::LatencyTest,
                    client.test_group_delay(&group).err(),
                )
            },
        ) {
            self.testing_latency = true;
            Ok(CommandOutcome::ClearError)
        } else {
            Ok(CommandOutcome::KeepError)
        }
    }

    pub(super) fn start_refresh_subscriptions(&mut self) -> Result<CommandOutcome, String> {
        let client = self.require_client()?.clone();
        if self.sub_tasks.is_active() {
            return Ok(CommandOutcome::KeepError);
        }
        let generation = self.sub_tasks.generation;
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
            self.sub_tasks.mark_started(generation);
            Ok(CommandOutcome::ClearError)
        } else {
            Ok(CommandOutcome::KeepError)
        }
    }

    fn spawn_apply_subscription_reload(&mut self, client: Client, generation: u64) {
        let sub_cache = self
            .cfg
            .home_dir()
            .join(mihomo::SUBSCRIPTION_LOCAL_PATH.trim_start_matches("./"));
        if self.spawn_background_task(
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
            self.sub_tasks.mark_started(generation);
        } else {
            self.state_sync.resolve_deferred(generation, true);
        }
    }

    pub(super) fn set_subscription_url(&mut self, url: &str) -> Result<(), String> {
        let saved = self.capture_live_or_persisted_state();
        mihomo::apply_subscription_url(self.cfg.config_file(), url)?;
        if let Some(client) = self.client.clone().filter(|c| c.probe().is_ok()) {
            let generation = self.sub_tasks.bump_generation();
            self.state_sync.defer_restore(generation, saved);
            if self.sub_tasks.is_active() {
                self.sub_tasks.pending_reload = true;
                self.set_status_note(Some(text::STATUS_UPDATING_SUBSCRIPTION));
            } else {
                self.spawn_apply_subscription_reload(client, generation);
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
    ) -> Result<CommandOutcome, String> {
        match kind {
            AsyncTaskKind::LatencyTest => {
                self.testing_latency = false;
                match error {
                    Some(err) => Err(err),
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
                    TaskCompletion::ChainedReload => {
                        if let Some(err) = error {
                            tracing::warn!("Superseded subscription task failed: {err}");
                        }
                        self.chain_pending_sub_reload();
                        Ok(CommandOutcome::KeepError)
                    }
                    TaskCompletion::Current => {
                        self.set_status_note(None);
                        if let AsyncTaskKind::ApplySubscription { reload_ok, .. } = kind {
                            self.state_sync.resolve_deferred(generation, reload_ok);
                        }
                        match error {
                            Some(err) => Err(err),
                            None => Ok(CommandOutcome::KeepError),
                        }
                    }
                }
            }
        }
    }

    fn chain_pending_sub_reload(&mut self) {
        let generation = self.sub_tasks.generation;
        if let Some(client) = self.client.clone().filter(|c| c.probe().is_ok()) {
            self.spawn_apply_subscription_reload(client, generation);
        } else {
            self.set_status_note(None);
            self.state_sync.resolve_deferred(generation, true);
        }
    }
}

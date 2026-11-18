//! Portable discovery for mihomo binary, home/config, external-ui, and controller address.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::config::mihomo::{self, MihomoConfigInfo};
use crate::config::tray::{self, TraySettings};
use crate::core::api::Client;
use crate::core::process::{self, RunningProcess};
use crate::platform;
use crate::text::{ERR_NO_CONTROLLER, MIHOMO_CONFIG_NAME};

pub fn find_kernel(
    portable_root: &Path,
    settings: &TraySettings,
) -> Result<Option<PathBuf>, String> {
    if let Some(declared) = tray::resolve_portable_path(portable_root, &settings.mihomo.path) {
        if !declared.is_file() {
            return Err(format!(
                "Declared mihomo.path does not exist: {}",
                declared.display()
            ));
        }
        return Ok(Some(declared));
    }
    Ok(discover_kernel(
        portable_root,
        std::env::var_os("PATH").as_deref(),
    ))
}

fn discover_kernel(portable_root: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let exact_name = platform::kernel_exe_name();
    let exact_candidates = [
        portable_root.join(exact_name),
        portable_root.join("core").join(exact_name),
        portable_root.join("bin").join(exact_name),
    ];
    for candidate in exact_candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Allow users to drop an unrenamed release binary (e.g. mihomo-windows-amd64-v3.exe)
    // into ./core/ or ./ as long as there is an unambiguous single candidate.
    for dir in [portable_root.join("core"), portable_root.to_path_buf()] {
        if let Some(single) = find_single_kernel_in_dir(&dir) {
            return Some(single);
        }
    }

    if let Some(path_var) = path_var
        && let Ok(mut matches) = which::which_in_global(exact_name, Some(path_var))
        && let Some(candidate) = matches.next()
    {
        let target = platform::resolve_shim_target(&candidate).unwrap_or(candidate);
        return Some(target);
    }

    None
}

fn find_single_kernel_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut matches: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = entry.file_name();
            let name_str = name.to_str()?;
            platform::is_kernel_candidate_name(name_str).then_some(path)
        })
        .collect();
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

/// Resolves mihomo's `-d` home directory and `-f` configuration file.
/// Defaults to `<portable_root>/config` so geodata and cache stay inside the portable directory.
pub fn resolve_home_and_config(
    portable_root: &Path,
    settings: &TraySettings,
    kernel_path: Option<&Path>,
) -> (PathBuf, PathBuf) {
    let declared_home = tray::resolve_portable_path(portable_root, &settings.mihomo.home);
    let declared_config = tray::resolve_portable_path(portable_root, &settings.mihomo.config);

    let config_file = if let Some(cfg) = declared_config {
        cfg
    } else if let Some(home) = &declared_home {
        home.join(MIHOMO_CONFIG_NAME)
    } else {
        discover_portable_config(portable_root, kernel_path)
    };

    let home_dir = declared_home.unwrap_or_else(|| {
        config_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| portable_root.join("config"))
    });

    (home_dir, config_file)
}

fn discover_portable_config(portable_root: &Path, kernel_path: Option<&Path>) -> PathBuf {
    let config_dir_file = portable_root.join("config").join(MIHOMO_CONFIG_NAME);
    if config_dir_file.is_file() || portable_root.join("config").is_dir() {
        return config_dir_file;
    }
    let root_file = portable_root.join(MIHOMO_CONFIG_NAME);
    if root_file.is_file() {
        return root_file;
    }
    if let Some(kernel_dir) = kernel_path.and_then(Path::parent) {
        let beside_kernel = kernel_dir.join(MIHOMO_CONFIG_NAME);
        if beside_kernel.is_file() {
            return beside_kernel;
        }
    }
    config_dir_file
}

/// Resolves optional external-ui directory to pass via `-ext-ui`.
fn resolve_external_ui(
    portable_root: &Path,
    settings: &TraySettings,
    config_info: &MihomoConfigInfo,
) -> Option<PathBuf> {
    if has_flag(&settings.mihomo.args, "ext-ui") {
        return None;
    }
    if let Some(explicit) = tray::resolve_portable_path(portable_root, &settings.ui.external_ui) {
        return Some(explicit);
    }
    if config_info.external_ui.is_some() {
        return None;
    }
    let portable_ui = portable_root.join("ui");
    portable_ui
        .join("index.html")
        .is_file()
        .then_some(portable_ui)
}

/// Builds the exact command-line arguments passed when spawning mihomo.
pub fn build_launch_args(
    portable_root: &Path,
    settings: &TraySettings,
    kernel_path: Option<&Path>,
) -> Result<Vec<String>, String> {
    let (home_dir, config_file) = resolve_home_and_config(portable_root, settings, kernel_path);
    build_launch_args_for_paths(portable_root, settings, &home_dir, &config_file)
}

pub(crate) fn build_launch_args_for_paths(
    portable_root: &Path,
    settings: &TraySettings,
    home_dir: &Path,
    config_file: &Path,
) -> Result<Vec<String>, String> {
    for (flag, owner) in [
        ("d", "mihomo.home in tray.yml"),
        ("f", "mihomo.config in tray.yml"),
        ("ext-ctl", "external-controller in config.yaml"),
        ("secret", "secret in config.yaml"),
    ] {
        if has_flag(&settings.mihomo.args, flag) {
            return Err(format!(
                "Do not pass -{flag} in mihomo.args; configure {owner} instead."
            ));
        }
    }
    if !settings.ui.external_ui.trim().is_empty() && has_flag(&settings.mihomo.args, "ext-ui") {
        return Err(
            "Do not pass -ext-ui in mihomo.args when ui.external_ui is configured in tray.yml."
                .to_string(),
        );
    }

    let config_info = mihomo::read_file(config_file).unwrap_or_default();

    let mut args: Vec<String> = settings
        .mihomo
        .args
        .iter()
        .map(|arg| platform::expand_env(arg))
        .collect();

    args.push("-d".to_string());
    args.push(home_dir.display().to_string());
    args.push("-f".to_string());
    args.push(config_file.display().to_string());

    if let Some(ui_dir) = resolve_external_ui(portable_root, settings, &config_info) {
        args.push("-ext-ui".to_string());
        args.push(ui_dir.display().to_string());
    }

    // Materialize CLASH_OVERRIDE_* into explicit flags so elevated child processes preserve them.
    if let Some(ctl) = env_non_empty("CLASH_OVERRIDE_EXTERNAL_CONTROLLER") {
        args.push("-ext-ctl".to_string());
        args.push(ctl);
    }
    if let Some(secret) = env_non_empty("CLASH_OVERRIDE_SECRET") {
        args.push("-secret".to_string());
        args.push(secret);
    }

    Ok(args)
}

/// Resolves the controller HTTP client from argv -> env -> config.yaml -> tray.yml fallback.
pub fn resolve_controller(
    settings: &TraySettings,
    running: &[RunningProcess],
    config_path: &Path,
) -> Result<Client, String> {
    let cfg_res = mihomo::read_file(config_path);

    let argv = matching_running_argv(running, config_path);
    let argv_ctl = argv.as_deref().and_then(|a| flag_value(a, "ext-ctl"));
    let argv_secret = argv.as_deref().and_then(|a| flag_value(a, "secret"));

    let env_ctl = env_non_empty("CLASH_OVERRIDE_EXTERNAL_CONTROLLER");
    let env_secret = env_non_empty("CLASH_OVERRIDE_SECRET");

    let cfg_ctl = cfg_res
        .as_ref()
        .ok()
        .and_then(|c| c.external_controller.clone());
    let cfg_secret = cfg_res.as_ref().ok().and_then(|c| c.secret.clone());
    let other_controller_family = cfg_res.as_ref().is_ok_and(|c| c.other_controller_family);

    let address = argv_ctl.or(env_ctl).or(cfg_ctl).or_else(|| {
        let trimmed = settings.controller.address.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });

    let Some(address) = address else {
        if let Err(err) = cfg_res
            && config_path.is_file()
        {
            return Err(err);
        }
        if other_controller_family {
            return Err(format!(
                "{} configures only TLS/unix/pipe external-controller, which is unsupported.",
                config_path.display()
            ));
        }
        return Err(ERR_NO_CONTROLLER.to_string());
    };

    let secret = argv_secret
        .or(env_secret)
        .or(cfg_secret)
        .unwrap_or_else(|| settings.controller.secret.trim().to_string());

    Client::new(&address, &secret, settings.controller.timeout_ms)
        .map_err(|err| err.message(&address))
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn matching_running_argv(running: &[RunningProcess], wanted_config: &Path) -> Option<Vec<String>> {
    for proc in running {
        if let Some(cfg) = argv_config(&proc.cmd)
            && process::paths_match(&cfg, wanted_config)
        {
            return Some(proc.cmd.clone());
        }
    }
    if running.len() == 1 && !running[0].cmd.is_empty() {
        return Some(running[0].cmd.clone());
    }
    None
}

fn argv_config(args: &[String]) -> Option<PathBuf> {
    if let Some(f) = flag_value(args, "f") {
        let p = PathBuf::from(f);
        return p.is_absolute().then_some(p);
    }
    if let Some(d) = flag_value(args, "d") {
        let p = PathBuf::from(d);
        return p.is_absolute().then(|| p.join(MIHOMO_CONFIG_NAME));
    }
    None
}

fn has_flag(args: &[String], flag: &str) -> bool {
    let short = format!("-{flag}");
    let long = format!("--{flag}");
    args.iter().any(|arg| {
        arg == &short
            || arg == &long
            || arg.starts_with(&format!("{short}="))
            || arg.starts_with(&format!("{long}="))
    })
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let short = format!("-{flag}");
    let long = format!("--{flag}");
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == &short || arg == &long {
            if let Some(next) = args.get(i + 1)
                && !next.is_empty()
            {
                found = Some(next.clone());
            }
            i += 2;
            continue;
        }
        if let Some(val) = arg
            .strip_prefix(&format!("{short}="))
            .or_else(|| arg.strip_prefix(&format!("{long}=")))
            && !val.is_empty()
        {
            found = Some(val.to_string());
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_temp_dir as temp_dir;

    #[test]
    fn discovers_unrenamed_single_kernel_in_core_dir() {
        let root = temp_dir("single_core");
        let core_dir = root.join("core");
        std::fs::create_dir_all(&core_dir).unwrap();
        let bin = core_dir.join(if cfg!(windows) {
            "mihomo-windows-amd64-v3.exe"
        } else {
            "mihomo-linux-amd64"
        });
        std::fs::write(&bin, b"stub").unwrap();
        // Should ignore our own tray binary name even if placed alongside.
        let self_bin = root.join(if cfg!(windows) {
            "mihomo-tray.exe"
        } else {
            "mihomo-tray"
        });
        std::fs::write(&self_bin, b"tray").unwrap();

        let found = discover_kernel(&root, None).expect("kernel should be discovered");
        assert_eq!(found, bin);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_adds_ext_ui_when_portable_ui_exists() {
        let root = temp_dir("ext_ui");
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(
            root.join("config").join("config.yaml"),
            "external-controller: 127.0.0.1:9090\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("ui")).unwrap();
        std::fs::write(root.join("ui").join("index.html"), "<html></html>").unwrap();

        let args = build_launch_args(&root, &TraySettings::default(), None).unwrap();
        let ui_idx = args
            .iter()
            .position(|a| a == "-ext-ui")
            .expect("-ext-ui present");
        assert_eq!(PathBuf::from(&args[ui_idx + 1]), root.join("ui"));

        let mut conflict_settings = TraySettings::default();
        conflict_settings.ui.external_ui = "./ui".to_string();
        conflict_settings.mihomo.args = vec!["-ext-ui".to_string(), "./other-ui".to_string()];
        assert!(build_launch_args(&root, &conflict_settings, None).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolves_declared_relative_kernel_path() {
        let root = temp_dir("declared_rel");
        std::fs::create_dir_all(root.join("custom")).unwrap();
        let custom_bin = root.join("custom").join("my-core.exe");
        std::fs::write(&custom_bin, b"core").unwrap();

        let mut settings = TraySettings::default();
        settings.mihomo.path = "./custom/my-core.exe".to_string();
        let found = find_kernel(&root, &settings).unwrap().unwrap();
        assert_eq!(found, custom_bin);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn controller_prefers_running_argv_over_config_and_fallback() {
        let root = temp_dir("ctl_precedence");
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("config.yaml");
        std::fs::write(
            &cfg_path,
            "external-controller: 127.0.0.1:9090\nsecret: from_cfg\n",
        )
        .unwrap();

        let mut settings = TraySettings::default();
        settings.controller.address = "127.0.0.1:9999".to_string();
        settings.controller.secret = "from_tray".to_string();

        // No running process -> uses config.yaml over tray.yml fallback.
        let client_cfg = resolve_controller(&settings, &[], &cfg_path).unwrap();
        assert_eq!(client_cfg.port, 9090);
        assert_eq!(client_cfg.secret, "from_cfg");

        // Running process with -ext-ctl/-secret overrides config.yaml.
        let running = vec![RunningProcess {
            pid: 42,
            exe: None,
            cmd: vec![
                "mihomo.exe".to_string(),
                "-f".to_string(),
                cfg_path.display().to_string(),
                "-ext-ctl".to_string(),
                "127.0.0.1:9095".to_string(),
                "-secret".to_string(),
                "from_argv".to_string(),
            ],
        }];
        let client_argv = resolve_controller(&settings, &running, &cfg_path).unwrap();
        assert_eq!(client_argv.port, 9095);
        assert_eq!(client_argv.secret, "from_argv");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovers_kernel_on_path_with_which() {
        let root = temp_dir("path_which_root");
        let path_dir = temp_dir("path_which_bin");
        let bin = path_dir.join(platform::kernel_exe_name());
        std::fs::write(&bin, b"stub").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
        }

        let found = discover_kernel(&root, Some(path_dir.as_os_str()))
            .expect("kernel should be found via PATH");
        assert_eq!(found, bin);

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&path_dir);
    }
}

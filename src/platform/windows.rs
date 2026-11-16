//! Windows 10/11 platform implementation.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Networking::WinInet::{
    INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetExitCodeProcess, INFINITE, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW, SW_HIDE,
};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const SCOOP_SHIM_MAX_BYTES: u64 = 64 * 1024;

pub fn portable_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn kernel_exe_name() -> &'static str {
    "mihomo.exe"
}

pub fn is_kernel_candidate_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.starts_with("mihomo") && lower.ends_with(".exe")
}

pub fn expand_env(value: &str) -> String {
    let source: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let needed = ExpandEnvironmentStringsW(source.as_ptr(), std::ptr::null_mut(), 0);
        if needed == 0 {
            return value.to_string();
        }
        let mut buf = vec![0u16; needed as usize];
        let written = ExpandEnvironmentStringsW(source.as_ptr(), buf.as_mut_ptr(), needed);
        if written == 0 || written > needed {
            return value.to_string();
        }
        String::from_utf16_lossy(&buf[..written as usize - 1])
    }
}

pub fn resolve_shim_target(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > SCOOP_SHIM_MAX_BYTES {
        return None;
    }
    let shim_file = path.with_extension("shim");
    let content = std::fs::read_to_string(shim_file).ok()?;
    let target = content
        .strip_prefix('\u{feff}')
        .unwrap_or(&content)
        .lines()
        .find_map(|line| {
            let trimmed = line.split('#').next()?.trim();
            let (k, v) = trimmed.split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case("path")
                .then(|| PathBuf::from(v.trim().trim_matches('"')))
        })?;
    target.is_file().then_some(target)
}

pub fn is_elevated() -> bool {
    deelevate::Token::with_current_process()
        .and_then(|token| token.privilege_level())
        .is_ok_and(|lvl| lvl != deelevate::PrivilegeLevel::NotPrivileged)
}

pub fn proxy_is_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(INTERNET_SETTINGS, KEY_READ)
        .and_then(|key| key.get_value::<u32, _>("ProxyEnable"))
        .map(|val| val == 1)
        .unwrap_or(false)
}

pub fn proxy_enable(port: u16, extra_bypass: &[String]) -> Result<(), String> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(INTERNET_SETTINGS, KEY_READ | KEY_SET_VALUE)
        .map_err(|e| format!("failed to open Internet Settings registry key: {e}"))?;

    let desired_server = format!("127.0.0.1:{port}");
    let current_enabled = key.get_value::<u32, _>("ProxyEnable").unwrap_or(0);
    let current_server = key
        .get_value::<String, _>("ProxyServer")
        .unwrap_or_default();

    let mut changed = false;
    if current_enabled != 1 {
        key.set_value("ProxyEnable", &1u32)
            .map_err(|e| format!("failed to set ProxyEnable: {e}"))?;
        changed = true;
    }
    if current_server != desired_server {
        key.set_value("ProxyServer", &desired_server)
            .map_err(|e| format!("failed to set ProxyServer: {e}"))?;
        changed = true;
    }

    // Seed ProxyOverride only when absent so user-curated bypass rules are preserved.
    let has_override = match key.get_raw_value("ProxyOverride") {
        Ok(_) => true,
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    };
    if !has_override {
        let mut entries: Vec<String> = extra_bypass
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        entries.push("<local>".to_string());
        key.set_value("ProxyOverride", &entries.join(";"))
            .map_err(|e| format!("failed to set ProxyOverride: {e}"))?;
        changed = true;
    }

    if changed {
        refresh_wininet();
    }
    Ok(())
}

pub fn proxy_disable() -> Result<(), String> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(INTERNET_SETTINGS, KEY_READ | KEY_SET_VALUE)
        .map_err(|e| format!("failed to open Internet Settings registry key: {e}"))?;

    if key.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0 {
        key.set_value("ProxyEnable", &0u32)
            .map_err(|e| format!("failed to clear ProxyEnable: {e}"))?;
        refresh_wininet();
    }
    Ok(())
}

fn refresh_wininet() {
    unsafe {
        InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_SETTINGS_CHANGED,
            std::ptr::null(),
            0,
        );
        InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_REFRESH,
            std::ptr::null(),
            0,
        );
    }
}

pub fn spawn_hidden(exe: &Path, args: &[String], cwd: Option<&Path>) -> std::io::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.spawn()
}

pub fn run_elevated_self(args: &[String]) -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("failed to locate self: {e}"))?;
    let params = crate::platform::quote_win32_args(args);
    let verb_w = wide("runas");
    let file_w = wide(&exe.to_string_lossy());
    let params_w = wide(&params);

    unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );

        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS;
        sei.lpVerb = verb_w.as_ptr();
        sei.lpFile = file_w.as_ptr();
        sei.lpParameters = params_w.as_ptr();
        sei.nShow = SW_HIDE;

        if ShellExecuteExW(&mut sei) == 0 || sei.hProcess.is_null() {
            return Err("UAC elevation was cancelled or denied".to_string());
        }

        WaitForSingleObject(sei.hProcess, INFINITE);
        let mut code: u32 = u32::MAX;
        let got = GetExitCodeProcess(sei.hProcess, &mut code);
        CloseHandle(sei.hProcess);
        if got == 0 {
            return Err("failed to read elevated helper exit code".to_string());
        }
        Ok(code as i32)
    }
}

pub fn notify_error(title: &str, message: &str) {
    let title_w = wide(title);
    let msg_w = wide(message);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            msg_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn confirm_yes_no(title: &str, message: &str) -> bool {
    let title_w = wide(title);
    let msg_w = wide(message);
    let res = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            msg_w.as_ptr(),
            title_w.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    res == IDYES
}

pub fn after_event_loop_ready() {}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

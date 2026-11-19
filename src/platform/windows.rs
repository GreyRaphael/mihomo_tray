//! Windows 10/11 platform implementation.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Networking::WinInet::{
    INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetCurrentThreadId, GetExitCodeProcess, INFINITE, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GUI_INMENUMODE, GUI_POPUPMENUMODE, GUITHREADINFO, GetClassNameW,
    GetGUIThreadInfo, HCBT_ACTIVATE, HHOOK, ICON_BIG, ICON_SMALL, IDI_INFORMATION, LoadIconW,
    SW_HIDE, SendMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_CBT, WM_SETICON,
};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const SCOOP_SHIM_MAX_BYTES: u64 = 64 * 1024;

pub fn kernel_exe_name() -> &'static str {
    "mihomo.exe"
}

pub fn is_kernel_candidate_name(lower_name: &str) -> bool {
    lower_name.starts_with("mihomo") && lower_name.ends_with(".exe")
}

pub fn expand_env(value: &str) -> String {
    let source = wide(value);
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
    let shim_file = path.with_extension("shim");
    let meta = std::fs::metadata(&shim_file).ok()?;
    if meta.len() > SCOOP_SHIM_MAX_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(shim_file).ok()?;
    let target = super::win_helpers::parse_shim_path(&content)?;
    target.is_file().then_some(target)
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

    let existing_override = match key.get_value::<String, _>("ProxyOverride") {
        Ok(val) => Some(val),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(format!("failed to read ProxyOverride: {err}"));
        }
    };
    if let Some(merged) =
        super::win_helpers::merge_proxy_override(existing_override.as_deref(), extra_bypass)
    {
        key.set_value("ProxyOverride", &merged)
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
    let params = super::win_helpers::quote_win32_args(args);
    let verb_w = wide("runas");
    let file_w = wide(&exe);
    let params_w = wide(&params);

    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    unsafe {
        let com_hr = CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let _com = ComGuard(com_hr >= 0);

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

pub fn format_env_vars_command(port: u16) -> String {
    format!(
        "$env:HTTP_PROXY=\"http://127.0.0.1:{port}\"; $env:HTTPS_PROXY=\"http://127.0.0.1:{port}\"; $env:ALL_PROXY=\"socks5://127.0.0.1:{port}\""
    )
}

pub fn is_tray_menu_open() -> bool {
    unsafe {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(GetCurrentThreadId(), &mut info) == 0 {
            return false;
        }
        (info.flags & (GUI_INMENUMODE | GUI_POPUPMENUMODE)) != 0
    }
}

pub fn can_show_dialog() -> bool {
    true
}

/// Attaches the Windows information icon to `rustydialogs::TextInput` (`#32770`)
/// while it runs on the calling thread.
pub fn with_dialog_icon<T>(f: impl FnOnce() -> T) -> T {
    struct HookGuard(HHOOK);
    impl Drop for HookGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { UnhookWindowsHookEx(self.0) };
            }
        }
    }

    let hook = unsafe {
        SetWindowsHookExW(
            WH_CBT,
            Some(dialog_icon_cbt_proc),
            std::ptr::null_mut(),
            GetCurrentThreadId(),
        )
    };
    let _guard = HookGuard(hook);
    f()
}

unsafe extern "system" fn dialog_icon_cbt_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code == HCBT_ACTIVATE as i32 {
        let hwnd = wparam as HWND;
        if !hwnd.is_null() && is_dialog_class(hwnd) {
            let icon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_INFORMATION) };
            if !icon.is_null() {
                unsafe {
                    SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, icon as isize);
                    SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, icon as isize);
                }
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn is_dialog_class(hwnd: HWND) -> bool {
    const DIALOG_CLASS: [u16; 6] = [
        b'#' as u16,
        b'3' as u16,
        b'2' as u16,
        b'7' as u16,
        b'7' as u16,
        b'0' as u16,
    ];
    let mut class_name = [0u16; 16];
    let len = unsafe { GetClassNameW(hwnd, class_name.as_mut_ptr(), class_name.len() as i32) };
    len == DIALOG_CLASS.len() as i32 && class_name[..DIALOG_CLASS.len()] == DIALOG_CLASS
}

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

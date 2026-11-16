//! Windows 10/11 platform implementation.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows_sys::Win32::Networking::WinInet::{
    INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetCurrentThreadId, GetExitCodeProcess, INFINITE, WaitForSingleObject,
};
use windows_sys::Win32::UI::Controls::EM_SETSEL;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOHSCROLL, GUI_INMENUMODE, GUI_POPUPMENUMODE, GUITHREADINFO,
    GWLP_USERDATA, GetGUIThreadInfo, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, IDCANCEL, IDOK, IDYES, IsDialogMessageW, MB_DEFBUTTON2,
    MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, MSG, MessageBoxW, PostQuitMessage,
    RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TranslateMessage, UnregisterClassW,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const SCOOP_SHIM_MAX_BYTES: u64 = 64 * 1024;
const EDIT_ID: usize = 101;
static PROMPT_OPEN: AtomicBool = AtomicBool::new(false);

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

struct PromptGuard;

impl Drop for PromptGuard {
    fn drop(&mut self) {
        PROMPT_OPEN.store(false, Ordering::Release);
    }
}

struct DialogState {
    edit: HWND,
    result: Option<String>,
}

pub fn prompt_subscription_url(current: Option<&str>) -> Option<String> {
    if PROMPT_OPEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    let _guard = PromptGuard;

    let class_name = wide("MihomoTraySubscriptionPrompt");
    let title_w = wide("Set Subscription URL");
    let label_w = wide("Enter subscription URL (auto-updates every 12 hours):");
    let initial_w = wide(current.unwrap_or(""));
    let save_w = wide("Save");
    let cancel_w = wide("Cancel");
    let static_cls = wide("STATIC");
    let edit_cls = wide("EDIT");
    let button_cls = wide("BUTTON");

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(prompt_wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name.as_ptr(),
            hbrBackground: (windows_sys::Win32::Graphics::Gdi::COLOR_BTNFACE + 1) as _,
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);

        let width = 520;
        let height = 160;
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let x = ((screen_w - width) / 2).max(0);
        let y = ((screen_h - height) / 2).max(0);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title_w.as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            UnregisterClassW(class_name.as_ptr(), hinstance);
            return None;
        }

        let font = GetStockObject(DEFAULT_GUI_FONT) as usize;

        let label = CreateWindowExW(
            0,
            static_cls.as_ptr(),
            label_w.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            16,
            14,
            472,
            20,
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(label, WM_SETFONT, font, 1);

        let edit = CreateWindowExW(
            0,
            edit_cls.as_ptr(),
            initial_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            16,
            40,
            472,
            26,
            hwnd,
            EDIT_ID as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(edit, WM_SETFONT, font, 1);

        let ok_btn = CreateWindowExW(
            0,
            button_cls.as_ptr(),
            save_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            306,
            82,
            86,
            28,
            hwnd,
            IDOK as usize as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(ok_btn, WM_SETFONT, font, 1);

        let cancel_btn = CreateWindowExW(
            0,
            button_cls.as_ptr(),
            cancel_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            402,
            82,
            86,
            28,
            hwnd,
            IDCANCEL as usize as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(cancel_btn, WM_SETFONT, font, 1);

        let mut state = Box::new(DialogState { edit, result: None });
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            (&mut *state as *mut DialogState) as isize,
        );

        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(edit);
        SendMessageW(edit, EM_SETSEL, 0, -1);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        UnregisterClassW(class_name.as_ptr(), hinstance);
        state
            .result
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

unsafe extern "system" fn prompt_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let cmd_id = (wparam & 0xFFFF) as i32;
            if cmd_id == IDOK {
                let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DialogState };
                if !ptr.is_null() {
                    let state = unsafe { &mut *ptr };
                    let len = unsafe { GetWindowTextLengthW(state.edit) };
                    if len > 0 {
                        let mut buf = vec![0u16; len as usize + 1];
                        let copied = unsafe {
                            GetWindowTextW(state.edit, buf.as_mut_ptr(), buf.len() as i32)
                        };
                        if copied > 0 {
                            state.result = Some(String::from_utf16_lossy(&buf[..copied as usize]));
                        }
                    }
                }
                unsafe { DestroyWindow(hwnd) };
                return 0;
            }
            if cmd_id == IDCANCEL {
                unsafe { DestroyWindow(hwnd) };
                return 0;
            }
        }
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            return 0;
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

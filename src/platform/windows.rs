//! Windows 10/11 platform implementation.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, GlobalFree, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontIndirectW, DEFAULT_GUI_FONT, DeleteObject, GetDC, GetMonitorInfoW,
    GetStockObject, GetSysColorBrush, GetTextMetricsW, HDC, HFONT, HGDIOBJ,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows_sys::Win32::Networking::WinInet::{
    INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetCurrentThreadId, GetExitCodeProcess, INFINITE, WaitForSingleObject,
};
use windows_sys::Win32::System::WindowsProgramming::MulDiv;
use windows_sys::Win32::UI::Controls::EM_SETMARGINS;
use windows_sys::Win32::UI::Controls::EM_SETSEL;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi, SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateIconFromResourceEx, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyWindow, DispatchMessageW, EC_LEFTMARGIN, EC_RIGHTMARGIN, ES_AUTOHSCROLL,
    ES_LEFT, GUI_INMENUMODE, GUI_POPUPMENUMODE, GUITHREADINFO, GWLP_USERDATA, GetCursorPos,
    GetGUIThreadInfo, GetMessageW, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HICON,
    ICON_BIG, ICON_SMALL, IDC_ARROW, IDCANCEL, IDOK, IDYES, IMAGE_ICON, IsDialogMessageW,
    LR_DEFAULTCOLOR, LR_SHARED, LoadCursorW, LoadImageW, MB_DEFBUTTON2, MB_ICONERROR,
    MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MSG, MessageBoxW, NONCLIENTMETRICSW,
    PostQuitMessage, RegisterClassExW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON,
    SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    UnregisterClassW, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_SETFONT, WM_SETICON,
    WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const SCOOP_SHIM_MAX_BYTES: u64 = 64 * 1024;
const EDIT_ID: usize = 101;
const EMBEDDED_ICO: &[u8] = include_bytes!("../../assets/app.ico");
static PROMPT_OPEN: AtomicBool = AtomicBool::new(false);
static PROMPT_HWND: AtomicIsize = AtomicIsize::new(0);

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

pub fn notify_info(title: &str, message: &str) {
    let title_w = wide(title);
    let msg_w = wide(message);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            msg_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
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

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let utf16 = wide(text);
    let byte_len = utf16.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("failed to open Windows clipboard".to_string());
        }
        EmptyClipboard();

        let hmem = GlobalAlloc(GMEM_MOVEABLE, byte_len);
        if hmem.is_null() {
            CloseClipboard();
            return Err("failed to allocate clipboard memory".to_string());
        }

        let ptr = GlobalLock(hmem) as *mut u16;
        if ptr.is_null() {
            GlobalFree(hmem);
            CloseClipboard();
            return Err("failed to lock clipboard memory".to_string());
        }
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
        GlobalUnlock(hmem);

        if SetClipboardData(CF_UNICODETEXT as u32, hmem).is_null() {
            GlobalFree(hmem);
            CloseClipboard();
            return Err("failed to set clipboard text".to_string());
        }
        CloseClipboard();
    }
    Ok(())
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

struct PromptGuard;

impl Drop for PromptGuard {
    fn drop(&mut self) {
        PROMPT_HWND.store(0, Ordering::Release);
        PROMPT_OPEN.store(false, Ordering::Release);
    }
}

struct DialogState {
    edit: HWND,
    hint_label: HWND,
    result: Option<String>,
}

struct OwnedIcon {
    handle: HICON,
    owned: bool,
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        if self.owned && !self.handle.is_null() {
            unsafe {
                DestroyIcon(self.handle);
            }
        }
    }
}

struct OwnedFont {
    handle: HFONT,
    owned: bool,
}

impl Drop for OwnedFont {
    fn drop(&mut self) {
        if self.owned && !self.handle.is_null() {
            unsafe {
                DeleteObject(self.handle as _);
            }
        }
    }
}

pub fn prompt_subscription_url(current: Option<&str>) -> Option<String> {
    if PROMPT_OPEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        let existing = PROMPT_HWND.load(Ordering::Acquire);
        if existing != 0 {
            unsafe {
                SetForegroundWindow(existing as HWND);
            }
        }
        return None;
    }
    let _guard = PromptGuard;

    let class_name = wide("MihomoTraySubscriptionPrompt");
    let title_w = wide("Set Subscription URL");
    let heading_w = wide("Subscription URL");
    let hint_w = wide("Enter your subscription link (auto-updates every 12 hours):");
    let initial_w = wide(current.unwrap_or(""));
    let save_w = wide("Save");
    let cancel_w = wide("Cancel");
    let static_cls = wide("STATIC");
    let edit_cls = wide("EDIT");
    let button_cls = wide("BUTTON");

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let init_big = load_app_icon(hinstance, 32, 32);
        let init_sm = load_app_icon(hinstance, 16, 16);

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(prompt_wnd_proc),
            hInstance: hinstance,
            hIcon: init_big.handle,
            hIconSm: init_sm.handle,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: class_name.as_ptr(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);

        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
        let mut cursor_pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor_pt);
        let work_area = monitor_work_area(cursor_pt);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title_w.as_ptr(),
            style,
            work_area.left,
            work_area.top,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            UnregisterClassW(class_name.as_ptr(), hinstance);
            return None;
        }
        PROMPT_HWND.store(hwnd as isize, Ordering::Release);

        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = |dip: i32| MulDiv(dip, dpi as i32, 96);

        let dpi_big = load_app_icon(
            hinstance,
            GetSystemMetricsForDpi(SM_CXICON, dpi).max(16),
            GetSystemMetricsForDpi(SM_CYICON, dpi).max(16),
        );
        let dpi_sm = load_app_icon(
            hinstance,
            GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16),
            GetSystemMetricsForDpi(SM_CYSMICON, dpi).max(16),
        );
        if !dpi_big.handle.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, dpi_big.handle as isize);
        }
        if !dpi_sm.handle.is_null() {
            SendMessageW(
                hwnd,
                WM_SETICON,
                ICON_SMALL as usize,
                dpi_sm.handle as isize,
            );
        }
        let _keep_icons = (init_big, init_sm, dpi_big, dpi_sm);

        let heading_font = create_message_font(dpi, 11, 600);
        let ui_font = create_message_font(dpi, 10, 400);
        let hint_font = create_message_font(dpi, 9, 400);

        let title_h = measure_font_height(hwnd, heading_font.handle).max(scale(20));
        let ui_h = measure_font_height(hwnd, ui_font.handle).max(scale(16));
        let hint_h = measure_font_height(hwnd, hint_font.handle).max(scale(15));

        let margin = scale(20);
        let client_w = scale(500);
        let content_w = client_w - margin * 2;
        let edit_h = ui_h + scale(8);
        let btn_w = scale(92);
        let btn_h = (ui_h + scale(12)).max(scale(30));
        let btn_gap = scale(10);

        let y_heading = margin;
        let y_hint = y_heading + title_h + scale(4);
        let y_edit = y_hint + hint_h + scale(12);
        let y_btns = y_edit + edit_h + scale(18);
        let client_h = y_btns + btn_h + margin;

        let mut win_rect = RECT {
            left: 0,
            top: 0,
            right: client_w,
            bottom: client_h,
        };
        AdjustWindowRectExForDpi(&mut win_rect, style, 0, 0, dpi);
        let win_w = win_rect.right - win_rect.left;
        let win_h = win_rect.bottom - win_rect.top;
        let work_w = work_area.right - work_area.left;
        let work_h = work_area.bottom - work_area.top;
        let win_x = work_area.left + ((work_w - win_w) / 2).max(0);
        let win_y = work_area.top + ((work_h - win_h) / 2).max(0);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            win_x,
            win_y,
            win_w,
            win_h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );

        let heading_label = CreateWindowExW(
            0,
            static_cls.as_ptr(),
            heading_w.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            margin,
            y_heading,
            content_w,
            title_h,
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(heading_label, WM_SETFONT, heading_font.handle as usize, 1);

        let hint_label = CreateWindowExW(
            0,
            static_cls.as_ptr(),
            hint_w.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            margin,
            y_hint,
            content_w,
            hint_h,
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(hint_label, WM_SETFONT, hint_font.handle as usize, 1);

        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            edit_cls.as_ptr(),
            initial_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_LEFT as u32 | ES_AUTOHSCROLL as u32,
            margin,
            y_edit,
            content_w,
            edit_h,
            hwnd,
            EDIT_ID as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(edit, WM_SETFONT, ui_font.handle as usize, 1);
        let h_pad = (scale(6).clamp(2, 24)) as usize;
        let margins_lparam = ((h_pad << 16) | h_pad) as isize;
        SendMessageW(
            edit,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
            margins_lparam,
        );

        let ok_x = client_w - margin - btn_w * 2 - btn_gap;
        let cancel_x = client_w - margin - btn_w;

        let ok_btn = CreateWindowExW(
            0,
            button_cls.as_ptr(),
            save_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            ok_x,
            y_btns,
            btn_w,
            btn_h,
            hwnd,
            IDOK as usize as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(ok_btn, WM_SETFONT, ui_font.handle as usize, 1);

        let cancel_btn = CreateWindowExW(
            0,
            button_cls.as_ptr(),
            cancel_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            cancel_x,
            y_btns,
            btn_w,
            btn_h,
            hwnd,
            IDCANCEL as usize as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(cancel_btn, WM_SETFONT, ui_font.handle as usize, 1);

        let mut state = Box::new(DialogState {
            edit,
            hint_label,
            result: None,
        });
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

        drop((heading_font, ui_font, hint_font));
        UnregisterClassW(class_name.as_ptr(), hinstance);
        state
            .result
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

unsafe fn monitor_work_area(pt: POINT) -> RECT {
    let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(hmon, &mut mi) } != 0 {
        mi.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        }
    }
}

unsafe fn load_app_icon(
    hinstance: windows_sys::Win32::Foundation::HINSTANCE,
    cx: i32,
    cy: i32,
) -> OwnedIcon {
    // Resource ID 1 matches `winresource::WindowsResource::set_icon("assets/app.ico")`.
    let handle = unsafe {
        LoadImageW(
            hinstance,
            std::ptr::without_provenance::<u16>(1),
            IMAGE_ICON,
            cx,
            cy,
            LR_SHARED,
        )
    } as HICON;
    if !handle.is_null() {
        return OwnedIcon {
            handle,
            owned: false,
        };
    }
    let fallback = unsafe { load_icon_from_ico_bytes(EMBEDDED_ICO, cx.max(cy)) };
    OwnedIcon {
        handle: fallback,
        owned: !fallback.is_null(),
    }
}

unsafe fn load_icon_from_ico_bytes(ico: &[u8], target_size: i32) -> HICON {
    if ico.len() < 6 {
        return std::ptr::null_mut();
    }
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let mut best: Option<(i32, usize, usize)> = None;
    for idx in 0..count {
        let base = 6 + idx * 16;
        if base + 16 > ico.len() {
            break;
        }
        let width = if ico[base] == 0 {
            256
        } else {
            ico[base] as i32
        };
        let bytes_in_res =
            u32::from_le_bytes([ico[base + 8], ico[base + 9], ico[base + 10], ico[base + 11]])
                as usize;
        let offset = u32::from_le_bytes([
            ico[base + 12],
            ico[base + 13],
            ico[base + 14],
            ico[base + 15],
        ]) as usize;
        if offset.saturating_add(bytes_in_res) > ico.len() || bytes_in_res == 0 {
            continue;
        }
        let diff = (width - target_size).abs();
        if best.is_none_or(|(best_diff, _, _)| diff < best_diff) {
            best = Some((diff, offset, bytes_in_res));
        }
    }
    let Some((_, offset, size)) = best else {
        return std::ptr::null_mut();
    };
    unsafe {
        CreateIconFromResourceEx(
            ico[offset..offset + size].as_ptr(),
            size as u32,
            1,
            0x0003_0000,
            target_size,
            target_size,
            LR_DEFAULTCOLOR,
        )
    }
}

unsafe fn create_message_font(dpi: u32, point_size: i32, weight: i32) -> OwnedFont {
    let mut ncm: NONCLIENTMETRICSW = unsafe { std::mem::zeroed() };
    ncm.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    if unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            (&mut ncm as *mut NONCLIENTMETRICSW).cast(),
            0,
            dpi,
        )
    } != 0
    {
        ncm.lfMessageFont.lfHeight = unsafe { -MulDiv(point_size, dpi as i32, 72) };
        ncm.lfMessageFont.lfWeight = weight;
        let font = unsafe { CreateFontIndirectW(&ncm.lfMessageFont) };
        if !font.is_null() {
            return OwnedFont {
                handle: font,
                owned: true,
            };
        }
    }
    OwnedFont {
        handle: unsafe { GetStockObject(DEFAULT_GUI_FONT) } as HFONT,
        owned: false,
    }
}

unsafe fn measure_font_height(hwnd: HWND, font: HFONT) -> i32 {
    let hdc: HDC = unsafe { GetDC(hwnd) };
    if hdc.is_null() {
        return 18;
    }
    let prev: HGDIOBJ = unsafe { SelectObject(hdc, font as HGDIOBJ) };
    let mut tm: TEXTMETRICW = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetTextMetricsW(hdc, &mut tm) };
    unsafe {
        SelectObject(hdc, prev);
        ReleaseDC(hwnd, hdc);
    }
    if ok != 0 && tm.tmHeight > 0 {
        tm.tmHeight
    } else {
        18
    }
}

unsafe extern "system" fn prompt_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as HDC;
            let ctrl = lparam as HWND;
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DialogState };
            if !ptr.is_null() && unsafe { (*ptr).hint_label } == ctrl {
                unsafe { SetTextColor(hdc, 0x006E_665F) };
            }
            unsafe {
                SetBkMode(hdc, TRANSPARENT as i32);
                return GetSysColorBrush(COLOR_WINDOW) as LRESULT;
            }
        }
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

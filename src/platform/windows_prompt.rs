//! DPI-aware Win32 modal input dialog for configuring the subscription URL.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontIndirectW, DEFAULT_GUI_FONT, DeleteObject, GetMonitorInfoW,
    GetStockObject, GetSysColorBrush, HDC, HFONT, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, SetBkMode, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::WindowsProgramming::MulDiv;
use windows_sys::Win32::UI::Controls::{EM_SETMARGINS, EM_SETSEL};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, EC_LEFTMARGIN, EC_RIGHTMARGIN, ES_AUTOHSCROLL, ES_LEFT, GWLP_USERDATA,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HICON,
    IDC_ARROW, IDCANCEL, IDOK, IMAGE_ICON, IsDialogMessageW, LR_DEFAULTSIZE, LR_SHARED,
    LoadCursorW, LoadImageW, MSG, NONCLIENTMETRICSW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SPI_GETNONCLIENTMETRICS, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, UnregisterClassW, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_SETFONT, WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};

use super::windows::wide;
use crate::text;

const EDIT_ID: usize = 101;
static PROMPT_OPEN: AtomicBool = AtomicBool::new(false);
static PROMPT_HWND: AtomicIsize = AtomicIsize::new(0);

struct PromptGuard;

impl Drop for PromptGuard {
    fn drop(&mut self) {
        PROMPT_HWND.store(0, Ordering::Release);
        PROMPT_OPEN.store(false, Ordering::Release);
    }
}

struct DialogState {
    edit: HWND,
    result: Option<String>,
}

struct OwnedFont {
    handle: HFONT,
    owned: bool,
}

impl Drop for OwnedFont {
    fn drop(&mut self) {
        if self.owned && !self.handle.is_null() {
            unsafe { DeleteObject(self.handle as _) };
        }
    }
}

fn focus_subscription_prompt() {
    let existing = PROMPT_HWND.load(Ordering::Acquire);
    if existing != 0 {
        unsafe { SetForegroundWindow(existing as HWND) };
    }
}

pub fn close_subscription_prompt() {
    let existing = PROMPT_HWND.load(Ordering::Acquire);
    if existing != 0 {
        unsafe { PostMessageW(existing as HWND, WM_CLOSE, 0, 0) };
    }
}

pub fn prompt_subscription_url(current: Option<&str>) -> Option<String> {
    if PROMPT_OPEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        focus_subscription_prompt();
        return None;
    }
    let _guard = PromptGuard;

    let class_name = wide("MihomoTraySubscriptionPrompt");
    let title_w = wide(text::PROMPT_SUBSCRIPTION_TITLE);
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let mut cursor_pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor_pt);
        let work_area = monitor_work_area(cursor_pt);

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(prompt_wnd_proc),
            hInstance: hinstance,
            // Resource ID 1 matches `winresource::WindowsResource::set_icon("assets/app.ico")`.
            hIcon: LoadImageW(
                hinstance,
                std::ptr::without_provenance::<u16>(1),
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED,
            ) as HICON,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: class_name.as_ptr(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);

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
        let font = create_ui_font(dpi, 10);

        let margin = scale(18);
        let client_w = scale(480);
        let content_w = client_w - margin * 2;
        let hint_h = scale(20);
        let edit_h = scale(26);
        let btn_w = scale(88);
        let btn_h = scale(30);
        let btn_gap = scale(10);

        let y_hint = margin;
        let y_edit = y_hint + hint_h + scale(6);
        let y_btns = y_edit + edit_h + scale(16);
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
        let win_x = work_area.left + ((work_area.right - work_area.left - win_w) / 2).max(0);
        let win_y = work_area.top + ((work_area.bottom - work_area.top - win_h) / 2).max(0);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            win_x,
            win_y,
            win_w,
            win_h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );

        create_ctrl(
            hinstance,
            hwnd,
            "STATIC",
            text::PROMPT_SUBSCRIPTION_HINT,
            0,
            0,
            [margin, y_hint, content_w, hint_h],
            0,
            font.handle,
        );
        let edit = create_ctrl(
            hinstance,
            hwnd,
            "EDIT",
            current.unwrap_or(""),
            WS_EX_CLIENTEDGE,
            WS_TABSTOP | ES_LEFT as u32 | ES_AUTOHSCROLL as u32,
            [margin, y_edit, content_w, edit_h],
            EDIT_ID,
            font.handle,
        );
        let h_pad = scale(6).clamp(2, 24) as usize;
        SendMessageW(
            edit,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
            ((h_pad << 16) | h_pad) as isize,
        );

        let ok_x = client_w - margin - btn_w * 2 - btn_gap;
        let cancel_x = client_w - margin - btn_w;
        create_ctrl(
            hinstance,
            hwnd,
            "BUTTON",
            text::PROMPT_BTN_SAVE,
            0,
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            [ok_x, y_btns, btn_w, btn_h],
            IDOK as usize,
            font.handle,
        );
        create_ctrl(
            hinstance,
            hwnd,
            "BUTTON",
            text::PROMPT_BTN_CANCEL,
            0,
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            [cancel_x, y_btns, btn_w, btn_h],
            IDCANCEL as usize,
            font.handle,
        );

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

        drop(font);
        UnregisterClassW(class_name.as_ptr(), hinstance);
        state
            .result
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_ctrl(
    hinstance: HINSTANCE,
    parent: HWND,
    class: &str,
    label: &str,
    ex_style: u32,
    style: u32,
    rect: [i32; 4],
    id: usize,
    font: HFONT,
) -> HWND {
    let class_w = wide(class);
    let label_w = wide(label);
    let ctrl = unsafe {
        CreateWindowExW(
            ex_style,
            class_w.as_ptr(),
            label_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            rect[0],
            rect[1],
            rect[2],
            rect[3],
            parent,
            id as _,
            hinstance,
            std::ptr::null(),
        )
    };
    unsafe { SendMessageW(ctrl, WM_SETFONT, font as usize, 1) };
    ctrl
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

unsafe fn create_ui_font(dpi: u32, point_size: i32) -> OwnedFont {
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

unsafe extern "system" fn prompt_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CTLCOLORSTATIC => {
            unsafe { SetBkMode(wparam as HDC, TRANSPARENT as i32) };
            unsafe { GetSysColorBrush(COLOR_WINDOW) as LRESULT }
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
                0
            } else if cmd_id == IDCANCEL {
                unsafe { DestroyWindow(hwnd) };
                0
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

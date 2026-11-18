//! DPI-aware Win32 modal input dialog for configuring the subscription URL.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontIndirectW, DEFAULT_GUI_FONT, DeleteObject, GetDC, GetMonitorInfoW,
    GetStockObject, GetSysColorBrush, GetTextMetricsW, HDC, HFONT, HGDIOBJ,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::WindowsProgramming::MulDiv;
use windows_sys::Win32::UI::Controls::{EM_SETMARGINS, EM_SETSEL};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi, SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, EC_LEFTMARGIN, EC_RIGHTMARGIN, ES_AUTOHSCROLL, ES_LEFT, GWLP_USERDATA,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HICON,
    ICON_BIG, ICON_SMALL, IDC_ARROW, IDCANCEL, IDOK, IMAGE_ICON, IsDialogMessageW, LR_SHARED,
    LoadCursorW, LoadImageW, MSG, NONCLIENTMETRICSW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SPI_GETNONCLIENTMETRICS,
    SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, UnregisterClassW, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_SETFONT, WM_SETICON, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
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
    hint_label: HWND,
    result: Option<String>,
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

struct PromptLayout {
    dpi: u32,
    client_w: i32,
    client_h: i32,
    margin: i32,
    content_w: i32,
    title_h: i32,
    hint_h: i32,
    edit_h: i32,
    btn_w: i32,
    btn_h: i32,
    btn_gap: i32,
    y_heading: i32,
    y_hint: i32,
    y_edit: i32,
    y_btns: i32,
}

impl PromptLayout {
    unsafe fn measure(hwnd: HWND, heading_font: HFONT, ui_font: HFONT, hint_font: HFONT) -> Self {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let scale = |dip: i32| unsafe { MulDiv(dip, dpi as i32, 96) };

        let title_h = unsafe { measure_font_height(hwnd, heading_font) }.max(scale(20));
        let ui_h = unsafe { measure_font_height(hwnd, ui_font) }.max(scale(16));
        let hint_h = unsafe { measure_font_height(hwnd, hint_font) }.max(scale(15));

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

        Self {
            dpi,
            client_w,
            client_h,
            margin,
            content_w,
            title_h,
            hint_h,
            edit_h,
            btn_w,
            btn_h,
            btn_gap,
            y_heading,
            y_hint,
            y_edit,
            y_btns,
        }
    }
}

pub fn focus_subscription_prompt() {
    let existing = PROMPT_HWND.load(Ordering::Acquire);
    if existing != 0 {
        unsafe {
            SetForegroundWindow(existing as HWND);
        }
    }
}

pub fn close_subscription_prompt() {
    let existing = PROMPT_HWND.load(Ordering::Acquire);
    if existing != 0 {
        unsafe {
            PostMessageW(existing as HWND, WM_CLOSE, 0, 0);
        }
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
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let mut cursor_pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor_pt);
        let work_area = monitor_work_area(cursor_pt);

        let hwnd = create_prompt_window(hinstance, &class_name, style, work_area)?;
        PROMPT_HWND.store(hwnd as isize, Ordering::Release);

        let dpi = GetDpiForWindow(hwnd).max(96);
        configure_window_icons(hinstance, hwnd, dpi);

        let heading_font = create_message_font(dpi, 11, 600);
        let ui_font = create_message_font(dpi, 10, 400);
        let hint_font = create_message_font(dpi, 9, 400);

        let layout =
            PromptLayout::measure(hwnd, heading_font.handle, ui_font.handle, hint_font.handle);
        center_window_on_work_area(hwnd, style, &layout, work_area);

        let (edit, hint_label) = create_prompt_controls(
            hinstance,
            hwnd,
            &layout,
            current.unwrap_or(""),
            heading_font.handle,
            ui_font.handle,
            hint_font.handle,
        );

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

        run_dialog_message_loop(hwnd);

        drop((heading_font, ui_font, hint_font));
        UnregisterClassW(class_name.as_ptr(), hinstance);
        state
            .result
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

unsafe fn create_prompt_window(
    hinstance: HINSTANCE,
    class_name: &[u16],
    style: u32,
    work_area: RECT,
) -> Option<HWND> {
    let title_w = wide(text::PROMPT_SUBSCRIPTION_TITLE);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(prompt_wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        lpszClassName: class_name.as_ptr(),
        hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
        ..unsafe { std::mem::zeroed() }
    };
    unsafe { RegisterClassExW(&wc) };

    let hwnd = unsafe {
        CreateWindowExW(
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
        )
    };
    if hwnd.is_null() {
        unsafe { UnregisterClassW(class_name.as_ptr(), hinstance) };
        None
    } else {
        Some(hwnd)
    }
}

unsafe fn configure_window_icons(hinstance: HINSTANCE, hwnd: HWND, dpi: u32) {
    let big = unsafe {
        load_app_icon(
            hinstance,
            GetSystemMetricsForDpi(SM_CXICON, dpi).max(16),
            GetSystemMetricsForDpi(SM_CYICON, dpi).max(16),
        )
    };
    let sm = unsafe {
        load_app_icon(
            hinstance,
            GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16),
            GetSystemMetricsForDpi(SM_CYSMICON, dpi).max(16),
        )
    };
    if !big.is_null() {
        unsafe { SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big as isize) };
    }
    if !sm.is_null() {
        unsafe { SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, sm as isize) };
    }
}

unsafe fn center_window_on_work_area(
    hwnd: HWND,
    style: u32,
    layout: &PromptLayout,
    work_area: RECT,
) {
    let mut win_rect = RECT {
        left: 0,
        top: 0,
        right: layout.client_w,
        bottom: layout.client_h,
    };
    unsafe { AdjustWindowRectExForDpi(&mut win_rect, style, 0, 0, layout.dpi) };
    let win_w = win_rect.right - win_rect.left;
    let win_h = win_rect.bottom - win_rect.top;
    let work_w = work_area.right - work_area.left;
    let work_h = work_area.bottom - work_area.top;
    let win_x = work_area.left + ((work_w - win_w) / 2).max(0);
    let win_y = work_area.top + ((work_h - win_h) / 2).max(0);
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            win_x,
            win_y,
            win_w,
            win_h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

unsafe fn create_prompt_controls(
    hinstance: HINSTANCE,
    hwnd: HWND,
    layout: &PromptLayout,
    initial_text: &str,
    heading_font: HFONT,
    ui_font: HFONT,
    hint_font: HFONT,
) -> (HWND, HWND) {
    let static_cls = wide("STATIC");
    let edit_cls = wide("EDIT");
    let button_cls = wide("BUTTON");

    let heading_w = wide(text::PROMPT_SUBSCRIPTION_HEADING);
    let hint_w = wide(text::PROMPT_SUBSCRIPTION_HINT);
    let initial_w = wide(initial_text);
    let save_w = wide(text::PROMPT_BTN_SAVE);
    let cancel_w = wide(text::PROMPT_BTN_CANCEL);

    unsafe {
        let heading_label = CreateWindowExW(
            0,
            static_cls.as_ptr(),
            heading_w.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            layout.margin,
            layout.y_heading,
            layout.content_w,
            layout.title_h,
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(heading_label, WM_SETFONT, heading_font as usize, 1);

        let hint_label = CreateWindowExW(
            0,
            static_cls.as_ptr(),
            hint_w.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            layout.margin,
            layout.y_hint,
            layout.content_w,
            layout.hint_h,
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(hint_label, WM_SETFONT, hint_font as usize, 1);

        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            edit_cls.as_ptr(),
            initial_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_LEFT as u32 | ES_AUTOHSCROLL as u32,
            layout.margin,
            layout.y_edit,
            layout.content_w,
            layout.edit_h,
            hwnd,
            EDIT_ID as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(edit, WM_SETFONT, ui_font as usize, 1);
        let h_pad = MulDiv(6, layout.dpi as i32, 96).clamp(2, 24) as usize;
        let margins_lparam = ((h_pad << 16) | h_pad) as isize;
        SendMessageW(
            edit,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
            margins_lparam,
        );

        let ok_x = layout.client_w - layout.margin - layout.btn_w * 2 - layout.btn_gap;
        let cancel_x = layout.client_w - layout.margin - layout.btn_w;

        let ok_btn = CreateWindowExW(
            0,
            button_cls.as_ptr(),
            save_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            ok_x,
            layout.y_btns,
            layout.btn_w,
            layout.btn_h,
            hwnd,
            IDOK as usize as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(ok_btn, WM_SETFONT, ui_font as usize, 1);

        let cancel_btn = CreateWindowExW(
            0,
            button_cls.as_ptr(),
            cancel_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            cancel_x,
            layout.y_btns,
            layout.btn_w,
            layout.btn_h,
            hwnd,
            IDCANCEL as usize as _,
            hinstance,
            std::ptr::null(),
        );
        SendMessageW(cancel_btn, WM_SETFONT, ui_font as usize, 1);

        (edit, hint_label)
    }
}

unsafe fn run_dialog_message_loop(hwnd: HWND) {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        if unsafe { IsDialogMessageW(hwnd, &msg) } == 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
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

unsafe fn load_app_icon(hinstance: HINSTANCE, cx: i32, cy: i32) -> HICON {
    // Resource ID 1 matches `winresource::WindowsResource::set_icon("assets/app.ico")`.
    unsafe {
        LoadImageW(
            hinstance,
            std::ptr::without_provenance::<u16>(1),
            IMAGE_ICON,
            cx,
            cy,
            LR_SHARED,
        ) as HICON
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

//! Lightweight native Win32 dialogs (`MessageBoxW` and in-memory `DLGTEMPLATE`).

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, DLGTEMPLATE, DS_CENTER, DS_SETFONT, DialogBoxIndirectParamW,
    ES_AUTOHSCROLL, ES_LEFT, EndDialog, GWLP_USERDATA, GetDlgItem, GetDlgItemTextW,
    GetWindowLongPtrW, GetWindowTextLengthW, ICON_BIG, ICON_SMALL, IDCANCEL, IDI_INFORMATION, IDOK,
    IDYES, LoadIconW, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND,
    MB_YESNO, MessageBoxW, SendMessageW, SetDlgItemTextW, SetWindowLongPtrW, WM_COMMAND,
    WM_INITDIALOG, WM_SETICON, WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};

const EDIT_ID_BASE: u16 = 100;
const LABEL_ID_BASE: u16 = 200;

const CLASS_BUTTON: u16 = 0x0080;
const CLASS_EDIT: u16 = 0x0081;
const CLASS_STATIC: u16 = 0x0082;

pub fn notify_error(title: &str, message: &str) {
    show_message_box(title, message, MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
}

pub fn notify_info(title: &str, message: &str) {
    show_message_box(
        title,
        message,
        MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
    );
}

pub fn confirm_yes_no(title: &str, message: &str) -> bool {
    show_message_box(title, message, MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND) == IDYES
}

fn show_message_box(title: &str, message: &str, flags: u32) -> i32 {
    let title_w = wide(title);
    let msg_w = wide(message);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            msg_w.as_ptr(),
            title_w.as_ptr(),
            flags,
        )
    }
}

/// Shows a centered native modal dialog containing one or more labeled single-line
/// text fields (`(label, initial_value)`). Returns all field values on OK.
pub fn prompt_inputs(title: &str, fields: &[(&str, &str)]) -> Option<Vec<String>> {
    if fields.is_empty() {
        return None;
    }

    let template = build_input_template(title, fields);
    let mut state = PromptState {
        initials: fields.iter().map(|(_, init)| wide(init)).collect(),
        result: None,
    };

    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let res = unsafe {
        DialogBoxIndirectParamW(
            hinstance,
            template.as_ptr().cast::<DLGTEMPLATE>(),
            std::ptr::null_mut(),
            Some(prompt_dialog_proc),
            (&mut state as *mut PromptState) as LPARAM,
        )
    };

    if res == IDOK as isize {
        state.result
    } else {
        None
    }
}

struct PromptState {
    initials: Vec<Vec<u16>>,
    result: Option<Vec<String>>,
}

unsafe extern "system" fn prompt_dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
                let icon = LoadIconW(std::ptr::null_mut(), IDI_INFORMATION);
                if !icon.is_null() {
                    SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, icon as isize);
                    SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, icon as isize);
                }
                let state = lparam as *const PromptState;
                if !state.is_null() {
                    for (idx, initial) in (*state).initials.iter().enumerate() {
                        if initial.len() > 1 {
                            SetDlgItemTextW(
                                hwnd,
                                (EDIT_ID_BASE + idx as u16) as i32,
                                initial.as_ptr(),
                            );
                        }
                    }
                }
            }
            1
        }
        WM_COMMAND => {
            let command_id = (wparam & 0xFFFF) as i32;
            if command_id == IDOK {
                unsafe {
                    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState;
                    if !state.is_null() {
                        let count = (*state).initials.len();
                        let mut values = Vec::with_capacity(count);
                        for idx in 0..count {
                            values
                                .push(read_dlg_item_text(hwnd, (EDIT_ID_BASE + idx as u16) as i32));
                        }
                        (*state).result = Some(values);
                    }
                    EndDialog(hwnd, IDOK as isize);
                }
                return 1;
            }
            if command_id == IDCANCEL {
                unsafe { EndDialog(hwnd, IDCANCEL as isize) };
                return 1;
            }
            0
        }
        _ => 0,
    }
}

unsafe fn read_dlg_item_text(hwnd: HWND, item_id: i32) -> String {
    let edit = unsafe { GetDlgItem(hwnd, item_id) };
    if edit.is_null() {
        return String::new();
    }
    let len = unsafe { GetWindowTextLengthW(edit) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let copied = unsafe { GetDlgItemTextW(hwnd, item_id, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..copied as usize])
}

/// Builds a 4-byte-aligned Win32 `DLGTEMPLATE` using Dialog Units (DLU) so Windows
/// scales the dialog cleanly across DPI settings without custom layout math.
fn build_input_template(title: &str, fields: &[(&str, &str)]) -> Vec<u32> {
    const DLG_W: i16 = 260;
    const MARGIN: i16 = 10;
    const FIELD_W: i16 = DLG_W - MARGIN * 2;
    const LABEL_H: i16 = 10;
    const EDIT_H: i16 = 14;
    const LABEL_GAP: i16 = 2;
    const FIELD_STEP: i16 = LABEL_H + LABEL_GAP + EDIT_H + 8;
    const BTN_W: i16 = 50;
    const BTN_H: i16 = 14;
    const BTN_GAP: i16 = 6;

    let count = fields.len() as i16;
    let btn_y = MARGIN + count * FIELD_STEP + 2;
    let dlg_h = btn_y + BTN_H + MARGIN;

    let dialog_style =
        WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE | DS_CENTER as u32 | DS_SETFONT as u32;
    let label_style = WS_CHILD | WS_VISIBLE;
    let edit_style =
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_LEFT as u32 | ES_AUTOHSCROLL as u32;
    let ok_style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32;
    let cancel_style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32;

    let mut tpl = TemplateBuilder::new();
    tpl.push_u32(dialog_style);
    tpl.push_u32(0); // dwExtendedStyle
    tpl.push_u16((fields.len() * 2 + 2) as u16); // cdit
    tpl.push_i16(0); // x (ignored with DS_CENTER)
    tpl.push_i16(0); // y
    tpl.push_i16(DLG_W);
    tpl.push_i16(dlg_h);
    tpl.push_u16(0); // menu
    tpl.push_u16(0); // windowClass (#32770)
    tpl.push_str(title);
    tpl.push_u16(9); // font size (DS_SETFONT)
    tpl.push_str("MS Shell Dlg 2");

    for (idx, (label, _)) in fields.iter().enumerate() {
        let y = MARGIN + (idx as i16) * FIELD_STEP;
        tpl.push_item(
            label_style,
            MARGIN,
            y,
            FIELD_W,
            LABEL_H,
            LABEL_ID_BASE + idx as u16,
            CLASS_STATIC,
            label,
        );
        tpl.push_item(
            edit_style,
            MARGIN,
            y + LABEL_H + LABEL_GAP,
            FIELD_W,
            EDIT_H,
            EDIT_ID_BASE + idx as u16,
            CLASS_EDIT,
            "",
        );
    }

    tpl.push_item(
        ok_style,
        DLG_W - MARGIN - BTN_W * 2 - BTN_GAP,
        btn_y,
        BTN_W,
        BTN_H,
        IDOK as u16,
        CLASS_BUTTON,
        "OK",
    );
    tpl.push_item(
        cancel_style,
        DLG_W - MARGIN - BTN_W,
        btn_y,
        BTN_W,
        BTN_H,
        IDCANCEL as u16,
        CLASS_BUTTON,
        "Cancel",
    );

    tpl.finish()
}

struct TemplateBuilder {
    words: Vec<u16>,
}

impl TemplateBuilder {
    fn new() -> Self {
        Self {
            words: Vec::with_capacity(256),
        }
    }

    fn align_dword(&mut self) {
        if !self.words.len().is_multiple_of(2) {
            self.words.push(0);
        }
    }

    fn push_u16(&mut self, value: u16) {
        self.words.push(value);
    }

    fn push_i16(&mut self, value: i16) {
        self.words.push(value as u16);
    }

    fn push_u32(&mut self, value: u32) {
        self.words.push((value & 0xFFFF) as u16);
        self.words.push((value >> 16) as u16);
    }

    fn push_str(&mut self, s: &str) {
        self.words.extend(s.encode_utf16());
        self.words.push(0);
    }

    #[allow(clippy::too_many_arguments)]
    fn push_item(
        &mut self,
        style: u32,
        x: i16,
        y: i16,
        cx: i16,
        cy: i16,
        id: u16,
        class_ordinal: u16,
        caption: &str,
    ) {
        self.align_dword();
        self.push_u32(style);
        self.push_u32(0); // dwExtendedStyle
        self.push_i16(x);
        self.push_i16(y);
        self.push_i16(cx);
        self.push_i16(cy);
        self.push_u16(id);
        self.push_u16(0xFFFF);
        self.push_u16(class_ordinal);
        self.push_str(caption);
        self.push_u16(0); // extra creation data
    }

    fn finish(self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.words.len().div_ceil(2));
        for pair in self.words.chunks(2) {
            let lo = pair[0] as u32;
            let hi = pair.get(1).copied().unwrap_or(0) as u32;
            out.push(lo | (hi << 16));
        }
        out
    }
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

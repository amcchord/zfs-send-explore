//! Modal dialogs: native task dialogs for choices, results and problems, and
//! a small form dialog for keys and paths.

use super::{APP_TITLE, Fonts, button, control, wide};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    FreeLibrary, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, GetStockObject, HBRUSH, SetBkColor, SetTextColor, UpdateWindow, WHITE_BRUSH,
};
use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
    ACTCTXW, ActivateActCtx, CreateActCtxW, DeactivateActCtx, ReleaseActCtx,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::WindowsProgramming::{
    ACTCTX_FLAG_HMODULE_VALID, ACTCTX_FLAG_RESOURCE_NAME_VALID,
};
use windows_sys::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_INFORMATION_ICON, TD_WARNING_ICON,
    TDCBF_CANCEL_BUTTON, TDCBF_CLOSE_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_POSITION_RELATIVE_TO_WINDOW, TDF_SIZE_TO_CONTENT, TDF_USE_COMMAND_LINKS,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EN_CHANGE,
    ES_AUTOHSCROLL, ES_PASSWORD, GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IsDialogMessageW, LoadCursorW,
    MB_ICONERROR, MB_OK, MSG, MessageBoxW, MoveWindow, PostQuitMessage, RegisterClassW, SW_SHOW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_NCCREATE, WM_SETFONT,
    WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CONTROLPARENT,
    WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub(super) enum DialogIcon {
    Information,
    Warning,
}

/// A native task dialog: a headline, an explanation, command-link choices and
/// optional collapsed technical details.
pub(super) struct Task<'a> {
    pub icon: DialogIcon,
    pub instruction: &'a str,
    pub content: &'a str,
    pub actions: &'a [(&'a str, &'a str)],
    pub details: Option<&'a str>,
    pub footer: Option<&'a str>,
    /// "Close" instead of "Cancel" for dialogs that report a finished step.
    pub close_label: bool,
}

impl<'a> Task<'a> {
    pub fn new(icon: DialogIcon, instruction: &'a str, content: &'a str) -> Self {
        Self {
            icon,
            instruction,
            content,
            actions: &[],
            details: None,
            footer: None,
            close_label: false,
        }
    }
}

/// Show a task dialog. Returns the chosen action index, or `None` when the
/// user cancels or closes it.
pub(super) unsafe fn task_dialog(owner: HWND, task: &Task) -> Option<usize> {
    let title = wide(APP_TITLE);
    let instruction = wide(task.instruction);
    let content = wide(task.content);
    let details = task.details.map(wide);
    let footer = task.footer.map(wide);
    let expand = wide("Show details");
    let collapse = wide("Hide details");
    let labels = task
        .actions
        .iter()
        .map(|(label, note)| {
            wide(&if note.is_empty() {
                (*label).to_owned()
            } else {
                format!("{label}\n{note}")
            })
        })
        .collect::<Vec<_>>();
    let buttons = labels
        .iter()
        .enumerate()
        .map(|(index, label)| TASKDIALOG_BUTTON {
            nButtonID: 1000 + index as i32,
            pszButtonText: label.as_ptr(),
        })
        .collect::<Vec<_>>();
    let icon = match task.icon {
        DialogIcon::Information => TD_INFORMATION_ICON,
        DialogIcon::Warning => TD_WARNING_ICON,
    };
    let mut flags =
        TDF_ALLOW_DIALOG_CANCELLATION | TDF_SIZE_TO_CONTENT | TDF_POSITION_RELATIVE_TO_WINDOW;
    if !buttons.is_empty() {
        flags |= TDF_USE_COMMAND_LINKS;
    }
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: flags,
        dwCommonButtons: if task.close_label {
            TDCBF_CLOSE_BUTTON
        } else {
            TDCBF_CANCEL_BUTTON
        },
        pszWindowTitle: title.as_ptr(),
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: icon },
        pszMainInstruction: instruction.as_ptr(),
        pszContent: content.as_ptr(),
        cButtons: buttons.len() as u32,
        pButtons: if buttons.is_empty() {
            null()
        } else {
            buttons.as_ptr()
        },
        nDefaultButton: if buttons.is_empty() { 0 } else { 1000 },
        pszExpandedInformation: details.as_ref().map_or(null(), |text| text.as_ptr()),
        pszExpandedControlText: collapse.as_ptr(),
        pszCollapsedControlText: expand.as_ptr(),
        pszFooter: footer.as_ref().map_or(null(), |text| text.as_ptr()),
        ..TASKDIALOGCONFIG::default()
    };
    let mut selected = 0_i32;
    match call_task_dialog(&config, &mut selected) {
        Some(result) if result >= 0 => (selected >= 1000
            && selected < 1000 + task.actions.len() as i32)
            .then_some((selected - 1000) as usize),
        _ => {
            // Without Common Controls 6 there is no task dialog; show the text.
            let text = wide(&format!("{}\n\n{}", task.instruction, task.content));
            MessageBoxW(owner, text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
            None
        }
    }
}

/// TaskDialogIndirect exists only in Common Controls 6. Activate the
/// executable's manifest on this thread before loading it, so the call also
/// works if another import loaded the legacy library first.
unsafe fn call_task_dialog(config: &TASKDIALOGCONFIG, selected: &mut i32) -> Option<i32> {
    let instance = GetModuleHandleW(null());
    let activation_config = ACTCTXW {
        cbSize: size_of::<ACTCTXW>() as u32,
        dwFlags: ACTCTX_FLAG_HMODULE_VALID | ACTCTX_FLAG_RESOURCE_NAME_VALID,
        lpResourceName: std::ptr::without_provenance::<u16>(1),
        hModule: instance,
        ..ACTCTXW::default()
    };
    let activation_context = CreateActCtxW(&activation_config);
    let mut cookie = 0_usize;
    let active = activation_context != INVALID_HANDLE_VALUE
        && ActivateActCtx(activation_context, &mut cookie) != 0;
    let library = LoadLibraryW(windows_sys::core::w!("comctl32.dll"));
    let function = (!library.is_null())
        .then(|| GetProcAddress(library, c"TaskDialogIndirect".as_ptr().cast()))
        .flatten();
    let result = function.map(|function| {
        type TaskDialogIndirectFn =
            unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut i32) -> i32;
        let function: TaskDialogIndirectFn = std::mem::transmute(function);
        function(config, selected, null_mut(), null_mut())
    });
    if !library.is_null() {
        FreeLibrary(library);
    }
    if active {
        DeactivateActCtx(0, cookie);
    }
    if activation_context != INVALID_HANDLE_VALUE {
        ReleaseActCtx(activation_context);
    }
    result
}

/// Explain a failure in plain words, keeping the engine text under "Show details".
pub(super) unsafe fn show_problem(owner: HWND, raw: &str) {
    let friendly = zfs_send_extract::presentation::friendly_error(raw);
    let mut task = Task::new(DialogIcon::Warning, &friendly.title, &friendly.message);
    task.details = Some(&friendly.details);
    task.close_label = true;
    task_dialog(owner, &task);
}

pub(super) unsafe fn show_message(owner: HWND, icon: DialogIcon, title: &str, message: &str) {
    let mut task = Task::new(icon, title, message);
    task.close_label = true;
    task_dialog(owner, &task);
}

/// What the user did in a form dialog.
pub(super) enum FormResult {
    /// The field's UTF-16 contents; zeroized when dropped.
    Submitted(Zeroizing<Vec<u16>>),
    Secondary,
    Cancelled,
}

pub(super) struct Form<'a> {
    pub heading: &'a str,
    pub body: &'a str,
    pub field: &'a str,
    pub initial: &'a str,
    pub password: bool,
    pub primary: &'a str,
    pub secondary: Option<&'a str>,
}

const FORM_EDIT: u16 = 200;
const FORM_SECONDARY: u16 = 3;
const IDOK: u16 = 1;
const IDCANCEL: u16 = 2;

struct FormState {
    edit: HWND,
    ok: HWND,
    result: Option<FormResult>,
    finished: bool,
}

unsafe extern "system" fn form_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = lparam as *const CREATESTRUCTW;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*create).lpCreateParams as isize);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FormState;
    match message {
        WM_COMMAND if !state.is_null() => {
            let id = (wparam & 0xffff) as u16;
            let notification = ((wparam >> 16) & 0xffff) as u32;
            match id {
                FORM_EDIT if notification == EN_CHANGE => {
                    EnableWindow(
                        (*state).ok,
                        (GetWindowTextLengthW((*state).edit) > 0) as i32,
                    );
                }
                IDOK => {
                    let length = GetWindowTextLengthW((*state).edit);
                    if length > 0 {
                        let mut text = Zeroizing::new(vec![0_u16; length as usize + 1]);
                        GetWindowTextW((*state).edit, text.as_mut_ptr(), text.len() as i32);
                        text.truncate(length as usize);
                        // Clear the control so the value does not outlive the dialog in it.
                        SetWindowTextW((*state).edit, wide("").as_ptr());
                        (*state).result = Some(FormResult::Submitted(text));
                        DestroyWindow(hwnd);
                    }
                }
                FORM_SECONDARY => {
                    SetWindowTextW((*state).edit, wide("").as_ptr());
                    (*state).result = Some(FormResult::Secondary);
                    DestroyWindow(hwnd);
                }
                IDCANCEL => {
                    SetWindowTextW((*state).edit, wide("").as_ptr());
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as windows_sys::Win32::Graphics::Gdi::HDC;
            SetTextColor(hdc, super::TEXT_COLOR);
            SetBkColor(hdc, 0x00ff_ffff);
            GetStockObject(WHITE_BRUSH) as LRESULT
        }
        WM_CLOSE => {
            if !state.is_null() {
                SetWindowTextW((*state).edit, wide("").as_ptr());
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            if !state.is_null() {
                (*state).finished = true;
            }
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// A compact modal form with one text field. Password fields are masked and
/// cleared before the dialog closes.
pub(super) unsafe fn form_dialog(owner: HWND, form: &Form) -> FormResult {
    let instance = GetModuleHandleW(null());
    let class_name = wide("ZfsExploreFormDialog");
    let class = WNDCLASSW {
        lpfnWndProc: Some(form_proc),
        hInstance: instance,
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_WINDOW as usize + 1) as HBRUSH,
        lpszClassName: class_name.as_ptr(),
        ..std::mem::zeroed()
    };
    RegisterClassW(&class);
    let dpi = GetDpiForWindow(owner).max(96) as i32;
    let scale = |value: i32| value * dpi / 96;
    let fonts = Fonts::create(dpi);
    let width = scale(540);
    let height = scale(268);
    let mut owner_rect = RECT::default();
    GetWindowRect(owner, &mut owner_rect);
    let x = owner_rect.left + ((owner_rect.right - owner_rect.left - width) / 2).max(0);
    let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top - height) / 3).max(0);
    let mut state = FormState {
        edit: null_mut(),
        ok: null_mut(),
        result: None,
        finished: false,
    };
    let title = wide(APP_TITLE);
    let dialog = CreateWindowExW(
        WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME,
        class_name.as_ptr(),
        title.as_ptr(),
        WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
        x,
        y,
        width,
        height,
        owner,
        null_mut(),
        instance,
        (&mut state as *mut FormState).cast::<c_void>(),
    );
    if dialog.is_null() {
        let mut fonts = fonts;
        fonts.destroy();
        return FormResult::Cancelled;
    }
    // Grow the window so the client area fits the content at this DPI.
    let mut client = RECT::default();
    GetClientRect(dialog, &mut client);
    let needed = scale(252);
    MoveWindow(dialog, x, y, width, height + needed - client.bottom, 0);
    GetClientRect(dialog, &mut client);
    let inner = client.right - scale(48);
    let pad = scale(24);
    let static_class = windows_sys::core::w!("STATIC");
    let heading = control(
        dialog,
        instance,
        static_class,
        form.heading,
        WS_CHILD | WS_VISIBLE,
        0,
        0,
    );
    MoveWindow(heading, pad, scale(18), inner, scale(30), 1);
    SendMessageW(heading, WM_SETFONT, fonts.title as usize, 1);
    let body = control(
        dialog,
        instance,
        static_class,
        form.body,
        WS_CHILD | WS_VISIBLE,
        0,
        0,
    );
    MoveWindow(body, pad, scale(54), inner, scale(64), 1);
    SendMessageW(body, WM_SETFONT, fonts.body as usize, 1);
    let label = control(
        dialog,
        instance,
        static_class,
        form.field,
        WS_CHILD | WS_VISIBLE,
        0,
        0,
    );
    MoveWindow(label, pad, scale(124), inner, scale(20), 1);
    SendMessageW(label, WM_SETFONT, fonts.strong as usize, 1);
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32;
    if form.password {
        style |= ES_PASSWORD as u32;
    }
    state.edit = control(
        dialog,
        instance,
        windows_sys::core::w!("EDIT"),
        form.initial,
        style,
        0,
        FORM_EDIT,
    );
    MoveWindow(state.edit, pad, scale(146), inner, scale(30), 1);
    SendMessageW(state.edit, WM_SETFONT, fonts.body as usize, 1);
    let buttons_y = scale(198);
    state.ok = button(dialog, instance, form.primary, IDOK, true);
    let cancel = button(dialog, instance, "Cancel", IDCANCEL, false);
    MoveWindow(
        cancel,
        client.right - pad - scale(100),
        buttons_y,
        scale(100),
        scale(32),
        1,
    );
    MoveWindow(
        state.ok,
        client.right - pad - scale(208),
        buttons_y,
        scale(100),
        scale(32),
        1,
    );
    for control in [state.ok, cancel] {
        SendMessageW(control, WM_SETFONT, fonts.body as usize, 1);
    }
    if let Some(secondary) = form.secondary {
        let other = button(dialog, instance, secondary, FORM_SECONDARY, false);
        MoveWindow(other, pad, buttons_y, scale(170), scale(32), 1);
        SendMessageW(other, WM_SETFONT, fonts.body as usize, 1);
    }
    EnableWindow(state.ok, (!form.initial.is_empty()) as i32);
    EnableWindow(owner, 0);
    ShowWindow(dialog, SW_SHOW);
    UpdateWindow(dialog);
    SetFocus(state.edit);
    let mut message = MSG::default();
    // `finished` is set by form_proc through the pointer given to CreateWindowExW.
    loop {
        if state.finished {
            break;
        }
        let result = GetMessageW(&mut message, null_mut(), 0, 0);
        if result <= 0 {
            if result == 0 {
                PostQuitMessage(message.wParam as i32);
            }
            break;
        }
        if IsDialogMessageW(dialog, &message) == 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    EnableWindow(owner, 1);
    SetForegroundWindow(owner);
    let mut fonts = fonts;
    fonts.destroy();
    state.result.take().unwrap_or(FormResult::Cancelled)
}

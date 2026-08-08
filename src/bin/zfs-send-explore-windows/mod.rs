#![allow(unsafe_op_in_unsafe_fn)]

use anyhow::{Context, Result, bail};
use std::ffi::{OsStr, OsString, c_void};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::thread;
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_NORMAL, HFONT, UpdateWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{
    EM_SETCUEBANNER, ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCFMT_LEFT, LVCOLUMNW, LVIF_TEXT,
    LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE,
    LVM_SETITEMTEXTW, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP,
    NM_DBLCLK, NMHDR, SB_SETTEXTW, STATUSCLASSNAMEW, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, CreateMenu, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, IsDialogMessageW, LoadCursorW,
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW,
    MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOW, SendMessageW, SetMenu,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_NCCREATE, WM_NOTIFY,
    WM_SETFONT, WM_SIZE, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE,
    WS_EX_CONTROLPARENT, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use zfs_send_extract::client::{
    ClientExtraction, InceptionCatalog, SourceCatalog, apply_incremental, child_path, parent_path,
};
use zfs_send_extract::filesystem::DirectoryEntry;
use zfs_send_extract::operations::Sidecar;
use zfs_send_extract::tree::RecursiveExtraction;

const CLASS_NAME: &str = "ZfsSendExploreWindows";
const APP_TITLE: &str = "ZFS Send Explorer";

const ID_SOURCE_PATH: u16 = 100;
const ID_BROWSE_SOURCE: u16 = 101;
const ID_OPEN_SEND: u16 = 102;
const ID_OPEN_POOL: u16 = 103;
const ID_VIEW: u16 = 104;
const ID_PATH: u16 = 105;
const ID_UP: u16 = 106;
const ID_GO: u16 = 107;
const ID_CHOOSE_KEY: u16 = 108;
const ID_LIST: u16 = 109;
const ID_EXTRACT: u16 = 110;
const ID_UPDATE: u16 = 111;
const ID_EXIT: u16 = 112;
const ID_ABOUT: u16 = 113;
const ID_INCEPTION: u16 = 114;
const ID_LEAVE_IMAGE: u16 = 115;
const ID_VOLUME: u16 = 116;
const ID_IMAGE_OFFSET: u16 = 117;
const ID_IMAGE_LENGTH: u16 = 118;
const ID_CONTAINER_KEY: u16 = 119;
const ID_AGENT_PASSWORD: u16 = 120;

const WM_JOB_COMPLETE: u32 = WM_APP + 1;

#[derive(Default)]
struct Controls {
    source_path: HWND,
    browse_source: HWND,
    open_send: HWND,
    open_pool: HWND,
    view: HWND,
    volume: HWND,
    path: HWND,
    up: HWND,
    go: HWND,
    choose_key: HWND,
    container_key: HWND,
    agent_password: HWND,
    list: HWND,
    extract: HWND,
    inception: HWND,
    leave_image: HWND,
    image_offset: HWND,
    image_length: HWND,
    update: HWND,
    status: HWND,
}

struct AppState {
    hwnd: HWND,
    controls: Controls,
    font: HFONT,
    catalog: Option<SourceCatalog>,
    entries: Vec<DirectoryEntry>,
    current_path: String,
    key_file: Option<PathBuf>,
    container_key_file: Option<PathBuf>,
    agent_password_file: Option<PathBuf>,
    inception: Option<InceptionCatalog>,
    busy: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            hwnd: null_mut(),
            controls: Controls::default(),
            font: null_mut(),
            catalog: None,
            entries: Vec::new(),
            current_path: "/".to_owned(),
            key_file: None,
            container_key_file: None,
            agent_password_file: None,
            inception: None,
            busy: false,
        }
    }
}

enum JobResult {
    Catalog(SourceCatalog),
    Directory {
        path: String,
        entries: Vec<DirectoryEntry>,
    },
    Extracted {
        destination: PathBuf,
        extraction: ClientExtraction,
        nested: bool,
    },
    TreeExtracted {
        destination: PathBuf,
        extraction: RecursiveExtraction,
    },
    InceptionOpened(InceptionCatalog),
    Updated {
        target: PathBuf,
        sidecar: Sidecar,
    },
}

type JobMessage = std::result::Result<JobResult, String>;

pub fn run() -> Result<()> {
    // SAFETY: the application initializes process-wide DPI awareness before it
    // creates any HWND, then confines all HWND access to this UI thread.
    unsafe { run_ui() }
}

unsafe fn run_ui() -> Result<()> {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let common = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES,
    };
    if InitCommonControlsEx(&common) == 0 {
        bail!("Windows common controls could not be initialized");
    }

    let instance = GetModuleHandleW(null());
    if instance.is_null() {
        return Err(std::io::Error::last_os_error()).context("getting application module");
    }
    let class_name = wide(CLASS_NAME);
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_WINDOW as usize + 1) as _,
        lpszClassName: class_name.as_ptr(),
        ..zeroed()
    };
    if RegisterClassW(&class) == 0 {
        return Err(std::io::Error::last_os_error()).context("registering window class");
    }

    let state = Box::into_raw(Box::new(AppState::default()));
    let title = wide(APP_TITLE);
    let hwnd = CreateWindowExW(
        WS_EX_CONTROLPARENT,
        class_name.as_ptr(),
        title.as_ptr(),
        WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        1120,
        720,
        null_mut(),
        null_mut(),
        instance,
        state.cast::<c_void>(),
    );
    if hwnd.is_null() {
        drop(Box::from_raw(state));
        return Err(std::io::Error::last_os_error()).context("creating main window");
    }
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);

    if let Some(argument) = std::env::args_os().nth(1) {
        set_text((*state).controls.source_path, &argument.to_string_lossy());
    }

    let mut message = MSG::default();
    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
        if IsDialogMessageW(hwnd, &message) == 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    drop(Box::from_raw(state));
    Ok(())
}

pub fn show_fatal_error(message: &str) {
    // SAFETY: all strings are valid, NUL-terminated UTF-16 for the duration of
    // the synchronous message-box call.
    unsafe {
        let title = wide("ZFS Send Explorer could not start");
        let message = wide(message);
        MessageBoxW(
            null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        let state = create.lpCreateParams as *mut AppState;
        (*state).hwnd = hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState;
    match message {
        WM_CREATE if !state.is_null() => {
            if let Err(error) = initialize_window(&mut *state) {
                show_error(
                    hwnd,
                    "Could not create the application window",
                    &format!("{error:#}"),
                );
                return -1;
            }
            0
        }
        WM_SIZE if !state.is_null() => {
            layout(&*state);
            0
        }
        WM_DPICHANGED => {
            let suggested = &*(lparam as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                0x0004 | 0x0010,
            );
            0
        }
        WM_COMMAND if !state.is_null() => {
            handle_command(&mut *state, wparam);
            0
        }
        WM_NOTIFY if !state.is_null() => {
            handle_notify(&mut *state, lparam);
            0
        }
        WM_JOB_COMPLETE if !state.is_null() => {
            let result = Box::from_raw(lparam as *mut JobMessage);
            finish_job(&mut *state, *result);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            if !state.is_null() && !(*state).font.is_null() {
                DeleteObject((*state).font);
                (*state).font = null_mut();
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn initialize_window(state: &mut AppState) -> Result<()> {
    let instance = GetModuleHandleW(null());
    state.font = CreateFontW(
        -16,
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.into(),
        0,
        0,
        5,
        0,
        wide("Segoe UI").as_ptr(),
    );
    let menu = CreateMenu();
    let file_menu = CreateMenu();
    let actions_menu = CreateMenu();
    let help_menu = CreateMenu();
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_SEND as usize,
        "Open send file…",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_POOL as usize,
        "Open pool member / drive",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_CHOOSE_KEY as usize,
        "Choose ZFS dataset key…",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_CONTAINER_KEY as usize,
        "Choose Datto pool passphrase…",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_AGENT_PASSWORD as usize,
        "Choose Datto agent password…",
    );
    AppendMenuW(file_menu, MF_SEPARATOR, 0, null());
    append_menu(file_menu, MF_STRING, ID_EXIT as usize, "Exit");
    append_menu(
        actions_menu,
        MF_STRING,
        ID_EXTRACT as usize,
        "Extract selected file or folder…",
    );
    append_menu(
        actions_menu,
        MF_STRING,
        ID_INCEPTION as usize,
        "Explore selected disk image…",
    );
    append_menu(
        actions_menu,
        MF_STRING,
        ID_LEAVE_IMAGE as usize,
        "Leave disk image",
    );
    append_menu(
        actions_menu,
        MF_STRING,
        ID_UPDATE as usize,
        "Update an extracted file…",
    );
    append_menu(
        help_menu,
        MF_STRING,
        ID_ABOUT as usize,
        "About ZFS Send Explorer",
    );
    append_menu(menu, MF_POPUP, file_menu as usize, "File");
    append_menu(menu, MF_POPUP, actions_menu as usize, "Actions");
    append_menu(menu, MF_POPUP, help_menu as usize, "Help");
    SetMenu(state.hwnd, menu);

    let controls = &mut state.controls;
    controls.source_path = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
        0,
        ID_SOURCE_PATH,
    );
    controls.browse_source = button(state.hwnd, instance, "Browse…", ID_BROWSE_SOURCE, false);
    controls.open_send = button(state.hwnd, instance, "Open send file", ID_OPEN_SEND, true);
    controls.open_pool = button(
        state.hwnd,
        instance,
        "Open pool / drive",
        ID_OPEN_POOL,
        false,
    );
    controls.view = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        0,
        ID_VIEW,
    );
    controls.volume = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        0,
        ID_VOLUME,
    );
    controls.path = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "/",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
        0,
        ID_PATH,
    );
    controls.up = button(state.hwnd, instance, "Up", ID_UP, false);
    controls.go = button(state.hwnd, instance, "Go", ID_GO, false);
    controls.choose_key = button(
        state.hwnd,
        instance,
        "Decryption key…",
        ID_CHOOSE_KEY,
        false,
    );
    controls.container_key = button(
        state.hwnd,
        instance,
        "Datto pool key…",
        ID_CONTAINER_KEY,
        false,
    );
    controls.agent_password = button(
        state.hwnd,
        instance,
        "Datto agent password…",
        ID_AGENT_PASSWORD,
        false,
    );
    controls.list = control(
        state.hwnd,
        instance,
        WC_LISTVIEWW,
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | 0x0001,
        WS_EX_CLIENTEDGE,
        ID_LIST,
    );
    controls.extract = button(
        state.hwnd,
        instance,
        "Extract file / folder…",
        ID_EXTRACT,
        false,
    );
    controls.inception = button(
        state.hwnd,
        instance,
        "Explore disk image…",
        ID_INCEPTION,
        false,
    );
    controls.leave_image = button(
        state.hwnd,
        instance,
        "Leave disk image",
        ID_LEAVE_IMAGE,
        false,
    );
    controls.image_offset = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
        0,
        ID_IMAGE_OFFSET,
    );
    controls.image_length = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
        0,
        ID_IMAGE_LENGTH,
    );
    controls.update = button(
        state.hwnd,
        instance,
        "Update extracted file…",
        ID_UPDATE,
        false,
    );
    controls.status = control(
        state.hwnd,
        instance,
        STATUSCLASSNAMEW,
        "Ready — open a send file, vdev image, or type \\\\.\\PhysicalDriveN",
        WS_CHILD | WS_VISIBLE,
        0,
        0,
    );

    for hwnd in all_controls(controls) {
        if hwnd.is_null() {
            bail!("a native Windows control could not be created");
        }
        SendMessageW(hwnd, WM_SETFONT, state.font as usize, 1);
    }
    let cue = wide("Send file, vdev image, partition path, or \\\\.\\PhysicalDriveN");
    SendMessageW(
        controls.source_path,
        EM_SETCUEBANNER,
        1,
        cue.as_ptr() as isize,
    );
    let offset_cue = wide("Image offset");
    SendMessageW(
        controls.image_offset,
        EM_SETCUEBANNER,
        0,
        offset_cue.as_ptr() as isize,
    );
    let length_cue = wide("Image length (optional)");
    SendMessageW(
        controls.image_length,
        EM_SETCUEBANNER,
        0,
        length_cue.as_ptr() as isize,
    );
    SendMessageW(
        controls.list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
    );
    add_column(controls.list, 0, "Name", 480);
    add_column(controls.list, 1, "Type", 130);
    add_column(controls.list, 2, "Size", 150);
    add_column(controls.list, 3, "Object", 120);
    set_source_enabled(state, false);
    EnableWindow(state.controls.open_send, 1);
    EnableWindow(state.controls.open_pool, 1);
    EnableWindow(state.controls.browse_source, 1);
    EnableWindow(state.controls.source_path, 1);
    EnableWindow(state.controls.update, 1);
    EnableWindow(state.controls.container_key, 1);
    EnableWindow(state.controls.agent_password, 1);
    layout(state);
    Ok(())
}

unsafe fn handle_command(state: &mut AppState, wparam: WPARAM) {
    let id = (wparam & 0xffff) as u16;
    let notification = ((wparam >> 16) & 0xffff) as u16;
    if state.busy && !matches!(id, ID_EXIT | ID_ABOUT) {
        return;
    }
    match id {
        ID_BROWSE_SOURCE if notification == BN_CLICKED as u16 => {
            if let Some(path) =
                open_file_dialog(state.hwnd, "Choose a ZFS source", SourceFilter::All)
            {
                set_text(state.controls.source_path, &path.display().to_string());
            }
        }
        ID_OPEN_SEND => open_source(state, true),
        ID_OPEN_POOL => open_source(state, false),
        ID_VIEW if notification == CBN_SELCHANGE as u16 => {
            leave_inception(state, false);
            browse(state, "/".to_owned());
        }
        ID_VOLUME if notification == CBN_SELCHANGE as u16 => browse(state, "/".to_owned()),
        ID_GO => browse(state, get_text(state.controls.path)),
        ID_UP => browse(state, parent_path(&state.current_path)),
        ID_CHOOSE_KEY => choose_key(state),
        ID_CONTAINER_KEY => choose_container_key(state),
        ID_AGENT_PASSWORD => choose_agent_password(state),
        ID_EXTRACT => extract_selected(state),
        ID_INCEPTION => explore_selected_image(state),
        ID_LEAVE_IMAGE => leave_inception(state, true),
        ID_UPDATE => update_extracted_file(state),
        ID_EXIT => {
            DestroyWindow(state.hwnd);
        }
        ID_ABOUT => show_about(state.hwnd),
        _ => {}
    }
}

unsafe fn handle_notify(state: &mut AppState, lparam: LPARAM) {
    let header = &*(lparam as *const NMHDR);
    if header.hwndFrom == state.controls.list && header.code == NM_DBLCLK {
        let Some(index) = selected_index(state.controls.list) else {
            return;
        };
        let Some(entry) = state.entries.get(index) else {
            return;
        };
        if entry.dirent_type == 4 {
            match child_path(&state.current_path, &entry.name) {
                Ok(path) => browse(state, path),
                Err(error) => {
                    show_error(state.hwnd, "Could not open directory", &error.to_string())
                }
            }
        } else if entry.dirent_type == 8 {
            extract_selected(state);
        }
    }
}

unsafe fn open_source(state: &mut AppState, send: bool) {
    let path = PathBuf::from(get_text(state.controls.source_path));
    if path.as_os_str().is_empty() {
        let title = if send {
            "Choose a ZFS send file"
        } else {
            "Choose a pool member or image"
        };
        let filter = if send {
            SourceFilter::Send
        } else {
            SourceFilter::All
        };
        let Some(selected) = open_file_dialog(state.hwnd, title, filter) else {
            return;
        };
        set_text(state.controls.source_path, &selected.display().to_string());
        return open_source(state, send);
    }
    set_busy(
        state,
        true,
        if send {
            "Inspecting send stream…"
        } else {
            "Reading pool labels and datasets…"
        },
    );
    let container_key = state.container_key_file.clone();
    spawn_job(state.hwnd, move || {
        if send {
            SourceCatalog::open_send(path).map(JobResult::Catalog)
        } else {
            SourceCatalog::open_pool_with_container_key_file(path, container_key.as_deref())
                .map(JobResult::Catalog)
        }
    });
}

unsafe fn browse(state: &mut AppState, path: String) {
    let Some(catalog) = state.catalog.clone() else {
        return;
    };
    let Some(view) = selected_view(state) else {
        return;
    };
    let inception = state.inception.clone();
    let selected_volume_index = if inception.is_some() {
        let Some(index) = selected_volume_index(state) else {
            return;
        };
        index
    } else {
        0
    };
    let key = state.key_file.clone();
    set_busy(state, true, "Reading directory…");
    spawn_job(state.hwnd, move || {
        let entries = if let Some(inception) = inception {
            let volume = inception
                .volumes
                .get(selected_volume_index)
                .ok_or_else(|| anyhow::anyhow!("selected inner volume no longer exists"))?;
            inception.list_directory(Some(&volume.selector), &path)
        } else {
            catalog.list_directory(view, &path, key.as_deref())
        }?;
        Ok(JobResult::Directory { path, entries })
    });
}

unsafe fn extract_selected(state: &mut AppState) {
    let Some(index) = selected_index(state.controls.list) else {
        show_error(
            state.hwnd,
            "No item selected",
            "Select a regular file or folder in the list first.",
        );
        return;
    };
    let Some(entry) = state.entries.get(index).cloned() else {
        return;
    };
    if !matches!(entry.dirent_type, 4 | 8) {
        show_error(
            state.hwnd,
            "Cannot extract this item",
            "Only regular files and folders can be extracted.",
        );
        return;
    }
    let is_directory = entry.dirent_type == 4;
    let Some(destination) = save_extraction_dialog(state.hwnd, &entry.name, is_directory) else {
        return;
    };
    let Some(catalog) = state.catalog.clone() else {
        return;
    };
    let Some(view) = selected_view(state) else {
        return;
    };
    let source_path = match child_path(&state.current_path, &entry.name) {
        Ok(path) => path,
        Err(error) => {
            show_error(state.hwnd, "Invalid source path", &error.to_string());
            return;
        }
    };
    let key = state.key_file.clone();
    let inception = state.inception.clone();
    let volume_index = selected_volume_index(state);
    set_busy(
        state,
        true,
        if is_directory {
            "Recursively extracting into a staged directory tree…"
        } else {
            "Extracting into a sparse temporary file…"
        },
    );
    spawn_job(state.hwnd, move || {
        if is_directory {
            let extraction = if let Some(inception) = inception {
                let volume = inception
                    .volumes
                    .get(volume_index.ok_or_else(|| anyhow::anyhow!("no inner volume selected"))?)
                    .ok_or_else(|| anyhow::anyhow!("selected inner volume no longer exists"))?;
                inception.extract_tree(Some(&volume.selector), &source_path, &destination, true)?
            } else {
                catalog.extract_tree(view, &source_path, &destination, true, key.as_deref())?
            };
            return Ok(JobResult::TreeExtracted {
                destination,
                extraction,
            });
        }
        let (extraction, nested) = if let Some(inception) = inception {
            let volume = inception
                .volumes
                .get(volume_index.ok_or_else(|| anyhow::anyhow!("no inner volume selected"))?)
                .ok_or_else(|| anyhow::anyhow!("selected inner volume no longer exists"))?;
            (
                inception.extract(Some(&volume.selector), &source_path, &destination, true)?,
                true,
            )
        } else {
            (
                catalog.extract(view, &source_path, &destination, true, key.as_deref())?,
                false,
            )
        };
        Ok(JobResult::Extracted {
            destination,
            extraction,
            nested,
        })
    });
}

unsafe fn explore_selected_image(state: &mut AppState) {
    if state.inception.is_some() {
        return;
    }
    let Some(index) = selected_index(state.controls.list) else {
        show_error(
            state.hwnd,
            "No disk image selected",
            "Select a regular file that contains a raw, QCOW2, or VMDK disk image.",
        );
        return;
    };
    let Some(entry) = state.entries.get(index) else {
        return;
    };
    if entry.dirent_type != 8 {
        show_error(
            state.hwnd,
            "Cannot explore this item",
            "Only regular files can contain a subordinate disk image.",
        );
        return;
    }
    let image_path = match child_path(&state.current_path, &entry.name) {
        Ok(path) => path,
        Err(error) => {
            show_error(state.hwnd, "Invalid image path", &error.to_string());
            return;
        }
    };
    let image_offset = match parse_byte_value(&get_text(state.controls.image_offset), false) {
        Ok(Some(value)) => value,
        Ok(None) => 0,
        Err(error) => {
            show_error(state.hwnd, "Invalid image offset", &error);
            return;
        }
    };
    let image_length = match parse_byte_value(&get_text(state.controls.image_length), true) {
        Ok(value) => value,
        Err(error) => {
            show_error(state.hwnd, "Invalid image length", &error);
            return;
        }
    };
    let Some(catalog) = state.catalog.clone() else {
        return;
    };
    let Some(view) = selected_view(state) else {
        return;
    };
    let key = state.key_file.clone();
    let agent_password = image_path
        .to_ascii_lowercase()
        .ends_with(".detto")
        .then(|| state.agent_password_file.clone())
        .flatten();
    set_busy(
        state,
        true,
        "Opening disk container and detecting inner volumes…",
    );
    spawn_job(state.hwnd, move || {
        catalog
            .inspect_inception_with_datto(
                view,
                &image_path,
                key.as_deref(),
                agent_password.as_deref(),
                None,
                image_offset,
                image_length,
            )
            .map(JobResult::InceptionOpened)
    });
}

unsafe fn leave_inception(state: &mut AppState, browse_outer: bool) {
    if state.inception.take().is_none() {
        return;
    }
    SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    set_text(state.controls.image_offset, "");
    set_text(state.controls.image_length, "");
    state.current_path = "/".to_owned();
    set_source_enabled(state, !state.busy && state.catalog.is_some());
    if browse_outer {
        browse(state, "/".to_owned());
    }
}

unsafe fn update_extracted_file(state: &mut AppState) {
    let Some(target) = open_file_dialog(
        state.hwnd,
        "Choose a file previously extracted by ZFS Send Explorer",
        SourceFilter::All,
    ) else {
        return;
    };
    let Some(stream) = open_file_dialog(
        state.hwnd,
        "Choose the matching incremental ZFS send file",
        SourceFilter::Send,
    ) else {
        return;
    };
    let key = state.key_file.clone();
    set_busy(
        state,
        true,
        "Validating the base file and applying the incremental send…",
    );
    spawn_job(state.hwnd, move || {
        apply_incremental(&stream, &target, key.as_deref())
            .map(|sidecar| JobResult::Updated { target, sidecar })
    });
}

unsafe fn choose_key(state: &mut AppState) {
    if let Some(path) = open_file_dialog(
        state.hwnd,
        "Choose the ZFS dataset key file",
        SourceFilter::All,
    ) {
        state.key_file = Some(path.clone());
        set_status(
            state,
            &format!("Decryption key selected: {}", file_label(&path)),
        );
        if state.catalog.is_some() {
            browse(state, state.current_path.clone());
        }
    }
}

unsafe fn choose_container_key(state: &mut AppState) {
    if let Some(path) = open_file_dialog(
        state.hwnd,
        "Choose the Datto Reverse RoundTrip pool passphrase file",
        SourceFilter::All,
    ) {
        state.container_key_file = Some(path.clone());
        set_status(
            state,
            &format!("Datto pool passphrase selected: {}", file_label(&path)),
        );
    }
}

unsafe fn choose_agent_password(state: &mut AppState) {
    if let Some(path) = open_file_dialog(
        state.hwnd,
        "Choose the Datto agent password file",
        SourceFilter::All,
    ) {
        state.agent_password_file = Some(path.clone());
        set_status(
            state,
            &format!("Datto agent password selected: {}", file_label(&path)),
        );
    }
}

unsafe fn finish_job(state: &mut AppState, result: JobMessage) {
    set_busy(state, false, "Ready");
    match result {
        Err(error) => show_error(state.hwnd, "The operation did not complete", &error),
        Ok(JobResult::Catalog(catalog)) => {
            state.inception = None;
            SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
            SendMessageW(state.controls.view, CB_RESETCONTENT, 0, 0);
            for view in &catalog.views {
                let label = wide(&view.label);
                SendMessageW(
                    state.controls.view,
                    CB_ADDSTRING,
                    0,
                    label.as_ptr() as isize,
                );
            }
            SendMessageW(state.controls.view, CB_SETCURSEL, 0, 0);
            let title = format!("{} — {}", catalog.title, APP_TITLE);
            set_text(state.hwnd, &title);
            set_status(state, &catalog.summary);
            state.catalog = Some(catalog);
            set_source_enabled(state, true);
            browse(state, "/".to_owned());
        }
        Ok(JobResult::Directory { path, entries }) => {
            state.current_path = path;
            set_text(state.controls.path, &state.current_path);
            state.entries = entries;
            populate_list(state);
            set_status(
                state,
                &format!(
                    "{} item{} in {}{}",
                    state.entries.len(),
                    if state.entries.len() == 1 { "" } else { "s" },
                    state.current_path,
                    state
                        .inception
                        .as_ref()
                        .map_or_else(String::new, |image| format!(" inside {}", image.image_path))
                ),
            );
        }
        Ok(JobResult::InceptionOpened(inception)) => {
            SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
            let mut first_supported = None;
            for (index, volume) in inception.volumes.iter().enumerate() {
                let label = wide(&volume.label());
                SendMessageW(
                    state.controls.volume,
                    CB_ADDSTRING,
                    0,
                    label.as_ptr() as isize,
                );
                if first_supported.is_none() && volume.filesystem.is_some() {
                    first_supported = Some(index);
                }
            }
            let Some(selected) = first_supported else {
                let details = inception
                    .volumes
                    .iter()
                    .filter_map(|volume| volume.diagnostic.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                show_error(
                    state.hwnd,
                    "No supported inner filesystem",
                    if details.is_empty() {
                        "No NTFS, FAT, exFAT, or ext filesystem was detected."
                    } else {
                        &details
                    },
                );
                SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
                return;
            };
            SendMessageW(state.controls.volume, CB_SETCURSEL, selected, 0);
            set_text(
                state.controls.image_offset,
                &inception.image_offset.to_string(),
            );
            set_text(
                state.controls.image_length,
                &inception.stored_size.to_string(),
            );
            let status = format!(
                "Inception mode · {} · {} virtual bytes · {} volume{}",
                inception.container,
                inception.disk_size,
                inception.volumes.len(),
                if inception.volumes.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
            state.inception = Some(inception);
            state.current_path = "/".to_owned();
            set_source_enabled(state, true);
            set_status(state, &status);
            browse(state, "/".to_owned());
        }
        Ok(JobResult::Extracted {
            destination,
            extraction,
            nested,
        }) => {
            let update = if nested {
                " The subordinate filesystem and its ZFS source were not modified."
            } else if extraction.update_eligible {
                " Update metadata was saved beside it."
            } else {
                " This current-dataset extraction cannot be advanced by an incremental send."
            };
            let message = format!(
                "Extracted {} ({}).{}\n\nSHA-256: {}",
                destination.display(),
                format_size(extraction.logical_size),
                update,
                extraction.sha256
            );
            show_information(state.hwnd, "Extraction complete", &message);
            set_status(state, &format!("Extracted {}", destination.display()));
        }
        Ok(JobResult::TreeExtracted {
            destination,
            extraction,
        }) => {
            let skipped = if extraction.skipped_entries == 0 {
                String::new()
            } else {
                format!(
                    "\n\nSkipped {} symbolic link or special entr{}; none were followed.",
                    extraction.skipped_entries,
                    if extraction.skipped_entries == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                )
            };
            let message = format!(
                "Extracted {} file{} in {} director{} to {}.\n\nLogical data: {}{}",
                extraction.files,
                if extraction.files == 1 { "" } else { "s" },
                extraction.directories,
                if extraction.directories == 1 {
                    "y"
                } else {
                    "ies"
                },
                destination.display(),
                format_size(extraction.logical_bytes),
                skipped
            );
            show_information(state.hwnd, "Folder extraction complete", &message);
            set_status(
                state,
                &format!("Extracted folder to {}", destination.display()),
            );
        }
        Ok(JobResult::Updated { target, sidecar }) => {
            let message = format!(
                "Updated {} to snapshot {}.\n\nNew size: {}\nSHA-256: {}",
                target.display(),
                sidecar.snapshot_guid,
                format_size(sidecar.logical_size),
                sidecar.sha256
            );
            show_information(state.hwnd, "Update complete", &message);
            set_status(state, &format!("Updated {}", target.display()));
        }
    }
}

unsafe fn populate_list(state: &AppState) {
    let list = state.controls.list;
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    for (index, entry) in state.entries.iter().enumerate() {
        let name = wide(&entry.name);
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: index as i32,
            iSubItem: 0,
            pszText: name.as_ptr() as *mut u16,
            ..zeroed()
        };
        SendMessageW(
            list,
            LVM_INSERTITEMW,
            0,
            (&mut item as *mut LVITEMW) as isize,
        );
        let kind = match entry.dirent_type {
            4 => "Folder",
            8 => "File",
            10 => "Symbolic link",
            _ => "Other",
        };
        set_list_text(list, index, 1, kind);
        set_list_text(
            list,
            index,
            2,
            &entry.logical_size.map_or_else(String::new, format_size),
        );
        set_list_text(list, index, 3, &entry.object_id.to_string());
    }
    if !state.entries.is_empty() {
        let mut selection = LVITEMW {
            stateMask: LVIS_SELECTED,
            state: LVIS_SELECTED,
            ..zeroed()
        };
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            0,
            (&mut selection as *mut LVITEMW) as isize,
        );
        SendMessageW(list, LVM_ENSUREVISIBLE, 0, 0);
    }
}

fn spawn_job<F>(hwnd: HWND, operation: F)
where
    F: FnOnce() -> Result<JobResult> + Send + 'static,
{
    let window = hwnd as usize;
    thread::spawn(move || {
        let result = operation().map_err(|error| format!("{error:#}"));
        let pointer = Box::into_raw(Box::new(result));
        // SAFETY: `pointer` is consumed by WM_JOB_COMPLETE. If the window has
        // already closed, posting fails and this worker reclaims the box.
        let posted = unsafe { PostMessageW(window as HWND, WM_JOB_COMPLETE, 0, pointer as isize) };
        if posted == 0 {
            // SAFETY: the UI did not receive ownership when posting failed.
            unsafe { drop(Box::from_raw(pointer)) };
        }
    });
}

unsafe fn set_busy(state: &mut AppState, busy: bool, message: &str) {
    state.busy = busy;
    let enabled = (!busy) as i32;
    for control in [
        state.controls.source_path,
        state.controls.browse_source,
        state.controls.open_send,
        state.controls.open_pool,
        state.controls.update,
        state.controls.container_key,
        state.controls.agent_password,
    ] {
        EnableWindow(control, enabled);
    }
    set_source_enabled(state, !busy && state.catalog.is_some());
    set_status(state, message);
}

unsafe fn set_source_enabled(state: &AppState, enabled: bool) {
    let enabled = enabled as i32;
    for control in [
        state.controls.view,
        state.controls.path,
        state.controls.up,
        state.controls.go,
        state.controls.choose_key,
        state.controls.list,
        state.controls.extract,
        state.controls.inception,
        state.controls.image_offset,
        state.controls.image_length,
    ] {
        EnableWindow(control, enabled);
    }
    EnableWindow(
        state.controls.volume,
        (enabled != 0 && state.inception.is_some()) as i32,
    );
    EnableWindow(
        state.controls.leave_image,
        (enabled != 0 && state.inception.is_some()) as i32,
    );
    EnableWindow(
        state.controls.inception,
        (enabled != 0 && state.inception.is_none()) as i32,
    );
    EnableWindow(
        state.controls.image_offset,
        (enabled != 0 && state.inception.is_none()) as i32,
    );
    EnableWindow(
        state.controls.image_length,
        (enabled != 0 && state.inception.is_none()) as i32,
    );
}

unsafe fn selected_view(state: &AppState) -> Option<usize> {
    let selected = SendMessageW(state.controls.view, CB_GETCURSEL, 0, 0);
    (selected >= 0).then_some(selected as usize)
}

unsafe fn selected_volume_index(state: &AppState) -> Option<usize> {
    let selected = SendMessageW(state.controls.volume, CB_GETCURSEL, 0, 0);
    (selected >= 0).then_some(selected as usize)
}

unsafe fn selected_index(list: HWND) -> Option<usize> {
    let selected = SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
    (selected >= 0).then_some(selected as usize)
}

unsafe fn layout(state: &AppState) {
    if state.controls.status.is_null() {
        return;
    }
    SendMessageW(state.controls.status, WM_SIZE, 0, 0);
    let mut client = RECT::default();
    GetClientRect(state.hwnd, &mut client);
    let dpi = GetDpiForWindow(state.hwnd).max(96) as i32;
    let scale = |value: i32| value * dpi / 96;
    let pad = scale(12);
    let gap = scale(8);
    let row = scale(30);
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    let status_height = scale(24);

    let browse_width = scale(84);
    let send_width = scale(118);
    let pool_width = scale(136);
    let container_key_width = scale(124);
    let source_edit_width =
        (width - pad * 2 - gap * 4 - browse_width - send_width - pool_width - container_key_width)
            .max(scale(180));
    let mut x = pad;
    MoveWindow(
        state.controls.source_path,
        x,
        pad,
        source_edit_width,
        row,
        1,
    );
    x += source_edit_width + gap;
    MoveWindow(state.controls.browse_source, x, pad, browse_width, row, 1);
    x += browse_width + gap;
    MoveWindow(state.controls.open_send, x, pad, send_width, row, 1);
    x += send_width + gap;
    MoveWindow(state.controls.open_pool, x, pad, pool_width, row, 1);
    x += pool_width + gap;
    MoveWindow(
        state.controls.container_key,
        x,
        pad,
        container_key_width,
        row,
        1,
    );

    let second_y = pad + row + gap;
    let view_width = (width * 42 / 100).max(scale(260));
    let up_width = scale(54);
    let go_width = scale(54);
    let key_width = scale(126);
    let path_width =
        (width - pad * 2 - gap * 4 - view_width - up_width - go_width - key_width).max(scale(120));
    x = pad;
    MoveWindow(state.controls.view, x, second_y, view_width, scale(240), 1);
    x += view_width + gap;
    MoveWindow(state.controls.up, x, second_y, up_width, row, 1);
    x += up_width + gap;
    MoveWindow(state.controls.path, x, second_y, path_width, row, 1);
    x += path_width + gap;
    MoveWindow(state.controls.go, x, second_y, go_width, row, 1);
    x += go_width + gap;
    MoveWindow(state.controls.choose_key, x, second_y, key_width, row, 1);

    let third_y = second_y + row + gap;
    let volume_width = (width * 36 / 100).max(scale(260));
    let offset_width = scale(116);
    let length_width = scale(154);
    let inception_width = scale(156);
    let leave_width = scale(126);
    x = pad;
    MoveWindow(
        state.controls.volume,
        x,
        third_y,
        volume_width,
        scale(240),
        1,
    );
    x += volume_width + gap;
    MoveWindow(
        state.controls.image_offset,
        x,
        third_y,
        offset_width,
        row,
        1,
    );
    x += offset_width + gap;
    MoveWindow(
        state.controls.image_length,
        x,
        third_y,
        length_width,
        row,
        1,
    );
    x += length_width + gap;
    MoveWindow(
        state.controls.inception,
        x,
        third_y,
        inception_width,
        row,
        1,
    );
    x += inception_width + gap;
    MoveWindow(state.controls.leave_image, x, third_y, leave_width, row, 1);

    let fourth_y = third_y + row + gap;
    MoveWindow(
        state.controls.agent_password,
        pad,
        fourth_y,
        scale(172),
        row,
        1,
    );

    let actions_y = height - status_height - pad - row;
    let list_y = fourth_y + row + gap;
    let list_height = (actions_y - gap - list_y).max(scale(80));
    MoveWindow(
        state.controls.list,
        pad,
        list_y,
        width - pad * 2,
        list_height,
        1,
    );
    MoveWindow(state.controls.extract, pad, actions_y, scale(144), row, 1);
    MoveWindow(
        state.controls.update,
        pad + scale(144) + gap,
        actions_y,
        scale(172),
        row,
        1,
    );
}

unsafe fn control(
    parent: HWND,
    instance: HINSTANCE,
    class: *const u16,
    text: &str,
    style: u32,
    ex_style: WINDOW_EX_STYLE,
    id: u16,
) -> HWND {
    let text = wide(text);
    CreateWindowExW(
        ex_style,
        class,
        text.as_ptr(),
        style,
        0,
        0,
        0,
        0,
        parent,
        id as usize as HMENU,
        instance,
        null(),
    )
}

unsafe fn button(parent: HWND, instance: HINSTANCE, text: &str, id: u16, default: bool) -> HWND {
    control(
        parent,
        instance,
        windows_sys::core::w!("BUTTON"),
        text,
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | if default {
                BS_DEFPUSHBUTTON as u32
            } else {
                BS_PUSHBUTTON as u32
            },
        0,
        id,
    )
}

unsafe fn all_controls(controls: &Controls) -> [HWND; 20] {
    [
        controls.source_path,
        controls.browse_source,
        controls.open_send,
        controls.open_pool,
        controls.view,
        controls.volume,
        controls.path,
        controls.up,
        controls.go,
        controls.choose_key,
        controls.container_key,
        controls.agent_password,
        controls.list,
        controls.extract,
        controls.inception,
        controls.leave_image,
        controls.image_offset,
        controls.image_length,
        controls.update,
        controls.status,
    ]
}

unsafe fn add_column(list: HWND, index: usize, text: &str, width: i32) {
    let text = wide(text);
    let mut column = LVCOLUMNW {
        mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
        fmt: LVCFMT_LEFT,
        cx: width,
        pszText: text.as_ptr() as *mut u16,
        ..zeroed()
    };
    SendMessageW(
        list,
        LVM_INSERTCOLUMNW,
        index,
        (&mut column as *mut LVCOLUMNW) as isize,
    );
}

unsafe fn set_list_text(list: HWND, row: usize, column: i32, text: &str) {
    let text = wide(text);
    let mut item = LVITEMW {
        iSubItem: column,
        pszText: text.as_ptr() as *mut u16,
        ..zeroed()
    };
    SendMessageW(
        list,
        LVM_SETITEMTEXTW,
        row,
        (&mut item as *mut LVITEMW) as isize,
    );
}

unsafe fn append_menu(menu: HMENU, flags: u32, id: usize, label: &str) {
    let label = wide(label);
    AppendMenuW(menu, flags, id, label.as_ptr());
}

unsafe fn set_status(state: &AppState, text: &str) {
    let text = wide(text);
    SendMessageW(
        state.controls.status,
        SB_SETTEXTW,
        0,
        text.as_ptr() as isize,
    );
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    SetWindowTextW(hwnd, text.as_ptr());
}

unsafe fn get_text(hwnd: HWND) -> String {
    let length = GetWindowTextLengthW(hwnd);
    let mut buffer = vec![0_u16; length as usize + 1];
    GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
    String::from_utf16_lossy(&buffer[..length as usize])
}

fn parse_byte_value(value: &str, empty_is_none: bool) -> std::result::Result<Option<u64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return if empty_is_none { Ok(None) } else { Ok(Some(0)) };
    }
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| format!("{value:?} is not a valid byte count (decimal or 0x hexadecimal)"))?;
    if empty_is_none && parsed == 0 {
        return Err("image length must be greater than zero".to_owned());
    }
    Ok(Some(parsed))
}

#[derive(Clone, Copy)]
enum SourceFilter {
    Send,
    All,
}

unsafe fn open_file_dialog(owner: HWND, title: &str, filter: SourceFilter) -> Option<PathBuf> {
    file_dialog(owner, title, filter, None, false)
}

unsafe fn save_extraction_dialog(owner: HWND, name: &str, directory: bool) -> Option<PathBuf> {
    file_dialog(
        owner,
        if directory {
            "Save recovered folder as"
        } else {
            "Extract file"
        },
        SourceFilter::All,
        Some(name),
        true,
    )
}

unsafe fn file_dialog(
    owner: HWND,
    title: &str,
    filter: SourceFilter,
    initial_name: Option<&str>,
    save: bool,
) -> Option<PathBuf> {
    let filter = match filter {
        SourceFilter::Send => wide_nul_groups(&[
            "ZFS send streams",
            "*.zfs;*.zstream;*.send",
            "All files",
            "*.*",
        ]),
        SourceFilter::All => wide_nul_groups(&["All files", "*.*"]),
    };
    let title = wide(title);
    let mut file = vec![0_u16; 32_768];
    if let Some(name) = initial_name {
        let encoded = OsStr::new(name).encode_wide().collect::<Vec<_>>();
        let count = encoded.len().min(file.len() - 1);
        file[..count].copy_from_slice(&encoded[..count]);
    }
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST
            },
        ..zeroed()
    };
    let accepted = if save {
        GetSaveFileNameW(&mut dialog)
    } else {
        GetOpenFileNameW(&mut dialog)
    };
    if accepted == 0 {
        return None;
    }
    let length = file
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(file.len());
    Some(PathBuf::from(OsString::from_wide(&file[..length])))
}

unsafe fn show_error(owner: HWND, title: &str, message: &str) {
    show_message(owner, title, message, MB_OK | MB_ICONERROR);
}

unsafe fn show_information(owner: HWND, title: &str, message: &str) {
    show_message(owner, title, message, MB_OK | MB_ICONINFORMATION);
}

unsafe fn show_about(owner: HWND) {
    show_information(
        owner,
        "About ZFS Send Explorer",
        "Browse snapshots in ZFS send files, Slide Boxes, Datto Reverse RoundTrip drives, and exported pool members without importing or mounting them. Inception mode opens raw, .datto, authenticated .detto, QCOW2, and self-contained sparse VMDK files to explore NTFS, FAT, exFAT, and ext filesystems one layer deeper.\n\nSources are always opened read-only. File and staged folder extraction plus validated incremental updates preserve sparse file ranges when the destination filesystem supports them.",
    );
}

unsafe fn show_message(owner: HWND, title: &str, message: &str, flags: u32) {
    let title = wide(title);
    let message = wide(message);
    MessageBoxW(owner, message.as_ptr(), title.as_ptr(), flags);
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn wide_nul_groups(values: &[&str]) -> Vec<u16> {
    let mut output = Vec::new();
    for value in values {
        output.extend(OsStr::new(value).encode_wide());
        output.push(0);
    }
    output.push(0);
    output
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into(),
    )
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#![allow(unsafe_op_in_unsafe_fn)]

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString, c_void};
use std::fs;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::thread;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, HINSTANCE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT,
    WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_NORMAL, HFONT, UpdateWindow,
};
use windows_sys::Win32::Security::Credentials::{
    CREDUI_FLAGS_ALWAYS_SHOW_UI, CREDUI_FLAGS_DO_NOT_PERSIST, CREDUI_FLAGS_EXCLUDE_CERTIFICATES,
    CREDUI_FLAGS_GENERIC_CREDENTIALS, CREDUI_FLAGS_KEEP_USERNAME, CREDUI_INFOW,
    CredUIPromptForCredentialsW,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_QUERY_PROPERTY,
    PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
    StorageDeviceProperty,
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
    LVM_SETITEMTEXTW, LVN_ITEMCHANGED, LVN_KEYDOWN, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, NM_DBLCLK, NMHDR, NMLVKEYDOWN, SB_SETTEXTW,
    STATUSCLASSNAMEW, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_BACK, VK_RETURN};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ACCEL, AppendMenuW, BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, CheckMenuItem, CreateAcceleratorTableW, CreateMenu, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyAcceleratorTable, DestroyMenu, DestroyWindow,
    DispatchMessageW, FALT, FCONTROL, FSHIFT, FVIRTKEY, GWLP_USERDATA, GetClientRect, GetMenu,
    GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, HACCEL,
    HMENU, IDC_ARROW, IsDialogMessageW, LoadCursorW, MB_ICONERROR, MB_ICONINFORMATION,
    MB_ICONWARNING, MB_OK, MB_OKCANCEL, MF_BYCOMMAND, MF_CHECKED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MF_UNCHECKED, MSG, MessageBoxW, MoveWindow, PostMessageW, PostQuitMessage,
    RegisterClassW, SW_SHOW, SendMessageW, SetMenu, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN, TrackPopupMenu,
    TranslateAcceleratorW, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_NCCREATE, WM_NOTIFY, WM_SETFONT, WM_SIZE, WNDCLASSW,
    WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use zeroize::{Zeroize, Zeroizing};
use zfs_send_extract::client::{
    ClientExtraction, InceptionCatalog, SourceCatalog, apply_incremental_with_key_material,
    child_path, parent_path,
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
const ID_OPEN_AUTO: u16 = 121;
const ID_OPEN_IMAGE: u16 = 122;
const ID_REFRESH: u16 = 123;
const ID_FOCUS_PATH: u16 = 124;
const ID_ENTER_KEY: u16 = 125;
const ID_ENTER_CONTAINER_KEY: u16 = 126;
const ID_ENTER_AGENT_PASSWORD: u16 = 127;
const ID_CLEAR_CREDENTIALS: u16 = 128;
const ID_SETTING_AUTO_IMAGES: u16 = 129;
const ID_SETTING_CLEAR_CREDENTIALS: u16 = 130;
const ID_SETTING_CONFIRM_DRIVE: u16 = 131;
const ID_SHORTCUTS: u16 = 132;
const ID_OPEN_SELECTED: u16 = 133;

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
    key: Option<SecretValue>,
    container_key: Option<SecretValue>,
    agent_password: Option<SecretValue>,
    inception: Vec<ImageFrame>,
    physical_drives: Vec<PhysicalDrive>,
    settings: UiSettings,
    busy: bool,
}

#[derive(Clone)]
struct SecretValue {
    material: std::sync::Arc<Zeroizing<Vec<u8>>>,
    label: String,
    source: Option<PathBuf>,
}

impl SecretValue {
    fn new(material: Vec<u8>, label: impl Into<String>, source: Option<PathBuf>) -> Self {
        Self {
            material: std::sync::Arc::new(Zeroizing::new(material)),
            label: label.into(),
            source,
        }
    }

    fn bytes(&self) -> &[u8] {
        self.material.as_slice()
    }
}

#[derive(Clone)]
struct ImageFrame {
    catalog: InceptionCatalog,
    parent_path: Option<String>,
    parent_volume: Option<usize>,
}

#[derive(Debug, Clone)]
struct PhysicalDrive {
    path: PathBuf,
    label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct UiSettings {
    auto_open_images: bool,
    clear_credentials_on_source_change: bool,
    confirm_physical_drive: bool,
    window_width: i32,
    window_height: i32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            auto_open_images: true,
            clear_credentials_on_source_change: true,
            confirm_physical_drive: true,
            window_width: 1180,
            window_height: 760,
        }
    }
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
            key: None,
            container_key: None,
            agent_password: None,
            inception: Vec::new(),
            physical_drives: Vec::new(),
            settings: load_settings(),
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
    InceptionOpened {
        catalog: InceptionCatalog,
        parent_path: Option<String>,
        parent_volume: Option<usize>,
        standalone: bool,
    },
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
        (*state).settings.window_width.max(900),
        (*state).settings.window_height.max(620),
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
        PostMessageW(hwnd, WM_COMMAND, ID_OPEN_AUTO as usize, 0);
    }

    let accelerators = create_accelerators()?;
    let mut message = MSG::default();
    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
        if TranslateAcceleratorW(hwnd, accelerators, &message) == 0
            && IsDialogMessageW(hwnd, &message) == 0
        {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    DestroyAcceleratorTable(accelerators);
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
            if !state.is_null() {
                let mut window = RECT::default();
                if GetWindowRect(hwnd, &mut window) != 0 {
                    (*state).settings.window_width = window.right - window.left;
                    (*state).settings.window_height = window.bottom - window.top;
                }
                let _ = save_settings(&(*state).settings);
                if !(*state).font.is_null() {
                    DeleteObject((*state).font);
                    (*state).font = null_mut();
                }
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
    let credentials_menu = CreateMenu();
    let settings_menu = CreateMenu();
    let help_menu = CreateMenu();
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_AUTO as usize,
        "Open backup or disk image…\tCtrl+O",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_IMAGE as usize,
        "Open standalone disk image…\tCtrl+Shift+O",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_POOL as usize,
        "Open selected physical drive\tCtrl+D",
    );
    AppendMenuW(file_menu, MF_SEPARATOR, 0, null());
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_SEND as usize,
        "Open path as ZFS send stream",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_REFRESH as usize,
        "Refresh current view / drives\tF5",
    );
    AppendMenuW(file_menu, MF_SEPARATOR, 0, null());
    append_menu(file_menu, MF_STRING, ID_EXIT as usize, "Exit");

    append_menu(
        credentials_menu,
        MF_STRING,
        ID_ENTER_KEY as usize,
        "Enter ZFS key or passphrase…\tCtrl+K",
    );
    append_menu(
        credentials_menu,
        MF_STRING,
        ID_CHOOSE_KEY as usize,
        "Choose ZFS key file…",
    );
    AppendMenuW(credentials_menu, MF_SEPARATOR, 0, null());
    append_menu(
        credentials_menu,
        MF_STRING,
        ID_ENTER_CONTAINER_KEY as usize,
        "Enter Datto pool passphrase…",
    );
    append_menu(
        credentials_menu,
        MF_STRING,
        ID_CONTAINER_KEY as usize,
        "Choose Datto pool passphrase file…",
    );
    append_menu(
        credentials_menu,
        MF_STRING,
        ID_ENTER_AGENT_PASSWORD as usize,
        "Enter Datto agent password…",
    );
    append_menu(
        credentials_menu,
        MF_STRING,
        ID_AGENT_PASSWORD as usize,
        "Choose Datto agent password file…",
    );
    AppendMenuW(credentials_menu, MF_SEPARATOR, 0, null());
    append_menu(
        credentials_menu,
        MF_STRING,
        ID_CLEAR_CREDENTIALS as usize,
        "Clear all credentials",
    );
    append_menu(
        actions_menu,
        MF_STRING,
        ID_EXTRACT as usize,
        "Extract selected file or folder…\tCtrl+E",
    );
    append_menu(
        actions_menu,
        MF_STRING,
        ID_INCEPTION as usize,
        "Explore selected disk image…\tCtrl+I",
    );
    append_menu(
        actions_menu,
        MF_STRING,
        ID_LEAVE_IMAGE as usize,
        "Go back one image layer\tEsc",
    );
    append_menu(
        actions_menu,
        MF_STRING,
        ID_UPDATE as usize,
        "Update an extracted file…\tCtrl+U",
    );
    append_menu(
        settings_menu,
        MF_STRING,
        ID_SETTING_AUTO_IMAGES as usize,
        "Open recognized disk images on double-click",
    );
    append_menu(
        settings_menu,
        MF_STRING,
        ID_SETTING_CLEAR_CREDENTIALS as usize,
        "Clear credentials when opening another source",
    );
    append_menu(
        settings_menu,
        MF_STRING,
        ID_SETTING_CONFIRM_DRIVE as usize,
        "Confirm physical-drive selection",
    );
    append_menu(
        help_menu,
        MF_STRING,
        ID_SHORTCUTS as usize,
        "Keyboard shortcuts",
    );
    append_menu(
        help_menu,
        MF_STRING,
        ID_ABOUT as usize,
        "About ZFS Send Explorer",
    );
    append_menu(menu, MF_POPUP, file_menu as usize, "File");
    append_menu(menu, MF_POPUP, credentials_menu as usize, "Credentials");
    append_menu(menu, MF_POPUP, actions_menu as usize, "Actions");
    append_menu(menu, MF_POPUP, settings_menu as usize, "Settings");
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
    controls.open_send = button(state.hwnd, instance, "Open", ID_OPEN_AUTO, true);
    controls.open_pool = button(state.hwnd, instance, "Open drive", ID_OPEN_POOL, false);
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
    controls.choose_key = button(state.hwnd, instance, "Credentials…", ID_ENTER_KEY, false);
    controls.container_key = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        0,
        ID_CONTAINER_KEY,
    );
    controls.agent_password = control(
        state.hwnd,
        instance,
        windows_sys::core::w!("STATIC"),
        "Source > choose a view > browse files",
        WS_CHILD | WS_VISIBLE,
        0,
        0,
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
        "Back one image",
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
        "Ready — open a backup, choose a physical drive, or open a disk image",
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
    let cue = wide("Backup, pool member, or standalone disk-image path");
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
    update_settings_menu(state);
    refresh_physical_drives(state);
    update_credential_button(state);
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
            if let Some(path) = open_file_dialog(
                state.hwnd,
                "Choose a backup, pool member, or disk image",
                SourceFilter::All,
            ) {
                set_text(state.controls.source_path, &path.display().to_string());
            }
        }
        ID_OPEN_AUTO => open_source(state, OpenMode::Auto),
        ID_OPEN_IMAGE => open_source(state, OpenMode::Image),
        ID_OPEN_SEND => open_source(state, OpenMode::Send),
        ID_OPEN_POOL => open_physical_drive(state),
        ID_VIEW if notification == CBN_SELCHANGE as u16 => {
            leave_all_images(state, false);
            ensure_selected_view_key(state);
            browse(state, "/".to_owned());
        }
        ID_VOLUME if notification == CBN_SELCHANGE as u16 => browse(state, "/".to_owned()),
        ID_CONTAINER_KEY if notification == CBN_SELCHANGE as u16 => select_physical_drive(state),
        ID_GO => browse(state, get_text(state.controls.path)),
        ID_UP => browse(state, parent_path(&state.current_path)),
        ID_FOCUS_PATH => {
            SetFocus(state.controls.path);
        }
        ID_REFRESH => refresh(state),
        ID_ENTER_KEY if notification == BN_CLICKED as u16 => show_credentials_menu(state),
        ID_ENTER_KEY => enter_key(state),
        ID_CHOOSE_KEY => choose_key_file(state),
        ID_ENTER_CONTAINER_KEY => enter_container_key(state),
        ID_CONTAINER_KEY => choose_container_key_file(state),
        ID_ENTER_AGENT_PASSWORD => enter_agent_password(state),
        ID_AGENT_PASSWORD => choose_agent_password_file(state),
        ID_CLEAR_CREDENTIALS => clear_credentials(state),
        ID_EXTRACT => extract_selected(state),
        ID_INCEPTION => explore_selected_image(state),
        ID_OPEN_SELECTED => open_selected(state),
        ID_LEAVE_IMAGE => leave_inception(state, true),
        ID_UPDATE => update_extracted_file(state),
        ID_SETTING_AUTO_IMAGES => toggle_setting(state, ID_SETTING_AUTO_IMAGES),
        ID_SETTING_CLEAR_CREDENTIALS => toggle_setting(state, ID_SETTING_CLEAR_CREDENTIALS),
        ID_SETTING_CONFIRM_DRIVE => toggle_setting(state, ID_SETTING_CONFIRM_DRIVE),
        ID_SHORTCUTS => show_shortcuts(state.hwnd),
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
        open_selected(state);
    } else if header.hwndFrom == state.controls.list && header.code == LVN_ITEMCHANGED {
        update_selection_actions(state);
    } else if header.hwndFrom == state.controls.list && header.code == LVN_KEYDOWN {
        let key = lparam as *const NMLVKEYDOWN;
        let virtual_key = std::ptr::addr_of!((*key).wVKey).read_unaligned();
        match virtual_key {
            value if value == VK_RETURN => open_selected(state),
            value if value == VK_BACK => browse(state, parent_path(&state.current_path)),
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
enum OpenMode {
    Auto,
    Send,
    Pool,
    Image,
}

unsafe fn open_source(state: &mut AppState, mode: OpenMode) {
    let path = PathBuf::from(get_text(state.controls.source_path));
    if path.as_os_str().is_empty() {
        let (title, filter) = match mode {
            OpenMode::Send => ("Choose a ZFS send file", SourceFilter::Send),
            OpenMode::Image => ("Choose a standalone disk image", SourceFilter::Image),
            OpenMode::Pool => (
                "Choose a pool member or whole-disk image",
                SourceFilter::All,
            ),
            OpenMode::Auto => (
                "Choose a backup, pool member, or disk image",
                SourceFilter::All,
            ),
        };
        let Some(selected) = open_file_dialog(state.hwnd, title, filter) else {
            return;
        };
        set_text(state.controls.source_path, &selected.display().to_string());
        return open_source(state, mode);
    }
    if state.settings.clear_credentials_on_source_change && source_is_changing(state, &path) {
        clear_credentials_for_source(state, &path);
    }
    set_busy(
        state,
        true,
        "Inspecting the source and detecting its format…",
    );
    let container_key = state.container_key.clone();
    spawn_job(state.hwnd, move || {
        let pool_key = container_key.as_ref().map(SecretValue::bytes);
        match mode {
            OpenMode::Send => SourceCatalog::open_send(path).map(JobResult::Catalog),
            OpenMode::Pool => SourceCatalog::open_pool_with_container_key_material(path, pool_key)
                .map(JobResult::Catalog),
            OpenMode::Image => InceptionCatalog::open_file(&path, 0, None).map(|catalog| {
                JobResult::InceptionOpened {
                    catalog,
                    parent_path: None,
                    parent_volume: None,
                    standalone: true,
                }
            }),
            OpenMode::Auto => open_automatically(path, pool_key),
        }
    });
}

unsafe fn browse(state: &mut AppState, path: String) {
    let inception = state.inception.last().map(|frame| frame.catalog.clone());
    let selected_volume_index = if inception.is_some() {
        let Some(index) = selected_volume_index(state) else {
            return;
        };
        index
    } else {
        0
    };
    let catalog = state.catalog.clone();
    let view = selected_view(state);
    if inception.is_none() && (catalog.is_none() || view.is_none()) {
        return;
    }
    let key = state.key.clone();
    set_busy(state, true, "Reading directory…");
    spawn_job(state.hwnd, move || {
        let entries = if let Some(inception) = inception {
            let volume = inception
                .volumes
                .get(selected_volume_index)
                .ok_or_else(|| anyhow::anyhow!("selected inner volume no longer exists"))?;
            inception.list_directory(Some(&volume.selector), &path)
        } else {
            catalog
                .ok_or_else(|| anyhow::anyhow!("no source is open"))?
                .list_directory_with_key_material(
                    view.ok_or_else(|| anyhow::anyhow!("no source view is selected"))?,
                    &path,
                    key.as_ref().map(SecretValue::bytes),
                )
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
    let catalog = state.catalog.clone();
    let view = selected_view(state);
    let source_path = match child_path(&state.current_path, &entry.name) {
        Ok(path) => path,
        Err(error) => {
            show_error(state.hwnd, "Invalid source path", &error.to_string());
            return;
        }
    };
    let key = state.key.clone();
    let inception = state.inception.last().map(|frame| frame.catalog.clone());
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
                catalog
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no ZFS source is open"))?
                    .extract_tree_with_key_material(
                        view.ok_or_else(|| anyhow::anyhow!("no source view is selected"))?,
                        &source_path,
                        &destination,
                        true,
                        key.as_ref().map(SecretValue::bytes),
                    )?
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
                catalog
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no ZFS source is open"))?
                    .extract_with_key_material(
                        view.ok_or_else(|| anyhow::anyhow!("no source view is selected"))?,
                        &source_path,
                        &destination,
                        true,
                        key.as_ref().map(SecretValue::bytes),
                    )?,
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
    if state.inception.len() >= 8 {
        show_error(
            state.hwnd,
            "Image nesting limit reached",
            "Eight nested disk-image layers are already open. Go back before opening another.",
        );
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
    let catalog = state.catalog.clone();
    let view = selected_view(state);
    let parent_image = state.inception.last().map(|frame| frame.catalog.clone());
    let parent_volume = selected_volume_index(state);
    let key = state.key.clone();
    let agent_password = image_path
        .to_ascii_lowercase()
        .ends_with(".detto")
        .then(|| state.agent_password.clone())
        .flatten();
    let parent_path = state.current_path.clone();
    set_busy(
        state,
        true,
        "Opening disk container and detecting inner volumes…",
    );
    spawn_job(state.hwnd, move || {
        let child = if let Some(parent_image) = parent_image {
            let volume = parent_image
                .volumes
                .get(parent_volume.ok_or_else(|| anyhow::anyhow!("no inner volume selected"))?)
                .ok_or_else(|| anyhow::anyhow!("selected inner volume no longer exists"))?;
            parent_image.inspect_child(
                Some(&volume.selector),
                &image_path,
                image_offset,
                image_length,
            )?
        } else {
            catalog
                .ok_or_else(|| anyhow::anyhow!("no ZFS source is open"))?
                .inspect_inception_with_key_material(
                    view.ok_or_else(|| anyhow::anyhow!("no source view is selected"))?,
                    &image_path,
                    key.as_ref().map(SecretValue::bytes),
                    agent_password.as_ref().map(SecretValue::bytes),
                    None,
                    image_offset,
                    image_length,
                )?
        };
        Ok(JobResult::InceptionOpened {
            catalog: child,
            parent_path: Some(parent_path),
            parent_volume,
            standalone: false,
        })
    });
}

unsafe fn leave_inception(state: &mut AppState, browse_outer: bool) {
    let Some(frame) = state.inception.pop() else {
        return;
    };
    SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    set_text(state.controls.image_offset, "");
    set_text(state.controls.image_length, "");
    let destination = frame.parent_path.unwrap_or_else(|| "/".to_owned());
    if let Some(image) = state.inception.last() {
        populate_volumes(state, &image.catalog, frame.parent_volume);
    }
    set_source_enabled(
        state,
        !state.busy && (state.catalog.is_some() || !state.inception.is_empty()),
    );
    update_breadcrumb(state);
    if browse_outer {
        if state.catalog.is_none() && state.inception.is_empty() {
            close_source_ui(state);
        } else {
            browse(state, destination);
        }
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
    let key = state.key.clone();
    set_busy(
        state,
        true,
        "Validating the base file and applying the incremental send…",
    );
    spawn_job(state.hwnd, move || {
        apply_incremental_with_key_material(&stream, &target, key.as_ref().map(SecretValue::bytes))
            .map(|sidecar| JobResult::Updated { target, sidecar })
    });
}

unsafe fn enter_key(state: &mut AppState) {
    if let Some(material) = prompt_secret(
        state.hwnd,
        "ZFS dataset key",
        "Enter the passphrase or hexadecimal key for the selected encrypted ZFS view. Binary raw keys must be chosen from a file.",
    ) {
        state.key = Some(SecretValue::new(
            material,
            "entered in memory",
            credential_scope(state),
        ));
        credential_changed(state, "ZFS key entered in protected memory");
        if state.catalog.is_some() && state.inception.is_empty() {
            browse(state, state.current_path.clone());
        }
    }
}

unsafe fn enter_container_key(state: &mut AppState) {
    if let Some(material) = prompt_secret(
        state.hwnd,
        "Datto pool passphrase",
        "Enter the LUKS passphrase supplied for this Datto Reverse RoundTrip drive.",
    ) {
        state.container_key = Some(SecretValue::new(
            material,
            "entered in memory",
            credential_scope(state),
        ));
        credential_changed(state, "Datto pool passphrase entered in protected memory");
    }
}

unsafe fn enter_agent_password(state: &mut AppState) {
    if let Some(material) = prompt_secret(
        state.hwnd,
        "Datto agent password",
        "Enter the protected system's agent password used to authenticate its .detto key stash.",
    ) {
        state.agent_password = Some(SecretValue::new(
            material,
            "entered in memory",
            credential_scope(state),
        ));
        credential_changed(state, "Datto agent password entered in protected memory");
    }
}

unsafe fn choose_key_file(state: &mut AppState) {
    if let Some(mut secret) =
        choose_secret_file(state.hwnd, "Choose the ZFS dataset key file", 514, false)
    {
        secret.source = credential_scope(state);
        state.key = Some(secret);
        credential_changed(state, "ZFS key file loaded into protected memory");
        if state.catalog.is_some() && state.inception.is_empty() {
            browse(state, state.current_path.clone());
        }
    }
}

unsafe fn choose_container_key_file(state: &mut AppState) {
    if let Some(mut secret) = choose_secret_file(
        state.hwnd,
        "Choose the Datto Reverse RoundTrip pool passphrase file",
        4096,
        true,
    ) {
        secret.source = credential_scope(state);
        state.container_key = Some(secret);
        credential_changed(
            state,
            "Datto pool passphrase file loaded into protected memory",
        );
    }
}

unsafe fn choose_agent_password_file(state: &mut AppState) {
    if let Some(mut secret) = choose_secret_file(
        state.hwnd,
        "Choose the Datto agent password file",
        4096,
        true,
    ) {
        secret.source = credential_scope(state);
        state.agent_password = Some(secret);
        credential_changed(
            state,
            "Datto agent password file loaded into protected memory",
        );
    }
}

unsafe fn finish_job(state: &mut AppState, result: JobMessage) {
    set_busy(state, false, "Ready");
    match result {
        Err(error) => show_error(state.hwnd, "The operation did not complete", &error),
        Ok(JobResult::Catalog(catalog)) => {
            state.inception.clear();
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
            update_breadcrumb(state);
            ensure_selected_view_key(state);
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
                        .last()
                        .map_or_else(String::new, |frame| format!(
                            " inside {}",
                            frame.catalog.image_path
                        ))
                ),
            );
            update_breadcrumb(state);
        }
        Ok(JobResult::InceptionOpened {
            catalog,
            parent_path,
            parent_volume,
            standalone,
        }) => {
            let Some(selected) = first_supported_volume(&catalog) else {
                let details = catalog
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
                return;
            };
            if standalone {
                state.catalog = None;
                state.inception.clear();
                SendMessageW(state.controls.view, CB_RESETCONTENT, 0, 0);
                set_text(
                    state.hwnd,
                    &format!(
                        "{} — {}",
                        file_label(Path::new(&catalog.image_path)),
                        APP_TITLE
                    ),
                );
            }
            populate_volumes(state, &catalog, Some(selected));
            set_text(
                state.controls.image_offset,
                &catalog.image_offset.to_string(),
            );
            set_text(
                state.controls.image_length,
                &catalog.stored_size.to_string(),
            );
            let status = format!(
                "Disk image layer {} · {} · {} virtual bytes · {} volume{}",
                state.inception.len() + 1,
                catalog.container,
                catalog.disk_size,
                catalog.volumes.len(),
                if catalog.volumes.len() == 1 { "" } else { "s" }
            );
            state.inception.push(ImageFrame {
                catalog,
                parent_path,
                parent_volume,
            });
            state.current_path = "/".to_owned();
            set_source_enabled(state, true);
            set_status(state, &status);
            update_breadcrumb(state);
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
    update_selection_actions(state);
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
    set_source_enabled(
        state,
        !busy && (state.catalog.is_some() || !state.inception.is_empty()),
    );
    set_status(state, message);
}

unsafe fn set_source_enabled(state: &AppState, enabled: bool) {
    let enabled = enabled as i32;
    for control in [
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
        state.controls.view,
        (enabled != 0 && state.catalog.is_some() && state.inception.is_empty()) as i32,
    );
    EnableWindow(
        state.controls.volume,
        (enabled != 0 && !state.inception.is_empty()) as i32,
    );
    EnableWindow(
        state.controls.leave_image,
        (enabled != 0 && !state.inception.is_empty()) as i32,
    );
    if enabled != 0 {
        update_selection_actions(state);
    }
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

unsafe fn update_selection_actions(state: &AppState) {
    if state.busy {
        return;
    }
    let entry = selected_index(state.controls.list).and_then(|index| state.entries.get(index));
    let extractable = entry.is_some_and(|entry| matches!(entry.dirent_type, 4 | 8));
    let image = entry.is_some_and(|entry| entry.dirent_type == 8);
    EnableWindow(state.controls.extract, extractable as i32);
    EnableWindow(state.controls.inception, image as i32);
    let label = entry
        .filter(|entry| likely_disk_image(&entry.name))
        .map_or("Explore as disk image…", |_| "Open disk image…");
    set_text(state.controls.inception, label);
}

unsafe fn open_selected(state: &mut AppState) {
    let Some(index) = selected_index(state.controls.list) else {
        return;
    };
    let Some(entry) = state.entries.get(index) else {
        return;
    };
    if entry.dirent_type == 4 {
        match child_path(&state.current_path, &entry.name) {
            Ok(path) => browse(state, path),
            Err(error) => show_error(state.hwnd, "Could not open directory", &error.to_string()),
        }
    } else if entry.dirent_type == 8
        && state.settings.auto_open_images
        && likely_disk_image(&entry.name)
    {
        explore_selected_image(state);
    } else if entry.dirent_type == 8 {
        extract_selected(state);
    }
}

fn likely_disk_image(name: &str) -> bool {
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    [
        "raw", "img", "dd", "qcow", "qcow2", "vmdk", "datto", "detto", "vhd", "vhdx",
    ]
    .iter()
    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

fn open_automatically(path: PathBuf, container_key: Option<&[u8]>) -> Result<JobResult> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let send_first = matches!(extension.as_str(), "zfs" | "zstream" | "send");
    let image_first = matches!(extension.as_str(), "qcow" | "qcow2" | "vmdk" | "dd");
    let mut errors = Vec::new();

    if image_first {
        match InceptionCatalog::open_file(&path, 0, None) {
            Ok(catalog) => {
                return Ok(JobResult::InceptionOpened {
                    catalog,
                    parent_path: None,
                    parent_volume: None,
                    standalone: true,
                });
            }
            Err(error) => errors.push(format!("disk image: {error:#}")),
        }
    }
    if send_first {
        match SourceCatalog::open_send(&path) {
            Ok(catalog) => return Ok(JobResult::Catalog(catalog)),
            Err(error) => errors.push(format!("ZFS send: {error:#}")),
        }
    }
    match SourceCatalog::open_pool_with_container_key_material(&path, container_key) {
        Ok(catalog) => return Ok(JobResult::Catalog(catalog)),
        Err(error) => errors.push(format!("pool member: {error:#}")),
    }
    if !send_first {
        match SourceCatalog::open_send(&path) {
            Ok(catalog) => return Ok(JobResult::Catalog(catalog)),
            Err(error) => errors.push(format!("ZFS send: {error:#}")),
        }
    }
    if !image_first {
        match InceptionCatalog::open_file(&path, 0, None) {
            Ok(catalog) => {
                return Ok(JobResult::InceptionOpened {
                    catalog,
                    parent_path: None,
                    parent_volume: None,
                    standalone: true,
                });
            }
            Err(error) => errors.push(format!("disk image: {error:#}")),
        }
    }
    bail!(
        "The source was not recognized as a supported send stream, pool member, or disk image.\n\n{}",
        errors.join("\n\n")
    )
}

unsafe fn select_physical_drive(state: &mut AppState) {
    let selected = SendMessageW(state.controls.container_key, CB_GETCURSEL, 0, 0);
    if selected <= 0 {
        return;
    }
    if let Some(drive) = state.physical_drives.get(selected as usize - 1) {
        set_text(
            state.controls.source_path,
            &drive.path.display().to_string(),
        );
        set_status(state, &format!("Selected {}", drive.label));
    }
}

unsafe fn open_physical_drive(state: &mut AppState) {
    select_physical_drive(state);
    let path = PathBuf::from(get_text(state.controls.source_path));
    let Some(drive) = state
        .physical_drives
        .iter()
        .find(|drive| drive.path == path)
    else {
        if path.as_os_str().is_empty() {
            show_error(
                state.hwnd,
                "Choose a physical drive",
                "Select a drive from the Physical drives list first. Press F5 after attaching a new drive.",
            );
        } else {
            open_source(state, OpenMode::Pool);
        }
        return;
    };
    if state.settings.confirm_physical_drive {
        let message = format!(
            "Open this physical drive read-only?\n\n{}\n{}\n\nThe app will not initialize, mount, import, or write to it. Verify the disk number after every attach or removal.",
            drive.label,
            drive.path.display()
        );
        if !show_confirmation(state.hwnd, "Confirm physical drive", &message) {
            return;
        }
    }
    open_source(state, OpenMode::Pool);
}

unsafe fn refresh_physical_drives(state: &mut AppState) {
    state.physical_drives = enumerate_physical_drives();
    SendMessageW(state.controls.container_key, CB_RESETCONTENT, 0, 0);
    let prompt = if state.physical_drives.is_empty() {
        "No physical drives detected — press F5 to scan again"
    } else {
        "Physical drives — choose by model, size, and disk number"
    };
    let prompt = wide(prompt);
    SendMessageW(
        state.controls.container_key,
        CB_ADDSTRING,
        0,
        prompt.as_ptr() as isize,
    );
    for drive in &state.physical_drives {
        let label = wide(&drive.label);
        SendMessageW(
            state.controls.container_key,
            CB_ADDSTRING,
            0,
            label.as_ptr() as isize,
        );
    }
    SendMessageW(state.controls.container_key, CB_SETCURSEL, 0, 0);
}

fn enumerate_physical_drives() -> Vec<PhysicalDrive> {
    (0..64).filter_map(inspect_physical_drive).collect()
}

fn inspect_physical_drive(number: u32) -> Option<PhysicalDrive> {
    let path = format!(r"\\.\PhysicalDrive{number}");
    let path_wide = wide(&path);
    // SAFETY: the handle is opened with zero desired access and is closed on
    // every successful path. IOCTL buffers have their documented layouts.
    unsafe {
        let handle = CreateFileW(
            path_wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut length = GET_LENGTH_INFORMATION::default();
        let mut returned = 0_u32;
        let size = (DeviceIoControl(
            handle,
            IOCTL_DISK_GET_LENGTH_INFO,
            null(),
            0,
            (&mut length as *mut GET_LENGTH_INFORMATION).cast(),
            size_of::<GET_LENGTH_INFORMATION>() as u32,
            &mut returned,
            null_mut(),
        ) != 0)
            .then_some(length.Length as u64);

        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut descriptor = vec![0_u8; 1024];
        let descriptor_ok = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&query as *const STORAGE_PROPERTY_QUERY).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            descriptor.as_mut_ptr().cast(),
            descriptor.len() as u32,
            &mut returned,
            null_mut(),
        ) != 0;
        let (model, removable) =
            if descriptor_ok && returned as usize >= size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
                let header = &*(descriptor.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>());
                let vendor = descriptor_string(&descriptor, header.VendorIdOffset);
                let product = descriptor_string(&descriptor, header.ProductIdOffset);
                let model = [vendor, product]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                (model, header.RemovableMedia)
            } else {
                (String::new(), false)
            };
        CloseHandle(handle);
        let model = if model.is_empty() {
            "Unknown device".to_owned()
        } else {
            model
        };
        let size_label = size.map_or_else(|| "unknown size".to_owned(), format_size);
        let media = if removable { "removable" } else { "fixed" };
        Some(PhysicalDrive {
            path: PathBuf::from(&path),
            label: format!("Disk {number} — {model} — {size_label} — {media}"),
        })
    }
}

fn descriptor_string(buffer: &[u8], offset: u32) -> String {
    let start = offset as usize;
    if start == 0 || start >= buffer.len() {
        return String::new();
    }
    let end = buffer[start..]
        .iter()
        .position(|byte| *byte == 0)
        .map_or(buffer.len(), |relative| start + relative);
    String::from_utf8_lossy(&buffer[start..end])
        .trim()
        .to_owned()
}

unsafe fn refresh(state: &mut AppState) {
    refresh_physical_drives(state);
    if state.catalog.is_some() || !state.inception.is_empty() {
        browse(state, state.current_path.clone());
    } else {
        set_status(
            state,
            &format!(
                "Found {} physical drive{}",
                state.physical_drives.len(),
                if state.physical_drives.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
        );
    }
}

unsafe fn show_credentials_menu(state: &mut AppState) {
    let menu = CreatePopupMenu();
    append_menu(
        menu,
        MF_STRING,
        ID_ENTER_KEY as usize,
        "Enter ZFS key or passphrase…",
    );
    append_menu(
        menu,
        MF_STRING,
        ID_CHOOSE_KEY as usize,
        "Choose ZFS key file…",
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    append_menu(
        menu,
        MF_STRING,
        ID_ENTER_CONTAINER_KEY as usize,
        "Enter Datto pool passphrase…",
    );
    append_menu(
        menu,
        MF_STRING,
        ID_CONTAINER_KEY as usize,
        "Choose Datto pool key file…",
    );
    append_menu(
        menu,
        MF_STRING,
        ID_ENTER_AGENT_PASSWORD as usize,
        "Enter Datto agent password…",
    );
    append_menu(
        menu,
        MF_STRING,
        ID_AGENT_PASSWORD as usize,
        "Choose Datto agent password file…",
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    append_menu(
        menu,
        MF_STRING,
        ID_CLEAR_CREDENTIALS as usize,
        "Clear all credentials",
    );
    let mut rect = RECT::default();
    GetWindowRect(state.controls.choose_key, &mut rect);
    let command = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD,
        rect.left,
        rect.bottom,
        0,
        state.hwnd,
        null(),
    );
    if command != 0 {
        SendMessageW(state.hwnd, WM_COMMAND, command as usize, 0);
    }
    DestroyMenu(menu);
}

unsafe fn prompt_secret(owner: HWND, label: &str, message: &str) -> Option<Vec<u8>> {
    let caption = wide(label);
    let message = wide(message);
    let target = wide("ZFS Send Explorer");
    let mut username = wide(label);
    username.resize(514, 0);
    let mut password = vec![0_u16; 514];
    let mut save = 0;
    let info = CREDUI_INFOW {
        cbSize: size_of::<CREDUI_INFOW>() as u32,
        hwndParent: owner,
        pszMessageText: message.as_ptr(),
        pszCaptionText: caption.as_ptr(),
        hbmBanner: null_mut(),
    };
    let result = CredUIPromptForCredentialsW(
        &info,
        target.as_ptr(),
        null(),
        0,
        username.as_mut_ptr(),
        username.len() as u32,
        password.as_mut_ptr(),
        password.len() as u32,
        &mut save,
        CREDUI_FLAGS_ALWAYS_SHOW_UI
            | CREDUI_FLAGS_DO_NOT_PERSIST
            | CREDUI_FLAGS_EXCLUDE_CERTIFICATES
            | CREDUI_FLAGS_GENERIC_CREDENTIALS
            | CREDUI_FLAGS_KEEP_USERNAME,
    );
    username.zeroize();
    if result == ERROR_CANCELLED {
        password.zeroize();
        return None;
    }
    if result != 0 {
        password.zeroize();
        show_error(
            owner,
            "Could not open the secure credential prompt",
            &format!("Windows returned error {result}."),
        );
        return None;
    }
    let length = password
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(password.len());
    let mut value = String::from_utf16_lossy(&password[..length]);
    password.zeroize();
    let material = value.as_bytes().to_vec();
    value.zeroize();
    Some(material)
}

unsafe fn choose_secret_file(
    owner: HWND,
    title: &str,
    maximum_size: u64,
    trim_newline: bool,
) -> Option<SecretValue> {
    let path = open_file_dialog(owner, title, SourceFilter::All)?;
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            show_error(owner, "Could not read credential file", &error.to_string());
            return None;
        }
    };
    if metadata.len() > maximum_size {
        show_error(
            owner,
            "Credential file is too large",
            &format!("{} is larger than {maximum_size} bytes.", path.display()),
        );
        return None;
    }
    match fs::read(&path) {
        Ok(mut material) => {
            if trim_newline && material.last() == Some(&b'\n') {
                material.pop();
                if material.last() == Some(&b'\r') {
                    material.pop();
                }
            }
            Some(SecretValue::new(material, file_label(&path), None))
        }
        Err(error) => {
            show_error(owner, "Could not read credential file", &error.to_string());
            None
        }
    }
}

unsafe fn credential_changed(state: &mut AppState, status: &str) {
    update_credential_button(state);
    set_status(state, status);
}

unsafe fn update_credential_button(state: &AppState) {
    let labels = [
        state
            .key
            .as_ref()
            .map(|value| format!("ZFS: {}", value.label)),
        state
            .container_key
            .as_ref()
            .map(|value| format!("pool: {}", value.label)),
        state
            .agent_password
            .as_ref()
            .map(|value| format!("agent: {}", value.label)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let label = if labels.is_empty() {
        "Credentials…".to_owned()
    } else {
        format!("Credentials ({} set)…", labels.len())
    };
    set_text(state.controls.choose_key, &label);
}

unsafe fn clear_credentials(state: &mut AppState) {
    clear_credentials_silent(state);
    set_status(state, "All in-memory credentials were cleared");
}

unsafe fn clear_credentials_silent(state: &mut AppState) {
    state.key = None;
    state.container_key = None;
    state.agent_password = None;
    update_credential_button(state);
}

unsafe fn clear_credentials_for_source(state: &mut AppState, source: &Path) {
    if state
        .key
        .as_ref()
        .is_some_and(|value| value.source.as_deref() != Some(source))
    {
        state.key = None;
    }
    if state
        .container_key
        .as_ref()
        .is_some_and(|value| value.source.as_deref() != Some(source))
    {
        state.container_key = None;
    }
    if state
        .agent_password
        .as_ref()
        .is_some_and(|value| value.source.as_deref() != Some(source))
    {
        state.agent_password = None;
    }
    update_credential_button(state);
}

unsafe fn credential_scope(state: &AppState) -> Option<PathBuf> {
    let path = PathBuf::from(get_text(state.controls.source_path));
    (!path.as_os_str().is_empty()).then_some(path)
}

unsafe fn ensure_selected_view_key(state: &mut AppState) {
    let Some(catalog) = &state.catalog else {
        return;
    };
    let Some(index) = selected_view(state) else {
        return;
    };
    if catalog.views.get(index).is_some_and(|view| view.encrypted) && state.key.is_none() {
        if let Some(material) = prompt_secret(
            state.hwnd,
            "ZFS dataset key",
            "This view is encrypted. Enter its passphrase or hexadecimal key, or cancel and choose a binary key file from Credentials.",
        ) {
            state.key = Some(SecretValue::new(
                material,
                "entered in memory",
                credential_scope(state),
            ));
            credential_changed(state, "ZFS key entered in protected memory");
        }
    }
}

fn source_is_changing(state: &AppState, path: &Path) -> bool {
    state
        .catalog
        .as_ref()
        .is_some_and(|catalog| catalog.path != path)
        || state.inception.first().is_some_and(|frame| {
            frame.parent_path.is_none() && frame.catalog.image_path != path.display().to_string()
        })
}

unsafe fn leave_all_images(state: &mut AppState, clear_list: bool) {
    state.inception.clear();
    SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    set_text(state.controls.image_offset, "");
    set_text(state.controls.image_length, "");
    if clear_list {
        state.entries.clear();
        populate_list(state);
    }
    update_breadcrumb(state);
}

unsafe fn close_source_ui(state: &mut AppState) {
    state.entries.clear();
    populate_list(state);
    state.current_path = "/".to_owned();
    set_text(state.controls.path, "/");
    set_text(state.hwnd, APP_TITLE);
    set_source_enabled(state, false);
    set_status(
        state,
        "Ready — open a backup, choose a physical drive, or open a disk image",
    );
    update_breadcrumb(state);
}

fn first_supported_volume(catalog: &InceptionCatalog) -> Option<usize> {
    catalog
        .volumes
        .iter()
        .position(|volume| volume.filesystem.is_some())
}

unsafe fn populate_volumes(state: &AppState, catalog: &InceptionCatalog, preferred: Option<usize>) {
    SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    for volume in &catalog.volumes {
        let label = wide(&volume.label());
        SendMessageW(
            state.controls.volume,
            CB_ADDSTRING,
            0,
            label.as_ptr() as isize,
        );
    }
    if let Some(selected) = preferred.or_else(|| first_supported_volume(catalog)) {
        SendMessageW(state.controls.volume, CB_SETCURSEL, selected, 0);
    }
}

unsafe fn update_breadcrumb(state: &AppState) {
    let mut parts = Vec::new();
    if let Some(catalog) = &state.catalog {
        parts.push(catalog.title.clone());
        if let Some(view) = selected_view(state).and_then(|index| catalog.views.get(index)) {
            parts.push(view.selector.clone());
        }
    }
    for frame in &state.inception {
        parts.push(
            Path::new(&frame.catalog.image_path)
                .file_name()
                .map_or_else(
                    || frame.catalog.image_path.clone(),
                    |name| name.to_string_lossy().into_owned(),
                ),
        );
    }
    if !state.current_path.is_empty() {
        parts.push(state.current_path.clone());
    }
    set_text(
        state.controls.agent_password,
        &if parts.is_empty() {
            "Source > choose a view > browse files".to_owned()
        } else {
            parts.join("  ›  ")
        },
    );
}

unsafe fn toggle_setting(state: &mut AppState, id: u16) {
    match id {
        ID_SETTING_AUTO_IMAGES => state.settings.auto_open_images ^= true,
        ID_SETTING_CLEAR_CREDENTIALS => state.settings.clear_credentials_on_source_change ^= true,
        ID_SETTING_CONFIRM_DRIVE => state.settings.confirm_physical_drive ^= true,
        _ => return,
    }
    update_settings_menu(state);
    if let Err(error) = save_settings(&state.settings) {
        show_error(state.hwnd, "Could not save settings", &error.to_string());
    } else {
        set_status(state, "Settings saved");
    }
}

unsafe fn update_settings_menu(state: &AppState) {
    let menu = GetMenu(state.hwnd);
    for (id, checked) in [
        (ID_SETTING_AUTO_IMAGES, state.settings.auto_open_images),
        (
            ID_SETTING_CLEAR_CREDENTIALS,
            state.settings.clear_credentials_on_source_change,
        ),
        (
            ID_SETTING_CONFIRM_DRIVE,
            state.settings.confirm_physical_drive,
        ),
    ] {
        CheckMenuItem(
            menu,
            id as u32,
            MF_BYCOMMAND | if checked { MF_CHECKED } else { MF_UNCHECKED },
        );
    }
}

fn settings_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("ZFS Send Explorer").join("settings.json"))
}

fn load_settings() -> UiSettings {
    settings_path()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &UiSettings) -> Result<()> {
    let Some(path) = settings_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(settings)?)?;
    Ok(())
}

unsafe fn create_accelerators() -> Result<HACCEL> {
    let mut entries = [
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL,
            key: b'O' as u16,
            cmd: ID_OPEN_AUTO,
        },
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL | FSHIFT,
            key: b'O' as u16,
            cmd: ID_OPEN_IMAGE,
        },
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL,
            key: b'D' as u16,
            cmd: ID_OPEN_POOL,
        },
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL,
            key: b'K' as u16,
            cmd: ID_ENTER_KEY,
        },
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL,
            key: b'E' as u16,
            cmd: ID_EXTRACT,
        },
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL,
            key: b'I' as u16,
            cmd: ID_INCEPTION,
        },
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL,
            key: b'U' as u16,
            cmd: ID_UPDATE,
        },
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL,
            key: b'L' as u16,
            cmd: ID_FOCUS_PATH,
        },
        ACCEL {
            fVirt: FVIRTKEY | FALT,
            key: 0x26,
            cmd: ID_UP,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: 0x74,
            cmd: ID_REFRESH,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: 0x1b,
            cmd: ID_LEAVE_IMAGE,
        },
    ];
    let table = CreateAcceleratorTableW(entries.as_mut_ptr(), entries.len() as i32);
    if table.is_null() {
        Err(std::io::Error::last_os_error()).context("creating keyboard accelerators")
    } else {
        Ok(table)
    }
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
    let send_width = scale(82);
    let source_edit_width = (width - pad * 2 - gap * 2 - browse_width - send_width).max(scale(280));
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
    let second_y = pad + row + gap;
    let drive_width = (width * 55 / 100).max(scale(360));
    MoveWindow(
        state.controls.container_key,
        pad,
        second_y,
        drive_width,
        scale(260),
        1,
    );
    x = pad + drive_width + gap;
    MoveWindow(state.controls.open_pool, x, second_y, scale(104), row, 1);
    x += scale(104) + gap;
    MoveWindow(state.controls.choose_key, x, second_y, scale(132), row, 1);

    let third_y = second_y + row + gap;
    let view_width = (width * 44 / 100).max(scale(300));
    let up_width = scale(54);
    let go_width = scale(54);
    let path_width = (width - pad * 2 - gap * 3 - view_width - up_width - go_width).max(scale(160));
    x = pad;
    MoveWindow(state.controls.view, x, third_y, view_width, scale(240), 1);
    x += view_width + gap;
    MoveWindow(state.controls.up, x, third_y, up_width, row, 1);
    x += up_width + gap;
    MoveWindow(state.controls.path, x, third_y, path_width, row, 1);
    x += path_width + gap;
    MoveWindow(state.controls.go, x, third_y, go_width, row, 1);

    let breadcrumb_y = third_y + row + scale(4);
    MoveWindow(
        state.controls.agent_password,
        pad,
        breadcrumb_y,
        width - pad * 2,
        scale(22),
        1,
    );

    let image_y = breadcrumb_y + scale(22) + scale(4);
    let volume_width = (width * 36 / 100).max(scale(260));
    let offset_width = scale(116);
    let length_width = scale(154);
    let inception_width = scale(156);
    let leave_width = scale(126);
    x = pad;
    MoveWindow(
        state.controls.volume,
        x,
        image_y,
        volume_width,
        scale(240),
        1,
    );
    x += volume_width + gap;
    MoveWindow(
        state.controls.image_offset,
        x,
        image_y,
        offset_width,
        row,
        1,
    );
    x += offset_width + gap;
    MoveWindow(
        state.controls.image_length,
        x,
        image_y,
        length_width,
        row,
        1,
    );
    x += length_width + gap;
    MoveWindow(
        state.controls.inception,
        x,
        image_y,
        inception_width,
        row,
        1,
    );
    x += inception_width + gap;
    MoveWindow(state.controls.leave_image, x, image_y, leave_width, row, 1);

    let actions_y = height - status_height - pad - row;
    let list_y = image_y + row + gap;
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
    Image,
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
        SourceFilter::Image => wide_nul_groups(&[
            "Disk images",
            "*.raw;*.img;*.dd;*.qcow;*.qcow2;*.vmdk;*.datto;*.detto",
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

unsafe fn show_confirmation(owner: HWND, title: &str, message: &str) -> bool {
    let title = wide(title);
    let message = wide(message);
    MessageBoxW(
        owner,
        message.as_ptr(),
        title.as_ptr(),
        MB_OKCANCEL | MB_ICONWARNING,
    ) == 1
}

unsafe fn show_shortcuts(owner: HWND) {
    show_information(
        owner,
        "Keyboard shortcuts",
        "Ctrl+O  Open a backup or disk image\nCtrl+Shift+O  Open a standalone disk image\nCtrl+D  Open the selected physical drive\nCtrl+K  Enter a ZFS key or passphrase\nCtrl+L  Focus the path box\nAlt+Up  Go to the parent folder\nEnter  Open a folder or recognized disk image\nBackspace  Go to the parent folder\nCtrl+I  Explore the selected file as a disk image\nEsc  Go back one image layer\nCtrl+E  Extract the selected file or folder\nCtrl+U  Update a previously extracted file\nF5  Refresh the directory and physical-drive list",
    );
}

unsafe fn show_about(owner: HWND) {
    show_information(
        owner,
        "About ZFS Send Explorer",
        "Browse snapshots in ZFS send files, Slide Boxes, Datto Reverse RoundTrip drives, exported pool members, and standalone disk images without importing or mounting them. Recursive inception mode follows raw, .datto, authenticated .detto, QCOW2, and self-contained sparse VMDK images through NTFS, FAT, exFAT, and ext filesystems.\n\nSources are always opened read-only. Entered credentials remain in zeroizing process memory and are never saved by the app. File and staged folder extraction plus validated incremental updates preserve sparse ranges when the destination filesystem supports them.",
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

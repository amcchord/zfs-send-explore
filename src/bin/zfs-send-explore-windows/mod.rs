#![allow(unsafe_op_in_unsafe_fn)]

mod dialogs;

use anyhow::{Context, Result, bail};
use dialogs::{
    DialogIcon, Form, FormResult, Task, form_dialog, show_message, show_problem, task_dialog,
};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString, c_void};
use std::fs;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::thread;
use windows_sys::Win32::Foundation::{
    CloseHandle, HINSTANCE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DeleteObject,
    EndPaint, FW_NORMAL, FW_SEMIBOLD, FillRect, HBRUSH, HDC, HFONT, InvalidateRect, PAINTSTRUCT,
    SetBkColor, SetTextColor, UpdateWindow,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    GET_LENGTH_INFORMATION, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_LENGTH_INFO,
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::WindowsProgramming::MulDiv;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{
    BCM_SETNOTE, BS_COMMANDLINK, ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCFMT_LEFT,
    LVCFMT_RIGHT, LVCOLUMNW, LVIF_IMAGE, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW,
    LVM_DELETEALLITEMS, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETIMAGELIST, LVM_SETITEMSTATE, LVM_SETITEMTEXTW,
    LVN_ITEMCHANGED, LVN_KEYDOWN, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_EX_LABELTIP, LVSIL_SMALL, NM_DBLCLK, NMHDR, NMLVKEYDOWN, SB_SETTEXTW, STATUSCLASSNAMEW,
    SetWindowTheme, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_BACK, VK_RETURN};
use windows_sys::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_SMALLICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGSI_SMALLICON,
    SHGSI_SYSICONINDEX, SHGetFileInfoW, SHGetStockIconInfo, SHSTOCKICONINFO, SIID_DRIVEFIXED,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ACCEL, AppendMenuW, BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, CheckMenuItem, CreateAcceleratorTableW, CreateMenu, CreatePopupMenu,
    CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyAcceleratorTable, DestroyWindow,
    DispatchMessageW, DrawIconEx, ES_AUTOHSCROLL, FCONTROL, FSHIFT, FVIRTKEY, GWLP_USERDATA,
    GetClientRect, GetMenu, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, HACCEL, HICON, HMENU, IDC_ARROW, IMAGE_ICON,
    IsDialogMessageW, LR_SHARED, LoadCursorW, LoadImageW, MB_ICONERROR, MB_OK, MF_BYCOMMAND,
    MF_CHECKED, MF_ENABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MINMAXINFO,
    MSG, MessageBoxW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW, SM_CXICON,
    SM_CXSMICON, SM_CYICON, SM_CYSMICON, SPI_GETWORKAREA, SW_HIDE, SW_SHOW, SendMessageW, SetMenu,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, SystemParametersInfoW,
    TranslateAcceleratorW, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_GETMINMAXINFO, WM_NCCREATE,
    WM_NOTIFY, WM_PAINT, WM_PRINTCLIENT, WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WM_TIMECHANGE,
    WNDCLASSEXW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CONTROLPARENT, WS_OVERLAPPEDWINDOW,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use zeroize::{Zeroize, Zeroizing};
use zfs_send_extract::client::{
    ClientExtraction, InceptionCatalog, SourceCatalog, apply_incremental_with_key_material,
    child_path, parent_path,
};
use zfs_send_extract::filesystem::DirectoryEntry;
use zfs_send_extract::operations::Sidecar;
use zfs_send_extract::presentation;
use zfs_send_extract::snapshot_time::SnapshotTimeZone;
use zfs_send_extract::tree::RecursiveExtraction;

const CLASS_NAME: &str = "ZfsSendExploreWindows";
const APP_TITLE: &str = "ZFS Explore";
// Matches the icon group embedded by packaging/windows/zfs-send-explore.rc.
const IDI_APP: usize = 101;

const ID_SOURCE_PATH: u16 = 100;
const ID_OPEN_BACKUP: u16 = 101;
const ID_OPEN_PATH: u16 = 102;
const ID_OPEN_DRIVE: u16 = 103;
const ID_VIEW: u16 = 104;
const ID_PATH: u16 = 105;
const ID_UP: u16 = 106;
const ID_GO: u16 = 107;
const ID_CHOOSE_KEY: u16 = 108;
const ID_LIST: u16 = 109;
const ID_RESTORE: u16 = 110;
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
const ID_UNLOCK: u16 = 134;
const ID_CLOSE_SOURCE: u16 = 135;
const ID_SETTING_ADVANCED_IMAGE_RANGE: u16 = 136;
const ID_GUIDE_ACTION: u16 = 137;
const ID_HOW_TO: u16 = 138;
const ID_WELCOME_FILE: u16 = 140;
const ID_WELCOME_DRIVE: u16 = 141;
const ID_WELCOME_PATH: u16 = 142;
const ID_TIME_ZONE_BASE: u16 = 10_000;

const WM_JOB_COMPLETE: u32 = WM_APP + 1;

// Static control styles (System Services constants, defined here to avoid a feature).
const SS_NOPREFIX: u32 = 0x0080;
const SS_ENDELLIPSIS: u32 = 0x4000;
const SS_CENTER: u32 = 0x0001;
const LVS_REPORT: u32 = 0x0001;
const LVS_SINGLESEL: u32 = 0x0004;
const LVS_SHOWSELALWAYS: u32 = 0x0008;
const LVS_SHAREIMAGELISTS: u32 = 0x0040;

const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | (green as u32) << 8 | (blue as u32) << 16
}
pub(crate) const TEXT_COLOR: u32 = rgb(0x1b, 0x1f, 0x23);
const MUTED_COLOR: u32 = rgb(0x5b, 0x66, 0x70);
const ACCENT_COLOR: u32 = rgb(0x00, 0x7c, 0x89);
const GUIDE_COLOR: u32 = rgb(0xe4, 0xf4, 0xf6);
const DIVIDER_COLOR: u32 = rgb(0xe1, 0xe5, 0xea);
const WHITE_COLOR: u32 = rgb(0xff, 0xff, 0xff);

/// Fonts for one DPI. Recreated when the window moves to another monitor.
#[derive(Default)]
pub(crate) struct Fonts {
    pub body: HFONT,
    pub strong: HFONT,
    pub title: HFONT,
    pub heading: HFONT,
}

impl Fonts {
    pub(crate) unsafe fn create(dpi: i32) -> Self {
        let font = |points: i32, weight: i32| {
            CreateFontW(
                -MulDiv(points, dpi, 72),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET.into(),
                0,
                0,
                5,
                0,
                wide("Segoe UI").as_ptr(),
            )
        };
        Self {
            body: font(10, FW_NORMAL as i32),
            strong: font(10, FW_SEMIBOLD as i32),
            title: font(14, FW_SEMIBOLD as i32),
            heading: font(22, FW_SEMIBOLD as i32),
        }
    }

    pub(crate) unsafe fn destroy(&mut self) {
        for font in [self.body, self.strong, self.title, self.heading] {
            if !font.is_null() {
                DeleteObject(font);
            }
        }
        *self = Self::default();
    }
}

#[derive(Default)]
struct Controls {
    /// Hidden: the path of the source being opened or last opened.
    source_path: HWND,
    header_title: HWND,
    header_detail: HWND,
    open_backup: HWND,
    open_drive: HWND,
    close_source: HWND,
    welcome_heading: HWND,
    welcome_body: HWND,
    welcome_file: HWND,
    welcome_drive: HWND,
    welcome_path: HWND,
    welcome_steps: HWND,
    view_label: HWND,
    view: HWND,
    volume_label: HWND,
    volume: HWND,
    leave_image: HWND,
    up: HWND,
    path: HWND,
    go: HWND,
    breadcrumb: HWND,
    guide_text: HWND,
    guide_action: HWND,
    image_range_label: HWND,
    image_offset: HWND,
    image_length: HWND,
    list: HWND,
    selection: HWND,
    inception: HWND,
    restore: HWND,
    status: HWND,
}

/// The next step the guidance bar suggests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Guide {
    None,
    Unlock,
    OpenImage(usize),
    ChooseVolume,
}

struct AppState {
    hwnd: HWND,
    controls: Controls,
    fonts: Fonts,
    white_brush: HBRUSH,
    guide_brush: HBRUSH,
    divider_brush: HBRUSH,
    accent_brush: HBRUSH,
    app_icon: HICON,
    dpi: i32,
    header_height: Cell<i32>,
    guide_rect: Cell<RECT>,
    guide: Cell<Guide>,
    icon_cache: RefCell<HashMap<String, i32>>,
    catalog: Option<SourceCatalog>,
    entries: Vec<DirectoryEntry>,
    current_path: String,
    keys: Vec<SecretValue>,
    container_key: Option<SecretValue>,
    agent_password: Option<SecretValue>,
    inception: Vec<ImageFrame>,
    settings: UiSettings,
    busy: bool,
    pending_job: Option<PendingJob>,
    /// Friendly name of the physical drive chosen in the drive picker.
    drive_title: Option<(PathBuf, String)>,
}

#[derive(Clone)]
struct SecretValue {
    material: std::sync::Arc<Zeroizing<Vec<u8>>>,
    source: Option<PathBuf>,
    view: Option<String>,
}

impl SecretValue {
    fn new(material: Vec<u8>, source: Option<PathBuf>, view: Option<String>) -> Self {
        Self {
            material: std::sync::Arc::new(Zeroizing::new(material)),
            source,
            view,
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
    number: u32,
    model: String,
    size: Option<u64>,
    removable: bool,
    system: bool,
}

impl PhysicalDrive {
    fn title(&self) -> String {
        format!(
            "Disk {} · {} · {}",
            self.number,
            self.model,
            self.size
                .map_or_else(|| "unknown size".to_owned(), presentation::format_size)
        )
    }

    fn note(&self) -> String {
        let kind = if self.system {
            "This computer’s Windows disk"
        } else if self.removable {
            "Removable drive"
        } else {
            "Fixed or USB drive"
        };
        format!("{kind} · {}", self.path.display())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct UiSettings {
    auto_open_images: bool,
    clear_credentials_on_source_change: bool,
    confirm_physical_drive: bool,
    show_advanced_image_range: bool,
    snapshot_time_zone: String,
    window_width: i32,
    window_height: i32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            auto_open_images: true,
            clear_credentials_on_source_change: true,
            confirm_physical_drive: true,
            show_advanced_image_range: false,
            snapshot_time_zone: "local".into(),
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
            fonts: Fonts::default(),
            white_brush: null_mut(),
            guide_brush: null_mut(),
            divider_brush: null_mut(),
            accent_brush: null_mut(),
            app_icon: null_mut(),
            dpi: 96,
            header_height: Cell::new(0),
            guide_rect: Cell::new(RECT::default()),
            guide: Cell::new(Guide::None),
            icon_cache: RefCell::new(HashMap::new()),
            catalog: None,
            entries: Vec::new(),
            current_path: "/".to_owned(),
            keys: Vec::new(),
            container_key: None,
            agent_password: None,
            inception: Vec::new(),
            settings: load_settings(),
            busy: false,
            pending_job: None,
            drive_title: None,
        }
    }
}

#[derive(Clone, Copy)]
enum PendingJob {
    Open(OpenMode),
    Browse,
    Extract,
    ExploreImage,
    Update,
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
    trace_startup("run_ui: begin");
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    // Shell icon lookups require COM on the UI thread.
    CoInitializeEx(null(), COINIT_APARTMENTTHREADED as _);
    let common = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
    };
    if InitCommonControlsEx(&common) == 0 {
        bail!("Windows common controls could not be initialized");
    }
    trace_startup("run_ui: common controls initialized");

    let instance = GetModuleHandleW(null());
    if instance.is_null() {
        return Err(std::io::Error::last_os_error()).context("getting application module");
    }
    let class_name = wide(CLASS_NAME);
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hIcon: LoadImageW(
            instance,
            IDI_APP as _,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXICON),
            GetSystemMetrics(SM_CYICON),
            LR_SHARED,
        ) as _,
        hIconSm: LoadImageW(
            instance,
            IDI_APP as _,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_SHARED,
        ) as _,
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_WINDOW as usize + 1) as _,
        lpszClassName: class_name.as_ptr(),
        ..zeroed()
    };
    if class.hIcon.is_null() || class.hIconSm.is_null() {
        return Err(std::io::Error::last_os_error()).context("loading application icons");
    }
    if RegisterClassExW(&class) == 0 {
        return Err(std::io::Error::last_os_error()).context("registering window class");
    }

    let state = Box::into_raw(Box::new(AppState::default()));
    // Fit the saved size inside the screen's work area, above the taskbar.
    let mut work = RECT::default();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
    let work_width = (work.right - work.left).max(800);
    let work_height = (work.bottom - work.top).max(600);
    let width = (*state).settings.window_width.max(900).min(work_width);
    let height = (*state).settings.window_height.max(620).min(work_height);
    let title = wide(APP_TITLE);
    trace_startup("run_ui: creating main window");
    let hwnd = CreateWindowExW(
        WS_EX_CONTROLPARENT,
        class_name.as_ptr(),
        title.as_ptr(),
        WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
        work.left + (work_width - width) / 2,
        work.top + (work_height - height) / 2,
        width,
        height,
        null_mut(),
        null_mut(),
        instance,
        state.cast::<c_void>(),
    );
    if hwnd.is_null() {
        drop(Box::from_raw(state));
        return Err(std::io::Error::last_os_error()).context("creating main window");
    }
    trace_startup("run_ui: main window created");
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
        let title = wide("ZFS Explore could not start");
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
            trace_startup("window: initializing controls");
            if let Err(error) = initialize_window(&mut *state) {
                show_problem(
                    hwnd,
                    &format!("Could not create the application window: {error:#}"),
                );
                return -1;
            }
            trace_startup("window: controls initialized");
            0
        }
        WM_SIZE if !state.is_null() => {
            layout(&*state);
            0
        }
        WM_GETMINMAXINFO if !state.is_null() => {
            let info = &mut *(lparam as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            info.ptMinTrackSize.x = 860 * dpi / 96;
            info.ptMinTrackSize.y = 600 * dpi / 96;
            0
        }
        WM_DPICHANGED => {
            if !state.is_null() {
                apply_dpi(&mut *state, (wparam & 0xffff) as i32);
            }
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
        WM_PAINT if !state.is_null() => {
            let mut paint = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut paint);
            paint_background(&*state, hdc);
            EndPaint(hwnd, &paint);
            0
        }
        WM_PRINTCLIENT if !state.is_null() => {
            // Themed buttons ask their parent to draw what lies behind them.
            paint_background(&*state, wparam as HDC);
            0
        }
        WM_CTLCOLORSTATIC if !state.is_null() => {
            color_static(&*state, wparam as HDC, lparam as HWND)
        }
        WM_TIMECHANGE | WM_SETTINGCHANGE if !state.is_null() => {
            update_snapshot_labels(&*state);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_COMMAND if !state.is_null() => {
            handle_command(&mut *state, wparam, lparam);
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
                let state = &mut *state;
                let mut window = RECT::default();
                if GetWindowRect(hwnd, &mut window) != 0 {
                    state.settings.window_width = window.right - window.left;
                    state.settings.window_height = window.bottom - window.top;
                }
                let _ = save_settings(&state.settings);
                state.fonts.destroy();
                for brush in [
                    state.white_brush,
                    state.guide_brush,
                    state.divider_brush,
                    state.accent_brush,
                ] {
                    if !brush.is_null() {
                        DeleteObject(brush);
                    }
                }
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_background(state: &AppState, hdc: HDC) {
    let mut client = RECT::default();
    GetClientRect(state.hwnd, &mut client);
    let header = state.header_height.get();
    let scale = |value: i32| value * state.dpi / 96;
    if !state.app_icon.is_null() {
        DrawIconEx(
            hdc,
            scale(18),
            (header - scale(36)) / 2,
            state.app_icon,
            scale(36),
            scale(36),
            0,
            null_mut(),
            DI_NORMAL,
        );
    }
    let divider = RECT {
        left: 0,
        top: header - 1,
        right: client.right,
        bottom: header,
    };
    FillRect(hdc, &divider, state.divider_brush);
    if state.guide.get() != Guide::None {
        let guide = state.guide_rect.get();
        FillRect(hdc, &guide, state.guide_brush);
        let bar = RECT {
            right: guide.left + scale(4),
            ..guide
        };
        FillRect(hdc, &bar, state.accent_brush);
    }
}

unsafe fn color_static(state: &AppState, hdc: HDC, control: HWND) -> LRESULT {
    let controls = &state.controls;
    if control == controls.guide_text {
        SetTextColor(hdc, TEXT_COLOR);
        SetBkColor(hdc, GUIDE_COLOR);
        return state.guide_brush as LRESULT;
    }
    let muted = [
        controls.header_detail,
        controls.welcome_body,
        controls.welcome_steps,
        controls.breadcrumb,
        controls.selection,
    ];
    SetTextColor(
        hdc,
        if muted.contains(&control) {
            MUTED_COLOR
        } else {
            TEXT_COLOR
        },
    );
    SetBkColor(hdc, WHITE_COLOR);
    state.white_brush as LRESULT
}

unsafe fn initialize_window(state: &mut AppState) -> Result<()> {
    trace_startup("initialize: begin");
    let instance = GetModuleHandleW(null());
    state.dpi = GetDpiForWindow(state.hwnd).max(96) as i32;
    state.fonts = Fonts::create(state.dpi);
    state.white_brush = CreateSolidBrush(WHITE_COLOR);
    state.guide_brush = CreateSolidBrush(GUIDE_COLOR);
    state.divider_brush = CreateSolidBrush(DIVIDER_COLOR);
    state.accent_brush = CreateSolidBrush(ACCENT_COLOR);
    load_app_icon(state);
    create_menus(state);
    trace_startup("initialize: menus created");

    let hwnd = state.hwnd;
    let static_class = windows_sys::core::w!("STATIC");
    let label = |text: &str, extra: u32| {
        control(
            hwnd,
            instance,
            static_class,
            text,
            WS_CHILD | WS_VISIBLE | SS_NOPREFIX | extra,
            0,
            0,
        )
    };
    let controls = &mut state.controls;
    controls.source_path = control(
        hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "",
        WS_CHILD,
        0,
        ID_SOURCE_PATH,
    );
    controls.header_title = label(APP_TITLE, SS_ENDELLIPSIS);
    controls.header_detail = label(
        "Recover files from ZFS, Slide and Datto backups",
        SS_ENDELLIPSIS,
    );
    controls.open_backup = button(hwnd, instance, "Open backup…", ID_OPEN_BACKUP, false);
    controls.open_drive = button(hwnd, instance, "Open drive…", ID_OPEN_DRIVE, false);
    controls.close_source = button(hwnd, instance, "Close", ID_CLOSE_SOURCE, false);

    controls.welcome_heading = label("Get your files back", 0);
    controls.welcome_body = label(
        "Open a backup, pick the date you want, and save copies of the files you need. ZFS Explore only reads your backup. It never changes it.",
        0,
    );
    controls.welcome_file = command_link(
        hwnd,
        instance,
        "Open a backup file",
        "A ZFS send file, a Slide or ZFS pool image, or a RAW, QCOW2 or VMDK disk",
        ID_WELCOME_FILE,
    );
    controls.welcome_drive = command_link(
        hwnd,
        instance,
        "Open a connected drive",
        "A drive from a Slide or Datto device. Windows can't show these drives in File Explorer.",
        ID_WELCOME_DRIVE,
    );
    controls.welcome_path = command_link(
        hwnd,
        instance,
        "Open from a path",
        "Type or paste the location of a backup, or a drive such as \\\\.\\PhysicalDrive1",
        ID_WELCOME_PATH,
    );
    controls.welcome_steps = label(
        "1  Open your backup      ›      2  Pick a restore point      ›      3  Restore your files",
        SS_CENTER,
    );

    controls.view_label = label("Restore point:", 0);
    controls.view = control(
        hwnd,
        instance,
        windows_sys::core::w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        0,
        ID_VIEW,
    );
    controls.volume_label = label("Part of the disk:", 0);
    controls.volume = control(
        hwnd,
        instance,
        windows_sys::core::w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        0,
        ID_VOLUME,
    );
    controls.leave_image = button(hwnd, instance, "Back", ID_LEAVE_IMAGE, false);
    controls.up = button(hwnd, instance, "↑  Up", ID_UP, false);
    controls.path = control(
        hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "/",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
        0,
        ID_PATH,
    );
    controls.go = button(hwnd, instance, "Go", ID_GO, false);
    controls.breadcrumb = label("", SS_ENDELLIPSIS);
    controls.guide_text = label("", SS_ENDELLIPSIS);
    controls.guide_action = button(hwnd, instance, "", ID_GUIDE_ACTION, false);
    controls.image_range_label = label("Selected file range:", 0);
    controls.image_offset = control(
        hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
        0,
        ID_IMAGE_OFFSET,
    );
    controls.image_length = control(
        hwnd,
        instance,
        windows_sys::core::w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
        0,
        ID_IMAGE_LENGTH,
    );
    controls.list = control(
        hwnd,
        instance,
        WC_LISTVIEWW,
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_BORDER
            | LVS_REPORT
            | LVS_SINGLESEL
            | LVS_SHOWSELALWAYS
            | LVS_SHAREIMAGELISTS,
        0,
        ID_LIST,
    );
    controls.selection = label("", SS_ENDELLIPSIS);
    controls.inception = button(hwnd, instance, "Open disk image", ID_INCEPTION, false);
    controls.restore = button(hwnd, instance, "Restore…", ID_RESTORE, true);
    controls.status = control(
        hwnd,
        instance,
        STATUSCLASSNAMEW,
        "Ready",
        WS_CHILD | WS_VISIBLE,
        0,
        0,
    );
    trace_startup("initialize: controls created");

    for hwnd in all_controls(&state.controls) {
        if hwnd.is_null() {
            bail!("a native Windows control could not be created");
        }
    }
    apply_fonts(state);
    trace_startup("initialize: controls validated");
    let controls = &state.controls;
    let offset_cue = wide("Offset (0 default)");
    SendMessageW(
        controls.image_offset,
        0x1501, // EM_SETCUEBANNER
        0,
        offset_cue.as_ptr() as isize,
    );
    let length_cue = wide("Length (optional)");
    SendMessageW(
        controls.image_length,
        0x1501,
        0,
        length_cue.as_ptr() as isize,
    );
    SetWindowTheme(controls.list, windows_sys::core::w!("Explorer"), null());
    SendMessageW(
        controls.list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
    );
    let mut shell = SHFILEINFOW::default();
    let images = SHGetFileInfoW(
        windows_sys::core::w!("C:\\"),
        0,
        &mut shell,
        size_of::<SHFILEINFOW>() as u32,
        SHGFI_SYSICONINDEX | SHGFI_SMALLICON,
    );
    if images != 0 {
        SendMessageW(
            controls.list,
            LVM_SETIMAGELIST,
            LVSIL_SMALL as usize,
            images as isize,
        );
    }
    add_column(controls.list, 0, "Name", 480, LVCFMT_LEFT);
    add_column(controls.list, 1, "Type", 150, LVCFMT_LEFT);
    add_column(controls.list, 2, "Size", 120, LVCFMT_RIGHT);
    update_settings_menu(state);
    set_source_enabled(state, false);
    update_mode(state);
    trace_startup("initialize: layout complete");
    Ok(())
}

unsafe fn load_app_icon(state: &mut AppState) {
    let size = 36 * state.dpi / 96;
    state.app_icon = LoadImageW(
        GetModuleHandleW(null()),
        IDI_APP as _,
        IMAGE_ICON,
        size,
        size,
        LR_SHARED,
    ) as HICON;
}

unsafe fn apply_fonts(state: &AppState) {
    let controls = &state.controls;
    for hwnd in all_controls(controls) {
        SendMessageW(hwnd, WM_SETFONT, state.fonts.body as usize, 1);
    }
    SendMessageW(
        controls.header_title,
        WM_SETFONT,
        state.fonts.title as usize,
        1,
    );
    SendMessageW(
        controls.welcome_heading,
        WM_SETFONT,
        state.fonts.heading as usize,
        1,
    );
    for hwnd in [
        controls.view_label,
        controls.volume_label,
        controls.welcome_steps,
    ] {
        SendMessageW(hwnd, WM_SETFONT, state.fonts.strong as usize, 1);
    }
}

unsafe fn apply_dpi(state: &mut AppState, dpi: i32) {
    state.dpi = dpi.max(96);
    state.fonts.destroy();
    state.fonts = Fonts::create(state.dpi);
    load_app_icon(state);
    apply_fonts(state);
    layout(state);
}

unsafe fn create_menus(state: &AppState) {
    let menu = CreateMenu();
    let file_menu = CreateMenu();
    let restore_menu = CreateMenu();
    let keys_menu = CreateMenu();
    let settings_menu = CreateMenu();
    let help_menu = CreateMenu();
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_BACKUP as usize,
        "Open backup file…\tCtrl+O",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_DRIVE as usize,
        "Open connected drive…\tCtrl+D",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_IMAGE as usize,
        "Open disk image file…\tCtrl+Shift+O",
    );
    append_menu(
        file_menu,
        MF_STRING,
        ID_OPEN_PATH as usize,
        "Open from a path…",
    );
    AppendMenuW(file_menu, MF_SEPARATOR, 0, null());
    append_menu(
        file_menu,
        MF_STRING,
        ID_CLOSE_SOURCE as usize,
        "Close backup\tCtrl+W",
    );
    append_menu(file_menu, MF_STRING, ID_REFRESH as usize, "Refresh\tF5");
    AppendMenuW(file_menu, MF_SEPARATOR, 0, null());
    append_menu(file_menu, MF_STRING, ID_EXIT as usize, "Exit");

    append_menu(
        restore_menu,
        MF_STRING,
        ID_RESTORE as usize,
        "Restore selected item…\tCtrl+S",
    );
    append_menu(
        restore_menu,
        MF_STRING,
        ID_INCEPTION as usize,
        "Open selected file as a disk image\tCtrl+I",
    );
    append_menu(
        restore_menu,
        MF_STRING,
        ID_LEAVE_IMAGE as usize,
        "Leave this disk image\tEsc",
    );
    append_menu(
        restore_menu,
        MF_STRING,
        ID_FOCUS_PATH as usize,
        "Go to a folder…\tCtrl+L",
    );
    AppendMenuW(restore_menu, MF_SEPARATOR, 0, null());
    append_menu(
        restore_menu,
        MF_STRING,
        ID_UPDATE as usize,
        "Update a restored file from an incremental send…\tCtrl+U",
    );

    append_menu(
        keys_menu,
        MF_STRING,
        ID_UNLOCK as usize,
        "Unlock this restore point…\tCtrl+K",
    );
    append_menu(
        keys_menu,
        MF_STRING,
        ID_CHOOSE_KEY as usize,
        "Choose a key file…",
    );
    AppendMenuW(keys_menu, MF_SEPARATOR, 0, null());
    append_menu(
        keys_menu,
        MF_STRING,
        ID_ENTER_CONTAINER_KEY as usize,
        "Enter Datto drive passphrase…",
    );
    append_menu(
        keys_menu,
        MF_STRING,
        ID_CONTAINER_KEY as usize,
        "Choose Datto drive passphrase file…",
    );
    append_menu(
        keys_menu,
        MF_STRING,
        ID_ENTER_AGENT_PASSWORD as usize,
        "Enter Datto agent password…",
    );
    append_menu(
        keys_menu,
        MF_STRING,
        ID_AGENT_PASSWORD as usize,
        "Choose Datto agent password file…",
    );
    AppendMenuW(keys_menu, MF_SEPARATOR, 0, null());
    append_menu(
        keys_menu,
        MF_STRING,
        ID_CLEAR_CREDENTIALS as usize,
        "Forget all keys",
    );

    append_menu(
        settings_menu,
        MF_STRING,
        ID_SETTING_AUTO_IMAGES as usize,
        "Open disk images on double-click",
    );
    append_menu(
        settings_menu,
        MF_STRING,
        ID_SETTING_CLEAR_CREDENTIALS as usize,
        "Forget keys when opening another backup",
    );
    append_menu(
        settings_menu,
        MF_STRING,
        ID_SETTING_CONFIRM_DRIVE as usize,
        "Confirm before opening a typed drive path",
    );
    append_menu(
        settings_menu,
        MF_STRING,
        ID_SETTING_ADVANCED_IMAGE_RANGE as usize,
        "Show advanced disk-image range fields",
    );
    add_time_zone_menu(settings_menu);
    append_menu(
        help_menu,
        MF_STRING,
        ID_HOW_TO as usize,
        "How to restore files",
    );
    append_menu(
        help_menu,
        MF_STRING,
        ID_SHORTCUTS as usize,
        "Keyboard shortcuts",
    );
    AppendMenuW(help_menu, MF_SEPARATOR, 0, null());
    append_menu(help_menu, MF_STRING, ID_ABOUT as usize, "About ZFS Explore");
    append_menu(menu, MF_POPUP, file_menu as usize, "&File");
    append_menu(menu, MF_POPUP, restore_menu as usize, "&Restore");
    append_menu(menu, MF_POPUP, keys_menu as usize, "&Keys");
    append_menu(menu, MF_POPUP, settings_menu as usize, "&Settings");
    append_menu(menu, MF_POPUP, help_menu as usize, "&Help");
    SetMenu(state.hwnd, menu);
}

unsafe fn handle_command(state: &mut AppState, wparam: WPARAM, _lparam: LPARAM) {
    let id = (wparam & 0xffff) as u16;
    let notification = ((wparam >> 16) & 0xffff) as u16;
    if state.busy && !matches!(id, ID_EXIT | ID_ABOUT) {
        return;
    }
    match id {
        ID_OPEN_BACKUP | ID_WELCOME_FILE => choose_source(state, OpenMode::Auto),
        ID_OPEN_DRIVE | ID_WELCOME_DRIVE => open_physical_drive(state),
        ID_OPEN_PATH | ID_WELCOME_PATH => open_path_dialog(state),
        ID_OPEN_AUTO => open_source(state, OpenMode::Auto),
        ID_OPEN_IMAGE => choose_source(state, OpenMode::Image),
        ID_CLOSE_SOURCE => close_source(state),
        ID_VIEW if notification == CBN_SELCHANGE as u16 => {
            leave_all_images(state, false);
            clear_directory_ui(state);
            if ensure_selected_view_key(state) {
                browse(state, "/".to_owned());
            }
            update_guidance(state);
        }
        ID_VOLUME if notification == CBN_SELCHANGE as u16 => browse(state, "/".to_owned()),
        ID_GO => browse(state, normalize_path(&get_text(state.controls.path))),
        // Enter outside a button arrives as IDOK because this is not a dialog.
        1 => {
            let focus = windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus();
            if focus == state.controls.path {
                browse(state, normalize_path(&get_text(state.controls.path)));
            } else if focus == state.controls.list {
                open_selected(state);
            }
        }
        ID_UP => browse(state, parent_path(&state.current_path)),
        ID_FOCUS_PATH => {
            SetFocus(state.controls.path);
            SendMessageW(state.controls.path, 0x00B1, 0, -1); // EM_SETSEL: select all
        }
        ID_REFRESH => refresh(state),
        ID_UNLOCK => show_contextual_credentials(state),
        ID_GUIDE_ACTION if notification == BN_CLICKED as u16 => run_guide_action(state),
        ID_ENTER_KEY => {
            if enter_key(state) {
                refresh_after_zfs_key(state);
            }
        }
        ID_CHOOSE_KEY => {
            if choose_key_file(state) {
                refresh_after_zfs_key(state);
            }
        }
        ID_ENTER_CONTAINER_KEY => {
            enter_container_key(state);
        }
        ID_CONTAINER_KEY => {
            choose_container_key_file(state);
        }
        ID_ENTER_AGENT_PASSWORD => {
            enter_agent_password(state);
        }
        ID_AGENT_PASSWORD => {
            choose_agent_password_file(state);
        }
        ID_CLEAR_CREDENTIALS => clear_credentials(state),
        ID_RESTORE => extract_selected(state),
        ID_INCEPTION => explore_selected_image(state),
        ID_OPEN_SELECTED => open_selected(state),
        ID_LEAVE_IMAGE => leave_inception(state, true),
        ID_UPDATE => update_extracted_file(state),
        ID_SETTING_AUTO_IMAGES => toggle_setting(state, ID_SETTING_AUTO_IMAGES),
        ID_SETTING_CLEAR_CREDENTIALS => toggle_setting(state, ID_SETTING_CLEAR_CREDENTIALS),
        ID_SETTING_CONFIRM_DRIVE => toggle_setting(state, ID_SETTING_CONFIRM_DRIVE),
        ID_SETTING_ADVANCED_IMAGE_RANGE => toggle_setting(state, ID_SETTING_ADVANCED_IMAGE_RANGE),
        id if id >= ID_TIME_ZONE_BASE
            && usize::from(id - ID_TIME_ZONE_BASE) < time_zone_choices().len() =>
        {
            state.settings.snapshot_time_zone =
                time_zone_choices()[usize::from(id - ID_TIME_ZONE_BASE)].clone();
            update_snapshot_labels(state);
            update_settings_menu(state);
            if let Err(error) = save_settings(&state.settings) {
                show_problem(
                    state.hwnd,
                    &format!("Could not save the time zone: {error}"),
                );
            } else {
                set_status(
                    state,
                    &format!(
                        "Restore point times are shown in {}",
                        state.settings.snapshot_time_zone
                    ),
                );
            }
        }
        ID_HOW_TO => show_how_to(state.hwnd),
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
    Image,
}

fn normalize_path(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    }
}

unsafe fn source_open(state: &AppState) -> bool {
    state.catalog.is_some() || !state.inception.is_empty()
}

unsafe fn choose_source(state: &mut AppState, mode: OpenMode) {
    let (title, filter) = match mode {
        OpenMode::Image => ("Open a disk image file", SourceFilter::Image),
        OpenMode::Auto => ("Open a backup", SourceFilter::Backup),
    };
    let Some(path) = open_file_dialog(state.hwnd, title, filter) else {
        return;
    };
    set_text(state.controls.source_path, &path.display().to_string());
    open_source(state, mode);
}

unsafe fn open_path_dialog(state: &mut AppState) {
    let current = get_text(state.controls.source_path);
    let form = Form {
        heading: "Open from a path",
        body: "Type or paste the full location of a backup file or disk image, or a drive such as \\\\.\\PhysicalDrive1. Drives open read-only.",
        field: "Location",
        initial: &current,
        password: false,
        primary: "Open",
        secondary: Some("Browse…"),
    };
    match form_dialog(state.hwnd, &form) {
        FormResult::Submitted(text) => {
            let path = String::from_utf16_lossy(&text)
                .trim()
                .trim_matches('"')
                .to_owned();
            if path.starts_with(r"\\.\")
                && state.settings.confirm_physical_drive
                && task_dialog(
                    state.hwnd,
                    &Task {
                        actions: &[(
                            "Open read-only",
                            "Nothing on the drive is written, mounted or initialized.",
                        )],
                        ..Task::new(DialogIcon::Warning, "Open this drive?", &path)
                    },
                ) != Some(0)
            {
                return;
            }
            set_text(state.controls.source_path, &path);
            open_source(state, OpenMode::Auto);
        }
        FormResult::Secondary => choose_source(state, OpenMode::Auto),
        FormResult::Cancelled => {}
    }
}

unsafe fn open_source(state: &mut AppState, mode: OpenMode) {
    let path = PathBuf::from(get_text(state.controls.source_path));
    if path.as_os_str().is_empty() {
        return choose_source(state, mode);
    }
    set_busy(state, true, &format!("Opening {}…", file_label(&path)));
    let container_key = state
        .container_key
        .clone()
        .filter(|value| secret_applies_to_source(value, &path));
    state.pending_job = Some(PendingJob::Open(mode));
    spawn_job(state.hwnd, move || {
        let pool_key = container_key.as_ref().map(SecretValue::bytes);
        match mode {
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

unsafe fn close_source(state: &mut AppState) {
    if !source_open(state) {
        return;
    }
    if state.settings.clear_credentials_on_source_change {
        clear_credentials_silent(state);
    }
    state.catalog = None;
    state.inception.clear();
    SendMessageW(state.controls.view, CB_RESETCONTENT, 0, 0);
    SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    set_text(state.controls.source_path, "");
    close_source_ui(state);
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
    let key = active_zfs_key(state);
    set_busy(state, true, "Reading folder…");
    state.pending_job = Some(PendingJob::Browse);
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
        show_message(
            state.hwnd,
            DialogIcon::Information,
            "Select something to restore",
            "Select a file or folder in the list, then choose Restore.",
        );
        return;
    };
    let Some(entry) = state.entries.get(index).cloned() else {
        return;
    };
    if !matches!(entry.dirent_type, 4 | 8) {
        show_message(
            state.hwnd,
            DialogIcon::Information,
            "This item can't be restored",
            "Links and special items are shown for reference only. Choose a regular file or folder.",
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
            show_problem(state.hwnd, &error.to_string());
            return;
        }
    };
    let key = active_zfs_key(state);
    let inception = state.inception.last().map(|frame| frame.catalog.clone());
    let volume_index = selected_volume_index(state);
    set_busy(state, true, &format!("Restoring {}…", entry.name));
    state.pending_job = Some(PendingJob::Extract);
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
        show_message(
            state.hwnd,
            DialogIcon::Information,
            "Too many disk images are open",
            "Eight nested disk images are already open. Go back before opening another.",
        );
        return;
    }
    let Some(index) = selected_index(state.controls.list) else {
        show_message(
            state.hwnd,
            DialogIcon::Information,
            "Select a disk image",
            "Select a file that contains a RAW, QCOW2 or VMDK disk image, then open it.",
        );
        return;
    };
    let Some(entry) = state.entries.get(index) else {
        return;
    };
    if entry.dirent_type != 8 {
        show_message(
            state.hwnd,
            DialogIcon::Information,
            "This isn't a disk image",
            "Only files can contain a disk image. Double-click folders to open them.",
        );
        return;
    }
    let image_path = match child_path(&state.current_path, &entry.name) {
        Ok(path) => path,
        Err(error) => {
            show_problem(state.hwnd, &error.to_string());
            return;
        }
    };
    let image_offset = match parse_byte_value(&get_text(state.controls.image_offset), false) {
        Ok(Some(value)) => value,
        Ok(None) => 0,
        Err(error) => {
            show_message(
                state.hwnd,
                DialogIcon::Warning,
                "Check the image offset",
                &error,
            );
            SetFocus(state.controls.image_offset);
            return;
        }
    };
    let image_length = match parse_byte_value(&get_text(state.controls.image_length), true) {
        Ok(value) => value,
        Err(error) => {
            show_message(
                state.hwnd,
                DialogIcon::Warning,
                "Check the image length",
                &error,
            );
            SetFocus(state.controls.image_length);
            return;
        }
    };
    let catalog = state.catalog.clone();
    let view = selected_view(state);
    let parent_image = state.inception.last().map(|frame| frame.catalog.clone());
    let parent_volume = selected_volume_index(state);
    let key = active_zfs_key(state);
    if image_path.to_ascii_lowercase().ends_with(".detto") && state.agent_password.is_none() {
        request_agent_password(
            state,
            "This encrypted Datto disk image needs the agent password of the computer it came from.",
        );
        if state.agent_password.is_none() {
            return;
        }
    }
    let agent_password = image_path
        .to_ascii_lowercase()
        .ends_with(".detto")
        .then(|| state.agent_password.clone())
        .flatten();
    let parent_path = state.current_path.clone();
    let name = file_label(Path::new(&image_path));
    set_busy(state, true, &format!("Looking inside {name}…"));
    state.pending_job = Some(PendingJob::ExploreImage);
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
    set_source_enabled(state, !state.busy && source_open(state));
    update_breadcrumb(state);
    if browse_outer {
        if !source_open(state) {
            close_source_ui(state);
        } else {
            browse(state, destination);
        }
    }
}

unsafe fn update_extracted_file(state: &mut AppState) {
    let Some(target) = open_file_dialog(
        state.hwnd,
        "Choose a file previously restored by ZFS Explore",
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
    let key = active_zfs_key(state).or_else(|| state.keys.last().cloned());
    set_busy(
        state,
        true,
        "Checking the restored file and applying the incremental send…",
    );
    state.pending_job = Some(PendingJob::Update);
    spawn_job(state.hwnd, move || {
        apply_incremental_with_key_material(&stream, &target, key.as_ref().map(SecretValue::bytes))
            .map(|sidecar| JobResult::Updated { target, sidecar })
    });
}

/// Ask for a secret in a masked field. The returned bytes are UTF-8.
unsafe fn prompt_secret(owner: HWND, heading: &str, body: &str, field: &str) -> Option<Vec<u8>> {
    match form_dialog(
        owner,
        &Form {
            heading,
            body,
            field,
            initial: "",
            password: true,
            primary: "Unlock",
            secondary: None,
        },
    ) {
        FormResult::Submitted(text) => Some(utf16_secret_bytes(&text)),
        _ => None,
    }
}

fn utf16_secret_bytes(text: &[u16]) -> Vec<u8> {
    let mut value = String::from_utf16_lossy(text);
    let material = value.as_bytes().to_vec();
    value.zeroize();
    material
}

fn key_prompt(format: &str) -> (&'static str, &'static str) {
    match format {
        "raw" => (
            "Paste the 64-character encryption key from your Slide console, or choose the key file you downloaded.",
            "Encryption key",
        ),
        "hex" => (
            "Enter the 64-character encryption key for this backup, or choose a file that contains it.",
            "Encryption key",
        ),
        "passphrase" => (
            "Enter the passphrase for this backup, or choose a file that contains it.",
            "Passphrase",
        ),
        _ => (
            "Enter the encryption key or passphrase for this backup, or choose its key file.",
            "Key or passphrase",
        ),
    }
}

unsafe fn enter_key(state: &mut AppState) -> bool {
    let format = selected_key_format(state).unwrap_or("supported");
    let (body, field) = key_prompt(format);
    let Some(material) = prompt_secret(state.hwnd, "Unlock this restore point", body, field) else {
        return false;
    };
    store_zfs_key(
        state,
        SecretValue::new(material, credential_scope(state), credential_view(state)),
    );
    credential_changed(
        state,
        "Key entered. It is kept only in memory while ZFS Explore is open.",
    );
    true
}

unsafe fn enter_container_key(state: &mut AppState) -> bool {
    let Some(material) = prompt_secret(
        state.hwnd,
        "Unlock the Datto drive",
        "Enter the passphrase supplied for this Datto Reverse RoundTrip drive.",
        "Drive passphrase",
    ) else {
        return false;
    };
    state.container_key = Some(SecretValue::new(material, source_box_scope(state), None));
    credential_changed(
        state,
        "Datto drive passphrase entered. It is kept only in memory.",
    );
    true
}

unsafe fn enter_agent_password(state: &mut AppState) -> bool {
    let Some(material) = prompt_secret(
        state.hwnd,
        "Unlock the Datto disk image",
        "Enter the agent password of the protected computer this disk image came from.",
        "Agent password",
    ) else {
        return false;
    };
    state.agent_password = Some(SecretValue::new(material, credential_scope(state), None));
    credential_changed(state, "Agent password entered. It is kept only in memory.");
    true
}

unsafe fn choose_key_file(state: &mut AppState) -> bool {
    let Some(mut secret) = choose_secret_file(
        state.hwnd,
        "Choose the key file (32-byte raw, 64-character hex, or passphrase)",
        514,
        false,
    ) else {
        return false;
    };
    secret.source = credential_scope(state);
    secret.view = credential_view(state);
    store_zfs_key(state, secret);
    credential_changed(
        state,
        "Key file loaded. Its contents are kept only in memory.",
    );
    true
}

unsafe fn choose_container_key_file(state: &mut AppState) -> bool {
    let Some(mut secret) = choose_secret_file(
        state.hwnd,
        "Choose the Datto drive passphrase file",
        4096,
        true,
    ) else {
        return false;
    };
    secret.source = source_box_scope(state);
    state.container_key = Some(secret);
    credential_changed(state, "Datto drive passphrase file loaded into memory.");
    true
}

unsafe fn choose_agent_password_file(state: &mut AppState) -> bool {
    let Some(mut secret) = choose_secret_file(
        state.hwnd,
        "Choose the Datto agent password file",
        4096,
        true,
    ) else {
        return false;
    };
    secret.source = credential_scope(state);
    state.agent_password = Some(secret);
    credential_changed(state, "Agent password file loaded into memory.");
    true
}

unsafe fn finish_job(state: &mut AppState, result: JobMessage) {
    let pending = state.pending_job.take();
    set_busy(state, false, "Ready");
    match result {
        Err(error) => handle_job_error(state, &error, pending),
        Ok(JobResult::Catalog(catalog)) => {
            activate_source(state, &catalog.path);
            SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
            SendMessageW(state.controls.view, CB_RESETCONTENT, 0, 0);
            let zone = state
                .settings
                .snapshot_time_zone
                .parse()
                .unwrap_or_default();
            for view in &catalog.views {
                let label = wide(&presentation::view_line(view, zone));
                SendMessageW(
                    state.controls.view,
                    CB_ADDSTRING,
                    0,
                    label.as_ptr() as isize,
                );
            }
            // Start on the newest restore point that holds files.
            let start = presentation::recommended_view(&catalog.views).unwrap_or(0);
            SendMessageW(state.controls.view, CB_SETCURSEL, start, 0);
            let title = format!("{} — {}", source_title(state, &catalog.path), APP_TITLE);
            set_text(state.hwnd, &title);
            set_status(state, &presentation::source_description(&catalog));
            state.catalog = Some(catalog);
            set_source_enabled(state, true);
            update_mode(state);
            update_breadcrumb(state);
            if ensure_selected_view_key(state) {
                browse(state, "/".to_owned());
            } else {
                clear_directory_ui(state);
            }
            update_guidance(state);
        }
        Ok(JobResult::Directory { path, mut entries }) => {
            entries.sort_by(|a, b| {
                (b.dirent_type == 4)
                    .cmp(&(a.dirent_type == 4))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
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
                            file_label(Path::new(&frame.catalog.image_path))
                        ))
                ),
            );
            update_breadcrumb(state);
            update_guidance(state);
        }
        Ok(JobResult::InceptionOpened {
            catalog,
            parent_path,
            parent_volume,
            standalone,
        }) => {
            let Some(selected) = presentation::recommended_volume(&catalog.volumes) else {
                let details = catalog
                    .volumes
                    .iter()
                    .filter_map(|volume| volume.diagnostic.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                handle_unsupported_image(
                    state,
                    if details.is_empty() {
                        "No NTFS, FAT, exFAT or ext filesystem was found."
                    } else {
                        &details
                    },
                    pending,
                );
                return;
            };
            if standalone {
                activate_source(state, Path::new(&catalog.image_path));
                state.catalog = None;
                state.inception.clear();
                clear_directory_ui(state);
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
            // These fields apply to the next selected child image, not the
            // image that was just opened. Carrying the parent's values forward
            // can make a valid nested image appear corrupt.
            set_text(state.controls.image_offset, "");
            set_text(state.controls.image_length, "");
            let supported = catalog
                .volumes
                .iter()
                .filter(|volume| volume.filesystem.is_some())
                .count();
            let status = format!(
                "Opened {} · {} · {} part{} ({} readable)",
                file_label(Path::new(&catalog.image_path)),
                presentation::format_size(catalog.disk_size),
                catalog.volumes.len(),
                if catalog.volumes.len() == 1 { "" } else { "s" },
                supported
            );
            state.inception.push(ImageFrame {
                catalog,
                parent_path,
                parent_volume,
            });
            state.current_path = "/".to_owned();
            set_source_enabled(state, true);
            update_mode(state);
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
                ""
            } else if extraction.update_eligible {
                "\n\nA small .zfse.json file was saved beside it so the copy can later be brought up to date from an incremental send (Restore > Update a restored file)."
            } else {
                "\n\nThis copy came from current files, not a snapshot, so it can't be updated from an incremental send."
            };
            let details = format!("SHA-256: {}{update}", extraction.sha256);
            show_restored(
                state,
                &destination,
                &format!(
                    "Saved to {} ({}).",
                    destination.display(),
                    presentation::format_size(extraction.logical_size)
                ),
                &details,
            );
            set_status(state, &format!("Restored {}", destination.display()));
        }
        Ok(JobResult::TreeExtracted {
            destination,
            extraction,
        }) => {
            let skipped = if extraction.skipped_entries == 0 {
                String::new()
            } else {
                format!(
                    " {} link{} or special item{} {} skipped.",
                    extraction.skipped_entries,
                    if extraction.skipped_entries == 1 {
                        ""
                    } else {
                        "s"
                    },
                    if extraction.skipped_entries == 1 {
                        ""
                    } else {
                        "s"
                    },
                    if extraction.skipped_entries == 1 {
                        "was"
                    } else {
                        "were"
                    },
                )
            };
            show_restored(
                state,
                &destination,
                &format!(
                    "Saved {} file{} in {} folder{} to {} ({}).{skipped}",
                    extraction.files,
                    if extraction.files == 1 { "" } else { "s" },
                    extraction.directories,
                    if extraction.directories == 1 { "" } else { "s" },
                    destination.display(),
                    presentation::format_size(extraction.logical_bytes),
                ),
                "Only file contents are restored; owners, permissions and timestamps are not. Links are never followed.",
            );
            set_status(
                state,
                &format!("Restored folder to {}", destination.display()),
            );
        }
        Ok(JobResult::Updated { target, sidecar }) => {
            show_message(
                state.hwnd,
                DialogIcon::Information,
                "The restored file is up to date",
                &format!(
                    "Updated {} to snapshot {}.\n\nNew size: {}\nSHA-256: {}",
                    target.display(),
                    sidecar.snapshot_guid,
                    presentation::format_size(sidecar.logical_size),
                    sidecar.sha256
                ),
            );
            set_status(state, &format!("Updated {}", target.display()));
        }
    }
}

unsafe fn show_restored(state: &AppState, destination: &Path, summary: &str, details: &str) {
    let heading = format!("Restored “{}”", file_label(destination));
    let mut task = Task::new(DialogIcon::Information, &heading, summary);
    task.actions = &[(
        "Show in File Explorer",
        "Open the folder and select the restored copy.",
    )];
    task.details = Some(details);
    task.close_label = true;
    if task_dialog(state.hwnd, &task) == Some(0) {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", destination.display()))
            .spawn();
    }
}

unsafe fn file_icon(state: &AppState, entry: &DirectoryEntry) -> i32 {
    let key = if entry.dirent_type == 4 {
        "<folder>".to_owned()
    } else if entry.dirent_type == 8 && likely_disk_image(&entry.name) {
        "<image>".to_owned()
    } else if entry.dirent_type == 8 {
        Path::new(&entry.name)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| value.len() <= 16 && value.chars().all(|c| c.is_ascii_alphanumeric()))
            .map_or_else(|| "<file>".to_owned(), |value| value.to_ascii_lowercase())
    } else {
        "<other>".to_owned()
    };
    if let Some(index) = state.icon_cache.borrow().get(&key) {
        return *index;
    }
    let index = if key == "<image>" {
        let mut info = SHSTOCKICONINFO {
            cbSize: size_of::<SHSTOCKICONINFO>() as u32,
            ..zeroed()
        };
        if SHGetStockIconInfo(
            SIID_DRIVEFIXED,
            SHGSI_SYSICONINDEX | SHGSI_SMALLICON,
            &mut info,
        ) >= 0
        {
            info.iSysImageIndex
        } else {
            0
        }
    } else {
        // Only the name pattern is used; nothing in the backup is read by the shell.
        let (name, attributes) = match key.as_str() {
            "<folder>" => ("folder".to_owned(), FILE_ATTRIBUTE_DIRECTORY),
            "<file>" | "<other>" => ("file".to_owned(), FILE_ATTRIBUTE_NORMAL),
            extension => (format!("file.{extension}"), FILE_ATTRIBUTE_NORMAL),
        };
        let name = wide(&name);
        let mut info = SHFILEINFOW::default();
        SHGetFileInfoW(
            name.as_ptr(),
            attributes,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        );
        info.iIcon
    };
    state.icon_cache.borrow_mut().insert(key, index);
    index
}

unsafe fn populate_list(state: &AppState) {
    let list = state.controls.list;
    SendMessageW(list, 0x000B, 0, 0); // WM_SETREDRAW off
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    for (index, entry) in state.entries.iter().enumerate() {
        let name = wide(&entry.name);
        let mut item = LVITEMW {
            mask: LVIF_TEXT | LVIF_IMAGE,
            iItem: index as i32,
            iSubItem: 0,
            pszText: name.as_ptr() as *mut u16,
            iImage: file_icon(state, entry),
            ..zeroed()
        };
        SendMessageW(
            list,
            LVM_INSERTITEMW,
            0,
            (&mut item as *mut LVITEMW) as isize,
        );
        set_list_text(list, index, 1, entry_kind(entry));
        let size = if entry.dirent_type == 8 {
            entry
                .logical_size
                .map_or_else(String::new, presentation::format_size)
        } else {
            String::new()
        };
        set_list_text(list, index, 2, &size);
    }
    SendMessageW(list, 0x000B, 1, 0);
    InvalidateRect(list, null(), 1);
    update_selection_actions(state);
}

fn entry_kind(entry: &DirectoryEntry) -> &'static str {
    match entry.dirent_type {
        4 => "Folder",
        8 if likely_disk_image(&entry.name) => "Disk image",
        8 => "File",
        10 => "Link",
        _ => "Special item",
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
        state.controls.open_backup,
        state.controls.open_drive,
        state.controls.close_source,
        state.controls.welcome_file,
        state.controls.welcome_drive,
        state.controls.welcome_path,
        state.controls.guide_action,
    ] {
        EnableWindow(control, enabled);
    }
    set_source_enabled(state, !busy && source_open(state));
    set_status(state, message);
}

unsafe fn set_source_enabled(state: &AppState, enabled: bool) {
    let enabled = enabled as i32;
    for control in [
        state.controls.path,
        state.controls.go,
        state.controls.list,
        state.controls.image_offset,
        state.controls.image_length,
    ] {
        EnableWindow(control, enabled);
    }
    EnableWindow(
        state.controls.up,
        (enabled != 0 && state.current_path != "/") as i32,
    );
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
    } else {
        EnableWindow(state.controls.restore, 0);
        EnableWindow(state.controls.inception, 0);
    }
    update_context_visibility(state);
    layout(state);
}

/// Show the welcome screen with nothing open, and the browser otherwise.
unsafe fn update_mode(state: &AppState) {
    let open = source_open(state);
    let controls = &state.controls;
    for control in [
        controls.welcome_heading,
        controls.welcome_body,
        controls.welcome_file,
        controls.welcome_drive,
        controls.welcome_path,
        controls.welcome_steps,
    ] {
        ShowWindow(control, if open { SW_HIDE } else { SW_SHOW });
    }
    for control in [
        controls.up,
        controls.path,
        controls.go,
        controls.breadcrumb,
        controls.list,
        controls.selection,
        controls.inception,
        controls.restore,
    ] {
        ShowWindow(control, if open { SW_SHOW } else { SW_HIDE });
    }
    ShowWindow(controls.close_source, if open { SW_SHOW } else { SW_HIDE });
    let menu = GetMenu(state.hwnd);
    windows_sys::Win32::UI::WindowsAndMessaging::EnableMenuItem(
        menu,
        ID_CLOSE_SOURCE as u32,
        MF_BYCOMMAND | if open { MF_ENABLED } else { MF_GRAYED },
    );
    update_header(state);
    update_context_visibility(state);
    update_guidance(state);
    layout(state);
}

unsafe fn update_header(state: &AppState) {
    let controls = &state.controls;
    if let Some(catalog) = &state.catalog {
        set_text(controls.header_title, &source_title(state, &catalog.path));
        let mut detail = presentation::source_description(catalog);
        if let Some(frame) = state.inception.last() {
            detail.push_str(&format!(
                " · inside {}",
                file_label(Path::new(&frame.catalog.image_path))
            ));
        }
        set_text(controls.header_detail, &detail);
    } else if let Some(frame) = state.inception.first() {
        set_text(
            controls.header_title,
            &file_label(Path::new(&frame.catalog.image_path)),
        );
        set_text(
            controls.header_detail,
            &format!(
                "Disk image · {} · {}",
                frame.catalog.container.to_string().to_uppercase(),
                presentation::format_size(frame.catalog.disk_size)
            ),
        );
    } else {
        set_text(controls.header_title, APP_TITLE);
        set_text(
            controls.header_detail,
            "Recover files from ZFS, Slide and Datto backups. Your backup is only read, never changed.",
        );
    }
}

unsafe fn update_context_visibility(state: &AppState) {
    let open = source_open(state);
    let in_image = !state.inception.is_empty();
    let show_view = open && state.catalog.is_some();
    let show_range = open && state.settings.show_advanced_image_range;
    for control in [state.controls.view_label, state.controls.view] {
        ShowWindow(control, if show_view { SW_SHOW } else { SW_HIDE });
    }
    for control in [
        state.controls.volume_label,
        state.controls.volume,
        state.controls.leave_image,
    ] {
        ShowWindow(control, if open && in_image { SW_SHOW } else { SW_HIDE });
    }
    for control in [
        state.controls.image_range_label,
        state.controls.image_offset,
        state.controls.image_length,
    ] {
        ShowWindow(control, if show_range { SW_SHOW } else { SW_HIDE });
    }
    let back_label = if state.inception.len() > 1 {
        "Back to the outer disk image"
    } else if in_image && state.catalog.is_some() {
        "Back to the backup"
    } else {
        "Close disk image"
    };
    set_text(state.controls.leave_image, back_label);
}

/// Suggest the one next step a first-time user needs, if any.
unsafe fn update_guidance(state: &AppState) {
    let guide = if !source_open(state) || state.busy {
        Guide::None
    } else if selected_view_locked(state) {
        Guide::Unlock
    } else if state.inception.is_empty()
        && let Some(index) = largest_image(state)
    {
        Guide::OpenImage(index)
    } else if let Some(frame) = state.inception.last()
        && frame
            .catalog
            .volumes
            .iter()
            .filter(|volume| volume.filesystem.is_some())
            .count()
            > 1
    {
        Guide::ChooseVolume
    } else {
        Guide::None
    };
    let (text, action) = match guide {
        Guide::None => (String::new(), ""),
        Guide::Unlock => (
            "This restore point is encrypted. Unlock it with its encryption key or key file to see its files.".to_owned(),
            "Unlock…",
        ),
        Guide::OpenImage(index) => {
            let entry = &state.entries[index];
            (
                format!(
                    "The computer’s files are inside its disk image “{}” ({}). Open it to browse documents and settings.",
                    entry.name,
                    entry.logical_size.map_or_else(String::new, presentation::format_size)
                ),
                "Open disk image",
            )
        }
        Guide::ChooseVolume => (
            "This disk has more than one readable part. The recommended part is shown; choose another in “Part of the disk” if your files aren’t here.".to_owned(),
            "",
        ),
    };
    let changed = state.guide.get() != guide;
    state.guide.set(guide);
    set_text(state.controls.guide_text, &text);
    set_text(state.controls.guide_action, action);
    ShowWindow(
        state.controls.guide_text,
        if guide == Guide::None {
            SW_HIDE
        } else {
            SW_SHOW
        },
    );
    ShowWindow(
        state.controls.guide_action,
        if action.is_empty() { SW_HIDE } else { SW_SHOW },
    );
    if changed {
        layout(state);
        InvalidateRect(state.hwnd, null(), 1);
    }
}

unsafe fn selected_view_locked(state: &AppState) -> bool {
    state.inception.is_empty()
        && state
            .catalog
            .as_ref()
            .and_then(|catalog| selected_view(state).and_then(|index| catalog.views.get(index)))
            .is_some_and(|view| view.encrypted)
        && active_zfs_key(state).is_none()
}

fn largest_image(state: &AppState) -> Option<usize> {
    state
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.dirent_type == 8 && likely_disk_image(&entry.name))
        .max_by_key(|(_, entry)| entry.logical_size.unwrap_or(0))
        .map(|(index, _)| index)
}

unsafe fn run_guide_action(state: &mut AppState) {
    match state.guide.get() {
        Guide::Unlock => show_contextual_credentials(state),
        Guide::OpenImage(index) => {
            select_list_item(state.controls.list, index);
            explore_selected_image(state);
        }
        Guide::ChooseVolume | Guide::None => {}
    }
}

unsafe fn select_list_item(list: HWND, index: usize) {
    let mut selection = LVITEMW {
        stateMask: LVIS_SELECTED | LVIS_FOCUSED,
        state: LVIS_SELECTED | LVIS_FOCUSED,
        ..zeroed()
    };
    SendMessageW(
        list,
        LVM_SETITEMSTATE,
        index,
        (&mut selection as *mut LVITEMW) as isize,
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

unsafe fn update_selection_actions(state: &AppState) {
    if state.busy {
        return;
    }
    let entry = selected_index(state.controls.list).and_then(|index| state.entries.get(index));
    let restorable = entry.is_some_and(|entry| matches!(entry.dirent_type, 4 | 8));
    let image = entry.is_some_and(|entry| entry.dirent_type == 8 && likely_disk_image(&entry.name));
    EnableWindow(state.controls.restore, restorable as i32);
    EnableWindow(state.controls.inception, image as i32);
    EnableWindow(state.controls.up, (state.current_path != "/") as i32);
    let text = match entry {
        None if state.entries.is_empty() && source_open(state) && !selected_view_locked(state) => {
            "This folder is empty.".to_owned()
        }
        None => "Select a file or folder, then choose Restore. Double-click a folder to open it."
            .to_owned(),
        Some(entry) if entry.dirent_type == 4 => {
            format!(
                "“{}” · folder · Restore saves everything inside it",
                entry.name
            )
        }
        Some(entry) if entry.dirent_type == 8 && likely_disk_image(&entry.name) => format!(
            "“{}” · disk image · {} · Open it to browse its files, or Restore to save the whole image",
            entry.name,
            entry
                .logical_size
                .map_or_else(String::new, presentation::format_size)
        ),
        Some(entry) if entry.dirent_type == 8 => format!(
            "“{}” · {}",
            entry.name,
            entry
                .logical_size
                .map_or_else(String::new, presentation::format_size)
        ),
        Some(entry) => format!(
            "“{}” · link or special item · can't be restored",
            entry.name
        ),
    };
    set_text(state.controls.selection, &text);
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
            Err(error) => show_problem(state.hwnd, &error.to_string()),
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

/// List physical drives in a native chooser and open the chosen one read-only.
unsafe fn open_physical_drive(state: &mut AppState) {
    loop {
        let drives = enumerate_physical_drives();
        let titles = drives.iter().map(PhysicalDrive::title).collect::<Vec<_>>();
        let notes = drives.iter().map(PhysicalDrive::note).collect::<Vec<_>>();
        let mut actions = titles
            .iter()
            .zip(&notes)
            .map(|(title, note)| (title.as_str(), note.as_str()))
            .collect::<Vec<_>>();
        actions.push(("Refresh the list", "Choose this after connecting a drive."));
        let content = if drives.is_empty() {
            "No drives are visible to ZFS Explore. Connect the drive, then choose Refresh. Reading drives usually requires starting ZFS Explore with “Run as administrator”."
        } else {
            "Choose the drive that came from your Slide or Datto device. It opens read-only: nothing on it is written, mounted or initialized. If a drive is missing, connect it and choose Refresh."
        };
        let footer = "Drives can only be read when ZFS Explore runs as an administrator. Right-click the app and choose “Run as administrator” if Windows denies access.";
        let task = Task {
            actions: &actions,
            footer: Some(footer),
            ..Task::new(DialogIcon::Information, "Open a connected drive", content)
        };
        match task_dialog(state.hwnd, &task) {
            Some(index) if index < drives.len() => {
                let drive = &drives[index];
                if drive.system
                    && task_dialog(
                        state.hwnd,
                        &Task {
                            actions: &[("Open it anyway", "It still opens read-only.")],
                            ..Task::new(
                                DialogIcon::Warning,
                                "This is the computer’s own Windows disk",
                                "Backups are usually on a separate drive. Open this disk only if you are sure it holds the backup.",
                            )
                        },
                    ) != Some(0)
                {
                    continue;
                }
                state.drive_title = Some((drive.path.clone(), drive.title()));
                set_text(
                    state.controls.source_path,
                    &drive.path.display().to_string(),
                );
                open_source(state, OpenMode::Auto);
                return;
            }
            Some(_) => continue,
            None => return,
        }
    }
}

fn enumerate_physical_drives() -> Vec<PhysicalDrive> {
    let system = system_disk_number();
    (0..64)
        .filter_map(|number| inspect_physical_drive(number, system))
        .collect()
}

/// The disk number holding the Windows system volume, so it can be labeled.
fn system_disk_number() -> Option<u32> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
    let path = wide(&format!(r"\\.\{drive}"));
    // SAFETY: the handle is opened with zero desired access, the output buffer
    // has the documented layout, and the handle is always closed.
    unsafe {
        let handle = CreateFileW(
            path.as_ptr(),
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
        let mut extents = VOLUME_DISK_EXTENTS::default();
        let mut returned = 0_u32;
        let ok = DeviceIoControl(
            handle,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            null(),
            0,
            (&mut extents as *mut VOLUME_DISK_EXTENTS).cast(),
            size_of::<VOLUME_DISK_EXTENTS>() as u32,
            &mut returned,
            null_mut(),
        ) != 0;
        CloseHandle(handle);
        (ok && extents.NumberOfDiskExtents >= 1).then_some(extents.Extents[0].DiskNumber)
    }
}

fn inspect_physical_drive(number: u32, system: Option<u32>) -> Option<PhysicalDrive> {
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
            .then_some(length.Length as u64)
            .or_else(|| {
                // Geometry needs no read access, unlike the length query.
                let mut geometry = [0_u8; 256];
                (DeviceIoControl(
                    handle,
                    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                    null(),
                    0,
                    geometry.as_mut_ptr().cast(),
                    geometry.len() as u32,
                    &mut returned,
                    null_mut(),
                ) != 0
                    && returned >= 32)
                    .then(|| i64::from_le_bytes(geometry[24..32].try_into().unwrap()) as u64)
            });

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
        Some(PhysicalDrive {
            path: PathBuf::from(&path),
            number,
            model: if model.is_empty() {
                "Unknown drive".to_owned()
            } else {
                model
            },
            size,
            removable,
            system: system == Some(number),
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
    if source_open(state) {
        browse(state, state.current_path.clone());
    }
}

unsafe fn request_zfs_key(state: &mut AppState, reason: Option<&str>) -> bool {
    let format = selected_key_format(state).unwrap_or("supported").to_owned();
    let (body, field) = key_prompt(&format);
    let body = reason.map_or_else(|| body.to_owned(), |reason| format!("{reason} {body}"));
    match form_dialog(
        state.hwnd,
        &Form {
            heading: "Unlock this restore point",
            body: &body,
            field,
            initial: "",
            password: true,
            primary: "Unlock",
            secondary: Some("Choose key file…"),
        },
    ) {
        FormResult::Submitted(text) => {
            store_zfs_key(
                state,
                SecretValue::new(
                    utf16_secret_bytes(&text),
                    credential_scope(state),
                    credential_view(state),
                ),
            );
            credential_changed(
                state,
                "Key entered. It is kept only in memory while ZFS Explore is open.",
            );
            true
        }
        FormResult::Secondary => choose_key_file(state),
        FormResult::Cancelled => false,
    }
}

unsafe fn request_container_key(state: &mut AppState, reason: &str) -> bool {
    match form_dialog(
        state.hwnd,
        &Form {
            heading: "Unlock the Datto drive",
            body: reason,
            field: "Drive passphrase",
            initial: "",
            password: true,
            primary: "Unlock",
            secondary: Some("Choose passphrase file…"),
        },
    ) {
        FormResult::Submitted(text) => {
            state.container_key = Some(SecretValue::new(
                utf16_secret_bytes(&text),
                source_box_scope(state),
                None,
            ));
            credential_changed(
                state,
                "Datto drive passphrase entered. It is kept only in memory.",
            );
            true
        }
        FormResult::Secondary => choose_container_key_file(state),
        FormResult::Cancelled => false,
    }
}

unsafe fn request_agent_password(state: &mut AppState, reason: &str) -> bool {
    match form_dialog(
        state.hwnd,
        &Form {
            heading: "Unlock the Datto disk image",
            body: reason,
            field: "Agent password",
            initial: "",
            password: true,
            primary: "Unlock",
            secondary: Some("Choose password file…"),
        },
    ) {
        FormResult::Submitted(text) => {
            state.agent_password = Some(SecretValue::new(
                utf16_secret_bytes(&text),
                credential_scope(state),
                None,
            ));
            credential_changed(state, "Agent password entered. It is kept only in memory.");
            true
        }
        FormResult::Secondary => choose_agent_password_file(state),
        FormResult::Cancelled => false,
    }
}

unsafe fn show_contextual_credentials(state: &mut AppState) {
    let encrypted_view = state
        .catalog
        .as_ref()
        .and_then(|catalog| selected_view(state).and_then(|index| catalog.views.get(index)))
        .is_some_and(|view| view.encrypted);
    if encrypted_view {
        if request_zfs_key(state, None) {
            refresh_after_zfs_key(state);
        }
        return;
    }
    let selected_detto = selected_index(state.controls.list)
        .and_then(|index| state.entries.get(index))
        .is_some_and(|entry| entry.name.to_ascii_lowercase().ends_with(".detto"));
    if selected_detto {
        request_agent_password(
            state,
            "The selected .detto disk image uses the agent password of the computer it came from.",
        );
        return;
    }
    show_message(
        state.hwnd,
        DialogIcon::Information,
        "Nothing here needs a key",
        "The current restore point isn't encrypted. Datto drive passphrases and agent passwords are in the Keys menu.",
    );
}

unsafe fn refresh_after_zfs_key(state: &mut AppState) {
    if state.catalog.is_some() && state.inception.is_empty() {
        browse(state, state.current_path.clone());
    }
    update_guidance(state);
}

unsafe fn retry_pending_job(state: &mut AppState, pending: Option<PendingJob>) {
    match pending {
        Some(PendingJob::Open(mode)) => open_source(state, mode),
        Some(PendingJob::Browse) => browse(state, state.current_path.clone()),
        Some(PendingJob::Extract) => extract_selected(state),
        Some(PendingJob::ExploreImage) => explore_selected_image(state),
        Some(PendingJob::Update) => update_extracted_file(state),
        None => {}
    }
}

unsafe fn restore_source_box_after_failed_open(state: &AppState, pending: Option<PendingJob>) {
    if matches!(pending, Some(PendingJob::Open(_)))
        && let Some(path) = active_source_path(state)
    {
        set_text(state.controls.source_path, &path.display().to_string());
    }
}

unsafe fn handle_job_error(state: &mut AppState, error: &str, pending: Option<PendingJob>) {
    let lower = error.to_ascii_lowercase();
    let needs_container_key = lower.contains("luks-encrypted")
        || lower.contains("unlocking luks container")
        || lower.contains("datto pool passphrase");
    if needs_container_key {
        state.container_key = None;
        if request_container_key(
            state,
            "This Datto drive is protected by a passphrase. Enter it and ZFS Explore will open the drive again.",
        ) {
            retry_pending_job(state, pending);
        } else {
            restore_source_box_after_failed_open(state, pending);
        }
        return;
    }

    let needs_zfs_key = lower.contains("enter or choose its")
        || lower.contains("uses native zfs dataset encryption; provide")
        || lower.contains("supplied key did not authenticate")
        || lower.contains("raw zfs key must")
        || lower.contains("hex zfs key must")
        || lower.contains("requires a dataset key");
    if needs_zfs_key {
        let wrong = lower.contains("did not authenticate") || lower.contains("key must");
        remove_active_zfs_key(state);
        clear_directory_ui(state);
        if request_zfs_key(
            state,
            Some(if wrong {
                "That key didn't unlock this restore point. Check that you copied all of it."
            } else {
                "This restore point is encrypted."
            }),
        ) {
            retry_pending_job(state, pending);
        } else {
            set_status(state, "The restore point is still locked");
            update_guidance(state);
        }
        return;
    }

    let needs_agent_password = lower.contains("agent password")
        || lower.contains("encrypted datto .detto")
        || (lower.contains("unlocking") && lower.contains("encryptionkeystash"));
    if needs_agent_password {
        state.agent_password = None;
        if request_agent_password(
            state,
            "That agent password didn't unlock this disk image. Enter it again and ZFS Explore will retry.",
        ) {
            retry_pending_job(state, pending);
        }
        return;
    }

    if matches!(pending, Some(PendingJob::ExploreImage)) {
        handle_unsupported_image(state, error, pending);
        return;
    }

    let access_denied = lower.contains("access is denied")
        || lower.contains("permission denied")
        || lower.contains("os error 5");
    if access_denied {
        let task = Task {
            actions: &[(
                "Choose another backup",
                "Anything already open stays open until another backup opens.",
            )],
            details: Some(error),
            ..Task::new(
                DialogIcon::Warning,
                "Windows didn't allow ZFS Explore to read this",
                "Ordinary backup files don't need special permission. To read a drive, close ZFS Explore, right-click it and choose “Run as administrator”, then open the drive again.",
            )
        };
        if task_dialog(state.hwnd, &task) == Some(0) {
            choose_source(state, OpenMode::Auto);
        } else {
            restore_source_box_after_failed_open(state, pending);
        }
        return;
    }

    if matches!(pending, Some(PendingJob::Open(_))) && lower.contains("source was not recognized") {
        let friendly = presentation::friendly_error(error);
        let task = Task {
            actions: &[(
                "Choose another backup",
                "Anything already open stays open until another backup opens.",
            )],
            details: Some(&friendly.details),
            ..Task::new(DialogIcon::Warning, &friendly.title, &friendly.message)
        };
        if task_dialog(state.hwnd, &task) == Some(0) {
            choose_source(state, OpenMode::Auto);
        } else {
            restore_source_box_after_failed_open(state, pending);
        }
        return;
    }

    show_problem(state.hwnd, error);
    restore_source_box_after_failed_open(state, pending);
}

unsafe fn handle_unsupported_image(
    state: &mut AppState,
    details: &str,
    pending: Option<PendingJob>,
) {
    let nested = matches!(pending, Some(PendingJob::ExploreImage));
    let first = if nested {
        (
            "Restore the image file instead",
            "Save a copy of the selected file without looking inside it.",
        )
    } else {
        (
            "Choose another file",
            "Select a RAW, QCOW2 or self-contained VMDK disk image.",
        )
    };
    let task = Task {
        actions: &[
            first,
            (
                "Set an advanced image range",
                "Show offset and length fields for a disk image embedded inside a larger file.",
            ),
        ],
        details: Some(details),
        ..Task::new(
            DialogIcon::Warning,
            "ZFS Explore can't look inside this file",
            "It isn't a disk image with a filesystem ZFS Explore can read (NTFS, FAT, exFAT or ext).",
        )
    };
    match task_dialog(state.hwnd, &task) {
        Some(0) if nested => extract_selected(state),
        Some(0) => choose_source(state, OpenMode::Image),
        Some(1) => show_advanced_image_range(state),
        _ => {}
    }
}

unsafe fn show_advanced_image_range(state: &mut AppState) {
    if !state.settings.show_advanced_image_range {
        state.settings.show_advanced_image_range = true;
        update_settings_menu(state);
        let _ = save_settings(&state.settings);
    }
    update_context_visibility(state);
    layout(state);
    SetFocus(state.controls.image_offset);
    set_status(
        state,
        "Enter the selected file's byte offset and optional length, then open it as a disk image again",
    );
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
            show_problem(owner, &format!("Could not read the key file: {error}"));
            return None;
        }
    };
    if metadata.len() > maximum_size {
        show_message(
            owner,
            DialogIcon::Warning,
            "That file is too large to be a key",
            &format!(
                "{} is larger than {maximum_size} bytes. Choose the key file for this backup.",
                path.display()
            ),
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
            Some(SecretValue::new(material, None, None))
        }
        Err(error) => {
            show_problem(owner, &format!("Could not read the key file: {error}"));
            None
        }
    }
}

unsafe fn credential_changed(state: &mut AppState, status: &str) {
    set_status(state, status);
}

unsafe fn clear_credentials(state: &mut AppState) {
    let source_open = source_open(state);
    if source_open
        && task_dialog(
            state.hwnd,
            &Task {
                actions: &[(
                    "Forget keys and close the backup",
                    "You can open it again and enter its keys later.",
                )],
                ..Task::new(
                    DialogIcon::Warning,
                    "Forget all keys?",
                    "The open backup must close so its keys can be removed from memory.",
                )
            },
        ) != Some(0)
    {
        return;
    }
    clear_credentials_silent(state);
    if source_open {
        state.catalog = None;
        state.inception.clear();
        SendMessageW(state.controls.view, CB_RESETCONTENT, 0, 0);
        SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
        close_source_ui(state);
        set_status(state, "All keys were forgotten and the backup was closed");
    } else {
        set_status(state, "All keys were forgotten");
    }
}

unsafe fn clear_credentials_silent(state: &mut AppState) {
    state.keys.clear();
    state.container_key = None;
    state.agent_password = None;
}

unsafe fn clear_credentials_for_source(state: &mut AppState, source: &Path) {
    state
        .keys
        .retain(|value| value.source.as_deref().is_none_or(|path| path == source));
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
}

unsafe fn credential_scope(state: &AppState) -> Option<PathBuf> {
    active_source_path(state).or_else(|| source_box_scope(state))
}

unsafe fn source_box_scope(state: &AppState) -> Option<PathBuf> {
    let path = PathBuf::from(get_text(state.controls.source_path));
    (!path.as_os_str().is_empty()).then_some(path)
}

unsafe fn credential_view(state: &AppState) -> Option<String> {
    let catalog = state.catalog.as_ref()?;
    let view = catalog.views.get(selected_view(state)?)?;
    Some(view.selector.clone())
}

fn secret_applies_to_source(secret: &SecretValue, source: &Path) -> bool {
    secret.source.as_deref().is_none_or(|path| path == source)
}

unsafe fn active_zfs_key(state: &AppState) -> Option<SecretValue> {
    let source = active_source_path(state).or_else(|| source_box_scope(state));
    let view = credential_view(state);
    state
        .keys
        .iter()
        .rev()
        .find(|key| {
            key.source
                .as_deref()
                .is_none_or(|path| source.as_deref() == Some(path))
                && key
                    .view
                    .as_deref()
                    .is_none_or(|selector| view.as_deref() == Some(selector))
        })
        .cloned()
}

unsafe fn store_zfs_key(state: &mut AppState, secret: SecretValue) {
    state
        .keys
        .retain(|existing| existing.source != secret.source || existing.view != secret.view);
    state.keys.push(secret);
}

unsafe fn remove_active_zfs_key(state: &mut AppState) {
    let source = credential_scope(state);
    let view = credential_view(state);
    state.keys.retain(|key| {
        !(key
            .source
            .as_deref()
            .is_none_or(|path| source.as_deref() == Some(path))
            && key
                .view
                .as_deref()
                .is_none_or(|selector| view.as_deref() == Some(selector)))
    });
}

unsafe fn selected_key_format(state: &AppState) -> Option<&str> {
    state
        .catalog
        .as_ref()?
        .views
        .get(selected_view(state)?)?
        .key_format
        .as_deref()
}

unsafe fn ensure_selected_view_key(state: &mut AppState) -> bool {
    let Some(catalog) = &state.catalog else {
        return true;
    };
    let Some(index) = selected_view(state) else {
        return false;
    };
    if !catalog.views.get(index).is_some_and(|view| view.encrypted) {
        return true;
    }
    if active_zfs_key(state).is_some() {
        return true;
    }
    let supplied = request_zfs_key(state, Some("This restore point is encrypted."));
    if !supplied {
        set_status(
            state,
            "The restore point is locked. Choose Unlock when you have its key.",
        );
    }
    supplied
}

unsafe fn clear_directory_ui(state: &mut AppState) {
    state.entries.clear();
    populate_list(state);
    state.current_path = "/".to_owned();
    set_text(state.controls.path, "/");
    update_breadcrumb(state);
}

unsafe fn active_source_path(state: &AppState) -> Option<PathBuf> {
    state
        .catalog
        .as_ref()
        .map(|catalog| catalog.path.clone())
        .or_else(|| {
            state.inception.first().and_then(|frame| {
                frame
                    .parent_path
                    .is_none()
                    .then(|| PathBuf::from(&frame.catalog.image_path))
            })
        })
}

unsafe fn activate_source(state: &mut AppState, source: &Path) {
    if source_is_changing(state, source) {
        if state.settings.clear_credentials_on_source_change {
            clear_credentials_for_source(state, source);
        }
        state.catalog = None;
        state.inception.clear();
        clear_directory_ui(state);
        SendMessageW(state.controls.view, CB_RESETCONTENT, 0, 0);
        SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    }
    set_text(state.controls.source_path, &source.display().to_string());
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
    update_context_visibility(state);
    update_breadcrumb(state);
}

unsafe fn close_source_ui(state: &mut AppState) {
    state.entries.clear();
    populate_list(state);
    state.current_path = "/".to_owned();
    set_text(state.controls.path, "/");
    set_text(state.hwnd, APP_TITLE);
    set_source_enabled(state, false);
    update_mode(state);
    set_status(state, "Ready");
    update_breadcrumb(state);
}

unsafe fn populate_volumes(state: &AppState, catalog: &InceptionCatalog, preferred: Option<usize>) {
    SendMessageW(state.controls.volume, CB_RESETCONTENT, 0, 0);
    let recommended = presentation::recommended_volume(&catalog.volumes);
    for (index, volume) in catalog.volumes.iter().enumerate() {
        let mut label = presentation::volume_title(volume);
        if recommended == Some(index) {
            label.push_str("  (recommended)");
        }
        let label = wide(&label);
        SendMessageW(
            state.controls.volume,
            CB_ADDSTRING,
            0,
            label.as_ptr() as isize,
        );
    }
    if let Some(selected) = preferred.or(recommended) {
        SendMessageW(state.controls.volume, CB_SETCURSEL, selected, 0);
    }
}

unsafe fn update_breadcrumb(state: &AppState) {
    let mut parts = Vec::new();
    if let Some(catalog) = &state.catalog {
        parts.push(source_title(state, &catalog.path));
    }
    for frame in &state.inception {
        parts.push(file_label(Path::new(&frame.catalog.image_path)));
    }
    if !state.current_path.is_empty() && state.current_path != "/" {
        parts.extend(
            state
                .current_path
                .split('/')
                .filter(|part| !part.is_empty())
                .map(str::to_owned),
        );
    }
    set_text(
        state.controls.breadcrumb,
        &if parts.is_empty() {
            String::new()
        } else {
            format!("Location:  {}", parts.join("  ›  "))
        },
    );
    EnableWindow(
        state.controls.up,
        (!state.busy && source_open(state) && state.current_path != "/") as i32,
    );
}

unsafe fn toggle_setting(state: &mut AppState, id: u16) {
    match id {
        ID_SETTING_AUTO_IMAGES => state.settings.auto_open_images ^= true,
        ID_SETTING_CLEAR_CREDENTIALS => state.settings.clear_credentials_on_source_change ^= true,
        ID_SETTING_CONFIRM_DRIVE => state.settings.confirm_physical_drive ^= true,
        ID_SETTING_ADVANCED_IMAGE_RANGE => state.settings.show_advanced_image_range ^= true,
        _ => return,
    }
    update_settings_menu(state);
    update_context_visibility(state);
    layout(state);
    if let Err(error) = save_settings(&state.settings) {
        show_problem(state.hwnd, &format!("Could not save settings: {error}"));
    } else {
        set_status(state, "Settings saved");
    }
}

fn time_zone_choices() -> Vec<String> {
    let mut zones: Vec<_> = chrono_tz::TZ_VARIANTS
        .iter()
        .map(|zone| zone.name().to_owned())
        .filter(|name| name != "UTC")
        .collect();
    zones.sort();
    zones.splice(0..0, ["local".into(), "UTC".into()]);
    zones
}

unsafe fn add_time_zone_menu(settings_menu: HMENU) {
    let menu = CreatePopupMenu();
    let mut groups = std::collections::BTreeMap::new();
    for (index, zone) in time_zone_choices().iter().enumerate() {
        let (parent, label) = match zone.as_str() {
            "local" => (menu, "Use this computer’s time zone (default)".to_owned()),
            "UTC" => (menu, "UTC".to_owned()),
            _ => {
                let (region, city) = zone.split_once('/').unwrap_or(("Other", zone));
                let parent = *groups.entry(region.to_owned()).or_insert_with(|| {
                    let child = CreatePopupMenu();
                    append_menu(menu, MF_POPUP, child as usize, region);
                    child
                });
                (parent, city.replace('_', " "))
            }
        };
        append_menu(
            parent,
            MF_STRING,
            usize::from(ID_TIME_ZONE_BASE) + index,
            &label,
        );
    }
    append_menu(
        settings_menu,
        MF_POPUP,
        menu as usize,
        "Restore point time zone",
    );
}

unsafe fn update_snapshot_labels(state: &AppState) {
    let Some(catalog) = &state.catalog else {
        return;
    };
    let selected = SendMessageW(state.controls.view, CB_GETCURSEL, 0, 0);
    let zone: SnapshotTimeZone = state
        .settings
        .snapshot_time_zone
        .parse()
        .unwrap_or_default();
    SendMessageW(state.controls.view, CB_RESETCONTENT, 0, 0);
    for view in &catalog.views {
        let text = wide(&presentation::view_line(view, zone));
        SendMessageW(state.controls.view, CB_ADDSTRING, 0, text.as_ptr() as isize);
    }
    if selected >= 0 {
        SendMessageW(state.controls.view, CB_SETCURSEL, selected as usize, 0);
    }
}

unsafe fn update_settings_menu(state: &AppState) {
    let menu = GetMenu(state.hwnd);
    for (index, zone) in time_zone_choices().iter().enumerate() {
        CheckMenuItem(
            menu,
            u32::from(ID_TIME_ZONE_BASE) + index as u32,
            MF_BYCOMMAND
                | if *zone == state.settings.snapshot_time_zone {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                },
        );
    }
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
        (
            ID_SETTING_ADVANCED_IMAGE_RANGE,
            state.settings.show_advanced_image_range,
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
    // The folder keeps its original name so existing settings carry over.
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
    let control = |key: u8, cmd: u16| ACCEL {
        fVirt: FVIRTKEY | FCONTROL,
        key: key as u16,
        cmd,
    };
    let mut entries = [
        control(b'O', ID_OPEN_BACKUP),
        ACCEL {
            fVirt: FVIRTKEY | FCONTROL | FSHIFT,
            key: b'O' as u16,
            cmd: ID_OPEN_IMAGE,
        },
        control(b'D', ID_OPEN_DRIVE),
        control(b'W', ID_CLOSE_SOURCE),
        control(b'K', ID_UNLOCK),
        control(b'E', ID_RESTORE),
        control(b'S', ID_RESTORE),
        control(b'I', ID_INCEPTION),
        control(b'U', ID_UPDATE),
        control(b'L', ID_FOCUS_PATH),
        ACCEL {
            fVirt: FVIRTKEY | windows_sys::Win32::UI::WindowsAndMessaging::FALT,
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
    let mut status_rect = RECT::default();
    GetWindowRect(state.controls.status, &mut status_rect);
    let scale = |value: i32| value * state.dpi / 96;
    let pad = scale(18);
    let gap = scale(8);
    let row = scale(32);
    let width = client.right - client.left;
    let height = client.bottom - client.top - (status_rect.bottom - status_rect.top);
    let controls = &state.controls;

    // Header: app icon (painted), backup name and description, open/close actions.
    let header = scale(68);
    state.header_height.set(header);
    let mut right = width - pad;
    let button_y = (header - row) / 2;
    for (control, button_width) in [
        (controls.open_backup, scale(140)),
        (controls.open_drive, scale(130)),
        (controls.close_source, scale(90)),
    ] {
        right -= button_width;
        MoveWindow(control, right, button_y, button_width, row, 1);
        right -= gap;
    }
    let text_x = scale(18 + 36 + 14);
    let text_width = (right - text_x - gap).max(scale(120));
    MoveWindow(
        controls.header_title,
        text_x,
        scale(12),
        text_width,
        scale(28),
        1,
    );
    MoveWindow(
        controls.header_detail,
        text_x,
        scale(40),
        text_width,
        scale(20),
        1,
    );

    if !source_open(state) {
        let column = (width - pad * 2).min(scale(640));
        let x = (width - column) / 2;
        let content = scale(46 + 64 + 3 * 80 + 36);
        let mut y = header + ((height - header - content) / 2).max(scale(28));
        MoveWindow(controls.welcome_heading, x, y, column, scale(46), 1);
        y += scale(50);
        MoveWindow(controls.welcome_body, x, y, column, scale(48), 1);
        y += scale(64);
        for link in [
            controls.welcome_file,
            controls.welcome_drive,
            controls.welcome_path,
        ] {
            MoveWindow(link, x, y, column, scale(70), 1);
            y += scale(80);
        }
        MoveWindow(
            controls.welcome_steps,
            x,
            y + scale(8),
            column,
            scale(24),
            1,
        );
        return;
    }

    let label_width = scale(130);
    let mut y = header + scale(14);
    if state.catalog.is_some() {
        MoveWindow(
            controls.view_label,
            pad,
            y + scale(7),
            label_width,
            scale(22),
            1,
        );
        MoveWindow(
            controls.view,
            pad + label_width,
            y,
            width - pad * 2 - label_width,
            scale(400),
            1,
        );
        y += row + gap;
    }
    if !state.inception.is_empty() {
        let back_width = scale(220);
        MoveWindow(
            controls.volume_label,
            pad,
            y + scale(7),
            label_width,
            scale(22),
            1,
        );
        MoveWindow(
            controls.volume,
            pad + label_width,
            y,
            width - pad * 2 - label_width - back_width - gap,
            scale(300),
            1,
        );
        MoveWindow(
            controls.leave_image,
            width - pad - back_width,
            y,
            back_width,
            row,
            1,
        );
        y += row + gap;
    }
    let up_width = scale(86);
    let go_width = scale(60);
    MoveWindow(controls.up, pad, y, up_width, row, 1);
    MoveWindow(
        controls.path,
        pad + up_width + gap,
        y + scale(1),
        width - pad * 2 - up_width - go_width - gap * 2,
        row - scale(2),
        1,
    );
    MoveWindow(controls.go, width - pad - go_width, y, go_width, row, 1);
    y += row + scale(6);
    MoveWindow(controls.breadcrumb, pad, y, width - pad * 2, scale(20), 1);
    y += scale(28);

    if state.guide.get() != Guide::None {
        let guide_height = scale(52);
        let action_width = if get_text(controls.guide_action).is_empty() {
            0
        } else {
            scale(160)
        };
        state.guide_rect.set(RECT {
            left: pad,
            top: y,
            right: width - pad,
            bottom: y + guide_height,
        });
        MoveWindow(
            controls.guide_text,
            pad + scale(18),
            y + scale(16),
            width - pad * 2 - scale(30) - action_width - gap,
            scale(22),
            1,
        );
        if action_width > 0 {
            MoveWindow(
                controls.guide_action,
                width - pad - scale(12) - action_width,
                y + scale(10),
                action_width,
                row,
                1,
            );
        }
        y += guide_height + gap;
    }

    if state.settings.show_advanced_image_range {
        MoveWindow(
            controls.image_range_label,
            pad,
            y + scale(7),
            label_width,
            scale(22),
            1,
        );
        MoveWindow(
            controls.image_offset,
            pad + label_width,
            y,
            scale(150),
            row,
            1,
        );
        MoveWindow(
            controls.image_length,
            pad + label_width + scale(158),
            y,
            scale(170),
            row,
            1,
        );
        y += row + gap;
    }

    let actions_y = height - pad - row;
    let restore_width = scale(130);
    let image_width = scale(150);
    MoveWindow(
        controls.restore,
        width - pad - restore_width,
        actions_y,
        restore_width,
        row,
        1,
    );
    MoveWindow(
        controls.inception,
        width - pad - restore_width - gap - image_width,
        actions_y,
        image_width,
        row,
        1,
    );
    MoveWindow(
        controls.selection,
        pad,
        actions_y + scale(7),
        width - pad * 2 - restore_width - image_width - gap * 3,
        scale(22),
        1,
    );
    let list_height = (actions_y - gap - y).max(scale(120));
    let list_width = width - pad * 2;
    MoveWindow(controls.list, pad, y, list_width, list_height, 1);
    let type_width = scale(140);
    let size_width = scale(110);
    let scrollbar = GetSystemMetrics(2); // SM_CXVSCROLL
    SendMessageW(controls.list, LVM_SETCOLUMNWIDTH, 1, type_width as isize);
    SendMessageW(controls.list, LVM_SETCOLUMNWIDTH, 2, size_width as isize);
    SendMessageW(
        controls.list,
        LVM_SETCOLUMNWIDTH,
        0,
        (list_width - type_width - size_width - scrollbar - scale(6)).max(scale(200)) as isize,
    );
}

pub(crate) unsafe fn control(
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

pub(crate) unsafe fn button(
    parent: HWND,
    instance: HINSTANCE,
    text: &str,
    id: u16,
    default: bool,
) -> HWND {
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

unsafe fn command_link(parent: HWND, instance: HINSTANCE, text: &str, note: &str, id: u16) -> HWND {
    let link = control(
        parent,
        instance,
        windows_sys::core::w!("BUTTON"),
        text,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_COMMANDLINK as u32,
        0,
        id,
    );
    let note = wide(note);
    SendMessageW(link, BCM_SETNOTE, 0, note.as_ptr() as isize);
    link
}

fn all_controls(controls: &Controls) -> [HWND; 31] {
    [
        controls.source_path,
        controls.header_title,
        controls.header_detail,
        controls.open_backup,
        controls.open_drive,
        controls.close_source,
        controls.welcome_heading,
        controls.welcome_body,
        controls.welcome_file,
        controls.welcome_drive,
        controls.welcome_path,
        controls.welcome_steps,
        controls.view_label,
        controls.view,
        controls.volume_label,
        controls.volume,
        controls.leave_image,
        controls.up,
        controls.path,
        controls.go,
        controls.breadcrumb,
        controls.guide_text,
        controls.guide_action,
        controls.image_range_label,
        controls.image_offset,
        controls.image_length,
        controls.list,
        controls.selection,
        controls.inception,
        controls.restore,
        controls.status,
    ]
}

unsafe fn add_column(list: HWND, index: usize, text: &str, width: i32, format: i32) {
    let text = wide(text);
    let mut column = LVCOLUMNW {
        mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
        fmt: format,
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
    Backup,
    All,
}

unsafe fn open_file_dialog(owner: HWND, title: &str, filter: SourceFilter) -> Option<PathBuf> {
    file_dialog(owner, title, filter, None, false)
}

unsafe fn save_extraction_dialog(owner: HWND, name: &str, directory: bool) -> Option<PathBuf> {
    file_dialog(
        owner,
        if directory {
            "Restore folder — choose where to save the copy"
        } else {
            "Restore file — choose where to save the copy"
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
            "ZFS send files",
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
        SourceFilter::Backup => wide_nul_groups(&[
            "Backups and disk images",
            "*.zfs;*.zstream;*.send;*.img;*.raw;*.dd;*.qcow;*.qcow2;*.vmdk;*.datto;*.detto",
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
    // Restores start on the Desktop; Windows then remembers the user's choice.
    let desktop = if save { desktop_folder() } else { None };
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrInitialDir: desktop.as_ref().map_or(null(), |path| path.as_ptr()),
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

fn desktop_folder() -> Option<Vec<u16>> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Desktop, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: the shell allocates the returned string, which is copied and
    // then released with CoTaskMemFree as documented.
    unsafe {
        let mut path = null_mut();
        if SHGetKnownFolderPath(
            &FOLDERID_Desktop,
            KF_FLAG_DEFAULT as u32,
            null_mut(),
            &mut path,
        ) < 0
            || path.is_null()
        {
            return None;
        }
        let length = (0..).take_while(|&index| *path.add(index) != 0).count();
        let mut value = std::slice::from_raw_parts(path, length).to_vec();
        value.push(0);
        CoTaskMemFree(path.cast());
        Some(value)
    }
}

unsafe fn show_how_to(owner: HWND) {
    show_message(
        owner,
        DialogIcon::Information,
        "How to restore files",
        "1.  Open your backup: choose Open backup for a file, or Open drive for a drive from a Slide or Datto device.\n\n2.  Pick a restore point: the newest one is chosen for you. Use the Restore point list for an earlier date.\n\n3.  Unlock it if asked, with the key from your Slide console or its key file.\n\n4.  Open the disk image if the guide suggests it, then double-click folders to find your files.\n\n5.  Select a file or folder and choose Restore. Pick a place to save the copy.\n\nYour backup is only read. ZFS Explore never changes it.",
    );
}

unsafe fn show_shortcuts(owner: HWND) {
    show_message(
        owner,
        DialogIcon::Information,
        "Keyboard shortcuts",
        "Ctrl+O    Open a backup file\nCtrl+D    Open a connected drive\nCtrl+Shift+O    Open a disk image file\nCtrl+W    Close the backup\nCtrl+K    Unlock this restore point\nCtrl+L    Go to a folder\nEnter    Open a folder or disk image\nBackspace or Alt+Up    Go up one folder\nCtrl+I    Open the selected file as a disk image\nEsc    Leave the current disk image\nCtrl+S or Ctrl+E    Restore the selected file or folder\nCtrl+U    Update a restored file from an incremental send\nF5    Refresh",
    );
}

unsafe fn show_about(owner: HWND) {
    show_message(
        owner,
        DialogIcon::Information,
        &format!("ZFS Explore {}", env!("CARGO_PKG_VERSION")),
        "Recover files from ZFS send files, Slide backups, Datto Reverse RoundTrip drives, exported pool disks and disk images, without installing ZFS or mounting anything.\n\nBackups are always opened read-only. Keys stay in memory only while the app is open and are never saved.",
    );
}

fn trace_startup(message: &str) {
    if let Some(path) = std::env::var_os("ZFSE_UI_TRACE") {
        let _ = fs::write(path, message);
    }
}

pub(crate) fn wide(value: &str) -> Vec<u16> {
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

/// A drive chosen from the picker keeps its model name; files show their name.
fn source_title(state: &AppState, path: &Path) -> String {
    state
        .drive_title
        .as_ref()
        .filter(|(drive, _)| drive == path)
        .map_or_else(|| file_label(path), |(_, title)| title.clone())
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into(),
    )
}

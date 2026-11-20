//! Native modal checklist for attached backup disks.
use super::*;
use windows_sys::Win32::Storage::FileSystem::{
    BusType1394, BusTypeFileBackedVirtual, BusTypeMmc, BusTypeNvme, BusTypeSas, BusTypeSata,
    BusTypeSd, BusTypeUsb, BusTypeVirtual,
};
use windows_sys::Win32::System::Ioctl::GUID_DEVINTERFACE_DISK;
use windows_sys::Win32::UI::Controls::{
    BST_CHECKED, LVIS_STATEIMAGEMASK, LVM_ENSUREVISIBLE, LVM_GETITEMSTATE, LVM_GETSTRINGWIDTHW,
    LVNI_FOCUSED, LVS_EX_CHECKBOXES, NMLISTVIEW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, VK_F5};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVNODES_CHANGED,
    DBT_DEVTYP_DEVICEINTERFACE, DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_WINDOW_HANDLE,
    DialogBoxParamW, EndDialog, GetDlgItem, HDEVNOTIFY, IDCANCEL, IDOK, KillTimer,
    RegisterDeviceNotificationW, SetTimer, UnregisterDeviceNotification, WM_DEVICECHANGE,
    WM_INITDIALOG, WM_SETREDRAW, WM_TIMER,
};

const DIALOG: u16 = 210;
const LIST: i32 = 2101;
const SYSTEM: i32 = 2102;
const REFRESH: i32 = 2103;
const STATUS: i32 = 2104;
const EMPTY: i32 = 2105;
const REFRESH_TIMER: usize = 1;
// A list view's one-based state-image indices are 1 (unchecked) and 2 (checked).
const CHECKED: u32 = 2 << 12;

#[allow(non_upper_case_globals)]
fn connection(drive: &PhysicalDrive) -> &str {
    match drive.bus {
        BusTypeUsb => "USB",
        BusTypeSd => "SD card",
        BusTypeMmc => "MMC",
        BusType1394 => "FireWire",
        BusTypeVirtual | BusTypeFileBackedVirtual => "Virtual disk",
        BusTypeNvme => "NVMe",
        BusTypeSas => "SAS",
        BusTypeSata => "SATA",
        _ if drive.removable => "Removable",
        _ => "",
    }
}

#[allow(non_upper_case_globals)]
fn visible_drives(mut drives: Vec<PhysicalDrive>, show_system: bool) -> Vec<PhysicalDrive> {
    drives.retain(|drive| show_system || !drive.system);
    drives.sort_by_key(|drive| {
        let external = drive.removable || matches!(drive.bus, BusTypeUsb | BusTypeSd | BusType1394);
        (drive.system, !drive.zfs_partition, !external, drive.number)
    });
    drives
}

fn row_label(drive: &PhysicalDrive) -> String {
    let mut parts = vec![drive.title()];
    for detail in [
        connection(drive),
        if drive.zfs_partition {
            "ZFS partition"
        } else {
            ""
        },
        if drive.system {
            "Windows system disk"
        } else if !drive.system_known {
            "System disk status unknown"
        } else {
            ""
        },
    ] {
        if !detail.is_empty() {
            parts.push(detail.to_owned());
        }
    }
    let serial: Vec<char> = drive
        .serial
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if !serial.is_empty() {
        let suffix: String = serial[serial.len().saturating_sub(6)..].iter().collect();
        parts.push(format!("Serial ending {suffix}"));
    }
    parts.join(" · ")
}

// Disk numbers can be reused. Carry checks over a refresh only for uniquely
// identified drives whose paths and reported metadata still match.
fn identified_in(drive: &PhysicalDrive, current: &[PhysicalDrive]) -> bool {
    drive.identity.is_some()
        && current
            .iter()
            .filter(|item| item.identity == drive.identity)
            .count()
            == 1
        && current.contains(drive)
}

fn can_open(selected: &[PhysicalDrive], inventory: &[PhysicalDrive]) -> bool {
    match selected {
        [] => false,
        [one] => inventory.contains(one),
        more => more.iter().all(|drive| identified_in(drive, inventory)),
    }
}

#[derive(Default)]
struct Picker {
    rows: RefCell<Vec<PhysicalDrive>>,
    updating: Cell<bool>,
    notification: Cell<HDEVNOTIFY>,
    pending_change: Cell<bool>,
    review_required: Cell<bool>,
    result: RefCell<Option<Vec<PhysicalDrive>>>,
}

pub(super) unsafe fn choose(owner: HWND) -> Option<Vec<PhysicalDrive>> {
    let state = Picker::default();
    let result = DialogBoxParamW(
        GetModuleHandleW(null()),
        DIALOG as usize as *const u16,
        owner,
        Some(dialog_proc),
        &state as *const Picker as isize,
    );
    if result == -1 {
        show_problem(
            owner,
            &format!(
                "Could not open the connected drives list: {}",
                std::io::Error::last_os_error()
            ),
        );
    }
    state.result.into_inner()
}

unsafe fn checked(dialog: HWND, state: &Picker) -> Vec<PhysicalDrive> {
    let list = GetDlgItem(dialog, LIST);
    state
        .rows
        .borrow()
        .iter()
        .enumerate()
        .filter_map(|(row, drive)| {
            let flags = SendMessageW(list, LVM_GETITEMSTATE, row, LVIS_STATEIMAGEMASK as isize);
            (flags as u32 & LVIS_STATEIMAGEMASK == CHECKED).then(|| drive.clone())
        })
        .collect()
}

unsafe fn update_status(dialog: HWND, state: &Picker) {
    let selected = checked(dialog, state);
    let allowed = can_open(&selected, &state.rows.borrow());
    EnableWindow(
        GetDlgItem(dialog, IDOK),
        i32::from(allowed && !state.pending_change.get() && !state.review_required.get()),
    );
    if state.review_required.get() {
        set_text(
            GetDlgItem(dialog, STATUS),
            "A checked drive is no longer available. Check your drives again before opening.",
        );
        return;
    }
    let status = match selected.as_slice() {
        [] => String::new(),
        [_] => "1 drive selected".to_owned(),
        _ if !allowed => "Windows cannot identify every selected disk uniquely. Open one disk or use offline member images.".to_owned(),
        more => format!("{} drives selected", more.len()),
    };
    set_text(GetDlgItem(dialog, STATUS), &status);
}

unsafe fn refresh(dialog: HWND, state: &Picker, inventory: Vec<PhysicalDrive>) {
    state.pending_change.set(false);
    KillTimer(dialog, REFRESH_TIMER);
    let list = GetDlgItem(dialog, LIST);
    let selected = checked(dialog, state);
    let focused_index = SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_FOCUSED as isize);
    let focused = state.rows.borrow().get(focused_index as usize).cloned();
    let show_system =
        SendMessageW(GetDlgItem(dialog, SYSTEM), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
    let any_system = inventory.iter().any(|disk| disk.system);
    let rows = visible_drives(inventory, show_system);
    let kept: Vec<_> = selected
        .iter()
        .filter(|old| identified_in(old, &rows))
        .collect();
    let mut focus_row = 0;
    state.updating.set(true);
    SendMessageW(list, WM_SETREDRAW, 0, 0);
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    let dpi = GetDpiForWindow(dialog).max(96) as i32;
    let mut rect = RECT::default();
    GetClientRect(list, &mut rect);
    let mut width = rect.right.saturating_sub(22 * dpi / 96);
    for (index, drive) in rows.iter().enumerate() {
        let mut text = wide(&row_label(drive));
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: index as i32,
            pszText: text.as_mut_ptr(),
            ..Default::default()
        };
        SendMessageW(list, LVM_INSERTITEMW, 0, &mut item as *mut LVITEMW as isize);
        if kept.contains(&drive) {
            item.state = CHECKED;
            item.stateMask = LVIS_STATEIMAGEMASK;
            SendMessageW(
                list,
                LVM_SETITEMSTATE,
                index,
                &item as *const LVITEMW as isize,
            );
        }
        if Some(drive) == focused.as_ref() {
            focus_row = index;
        }
        let length = SendMessageW(list, LVM_GETSTRINGWIDTHW, 0, text.as_ptr() as isize) as i32;
        width = width.max(length + 38 * dpi / 96);
    }
    SendMessageW(list, LVM_SETCOLUMNWIDTH, 0, width as isize);
    let empty = rows.is_empty();
    if !empty {
        let focus = LVITEMW {
            state: LVIS_FOCUSED,
            stateMask: LVIS_FOCUSED | LVIS_SELECTED,
            ..Default::default()
        };
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            focus_row,
            &focus as *const LVITEMW as isize,
        );
        SendMessageW(list, LVM_ENSUREVISIBLE, focus_row, 0);
    }
    *state.rows.borrow_mut() = rows;
    SendMessageW(list, WM_SETREDRAW, 1, 0);
    if empty && GetFocus() == list {
        SetFocus(GetDlgItem(dialog, REFRESH));
    }
    ShowWindow(list, if empty { SW_HIDE } else { SW_SHOW });
    ShowWindow(
        GetDlgItem(dialog, EMPTY),
        if empty { SW_SHOW } else { SW_HIDE },
    );
    if empty {
        let text = if any_system && !show_system {
            "No backup drives found. Connect a drive and choose Refresh. The Windows system disk is hidden."
        } else {
            "No drives found. Connect a backup drive and choose Refresh."
        };
        set_text(GetDlgItem(dialog, EMPTY), text);
    }
    state.updating.set(false);
    InvalidateRect(list, null(), 1);
    if kept.len() != selected.len() {
        state.review_required.set(true);
    }
    update_status(dialog, state);
}

unsafe fn open(dialog: HWND, state: &Picker) {
    let selected = checked(dialog, state);
    if selected.is_empty() || state.review_required.get() || state.pending_change.get() {
        return;
    }
    if selected
        .iter()
        .any(|disk| disk.system || !disk.system_known)
    {
        let names = selected
            .iter()
            .filter(|disk| disk.system || !disk.system_known)
            .map(PhysicalDrive::title)
            .collect::<Vec<_>>()
            .join("\n");
        let body =
            format!("{names}\n\nContinue only if this disk holds the backup you want to read.");
        let title = if selected.iter().any(|disk| disk.system) {
            "Open the Windows system disk?"
        } else {
            "Windows could not identify its system disk"
        };
        if task_dialog(
            dialog,
            &Task {
                actions: &[(
                    "Open read-only",
                    "Read the selected drives without changing them.",
                )],
                ..Task::new(DialogIcon::Warning, title, &body)
            },
        ) != Some(0)
        {
            return;
        }
    }
    // Query again after any confirmation. Never substitute a drive or proceed
    // with a subset if something changed while the dialog was open.
    let current = enumerate_physical_drives();
    if state.pending_change.get()
        || checked(dialog, state) != selected
        || !can_open(&selected, &current)
    {
        refresh(dialog, state, current);
        return;
    }
    *state.result.borrow_mut() = Some(selected);
    EndDialog(dialog, IDOK as isize);
}

unsafe extern "system" fn dialog_proc(dialog: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        SetWindowLongPtrW(dialog, GWLP_USERDATA, lp);
    }
    let Some(state) = (GetWindowLongPtrW(dialog, GWLP_USERDATA) as *const Picker).as_ref() else {
        return 0;
    };
    match msg {
        WM_INITDIALOG => {
            let list = GetDlgItem(dialog, LIST);
            let styles =
                LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_LABELTIP | LVS_EX_DOUBLEBUFFER;
            SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, styles as isize);
            let column = LVCOLUMNW {
                mask: LVCF_WIDTH,
                cx: 300,
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                0,
                &column as *const LVCOLUMNW as isize,
            );
            let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
                dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
                dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE,
                dbcc_classguid: GUID_DEVINTERFACE_DISK,
                ..Default::default()
            };
            state.notification.set(RegisterDeviceNotificationW(
                dialog,
                &filter as *const _ as *const c_void,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            ));
            refresh(dialog, state, enumerate_physical_drives());
            SetFocus(if state.rows.borrow().is_empty() {
                GetDlgItem(dialog, REFRESH)
            } else {
                list
            });
            return 0; // Focus was assigned explicitly.
        }
        WM_DEVICECHANGE
            if matches!(
                wp as u32,
                DBT_DEVNODES_CHANGED | DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE
            ) =>
        {
            state.pending_change.set(true);
            EnableWindow(GetDlgItem(dialog, IDOK), 0);
            if SetTimer(dialog, REFRESH_TIMER, 175, None) == 0 {
                refresh(dialog, state, enumerate_physical_drives());
            }
        }
        WM_TIMER if wp == REFRESH_TIMER => refresh(dialog, state, enumerate_physical_drives()),
        WM_COMMAND => match (wp & 0xffff) as i32 {
            IDOK => open(dialog, state),
            IDCANCEL => {
                EndDialog(dialog, IDCANCEL as isize);
            }
            REFRESH | SYSTEM => refresh(dialog, state, enumerate_physical_drives()),
            _ => return 0,
        },
        WM_NOTIFY if !state.updating.get() && lp != 0 => {
            let event = &*(lp as *const NMHDR);
            if event.idFrom != LIST as usize {
                return 0;
            }
            if event.code == LVN_ITEMCHANGED {
                let change = &*(lp as *const NMLISTVIEW);
                if (change.uOldState ^ change.uNewState) & LVIS_STATEIMAGEMASK != 0 {
                    state.review_required.set(false);
                    update_status(dialog, state);
                }
            } else if event.code == LVN_KEYDOWN && (*(lp as *const NMLVKEYDOWN)).wVKey == VK_F5 {
                refresh(dialog, state, enumerate_physical_drives());
            }
        }
        WM_DESTROY => {
            KillTimer(dialog, REFRESH_TIMER);
            let handle = state.notification.replace(null_mut());
            if !handle.is_null() {
                UnregisterDeviceNotification(handle);
            }
        }
        _ => return 0,
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(number: u32, id: Option<&str>) -> PhysicalDrive {
        PhysicalDrive {
            identity: id.map(str::to_owned),
            path: PathBuf::from(format!(r"\\.\PhysicalDrive{number}")),
            number,
            model: "Backup drive".into(),
            size: Some(1 << 30),
            bus: BusTypeSata,
            removable: false,
            serial: String::new(),
            system: false,
            system_known: true,
            zfs_partition: false,
        }
    }

    #[test]
    fn filters_the_system_disk_and_orders_zfs_and_external_candidates_first() {
        let mut system = drive(0, Some("windows"));
        system.system = true;
        let mut external = drive(9, None);
        external.bus = BusTypeUsb;
        let mut zfs = drive(12, Some("pool"));
        zfs.zfs_partition = true;
        zfs.serial = "012345678".into();
        let inventory = vec![system.clone(), drive(3, None), external, zfs.clone()];
        assert_eq!(
            visible_drives(inventory.clone(), false)
                .iter()
                .map(|d| d.number)
                .collect::<Vec<_>>(),
            [12, 9, 3]
        );
        assert_eq!(visible_drives(inventory, true).last(), Some(&system));
        assert!(row_label(&zfs).contains("ZFS partition"));
        assert!(row_label(&zfs).contains("Serial ending 345678"));
    }

    #[test]
    fn never_keeps_a_reused_number_or_ambiguous_identity_after_a_refresh() {
        let chosen = drive(1, Some("first"));
        let another = drive(2, Some("second"));
        let unidentified = drive(3, None);
        let current = [chosen.clone(), another.clone(), unidentified.clone()];
        assert!(identified_in(&chosen, &current));
        assert!(!identified_in(&drive(1, Some("gone")), &current));
        assert!(!identified_in(&unidentified, &current));
        assert!(!identified_in(
            &chosen,
            &[chosen.clone(), drive(2, Some("first"))]
        ));
        assert!(can_open(std::slice::from_ref(&unidentified), &current));
        assert!(!can_open(&[chosen.clone(), unidentified], &current));
        assert!(can_open(&[chosen, another], &current));
        assert!(!can_open(&[], &current));
    }
}

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[path = "zfs-send-explore-windows/mod.rs"]
mod zfs_send_explore_windows;

#[cfg(windows)]
fn main() {
    if let Err(error) = zfs_send_explore_windows::run() {
        zfs_send_explore_windows::show_fatal_error(&format!("{error:#}"));
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("zfs-send-explore-windows is a native Windows application; build it on Windows");
}

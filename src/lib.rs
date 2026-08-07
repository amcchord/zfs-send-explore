//! Pure-userspace browsing and extraction for ZFS send streams and offline
//! pool members.

mod compression;

pub mod client;
pub mod encrypted;
pub mod filesystem;
pub mod operations;
pub mod pool;
pub mod sparse;
pub mod stream;

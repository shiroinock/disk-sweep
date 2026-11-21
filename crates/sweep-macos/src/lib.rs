//! macOS platform layer: APFS-aware capacity, snapshots, NSWorkspace running apps,
//! TCC-aware hints, multi-user homes and an FSEvents change journal for fast rescans.

#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
mod imp;

#[cfg(target_os = "macos")]
pub use imp::MacPlatform;

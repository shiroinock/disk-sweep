use crate::model::{DenyReason, Snapshot, VolumeInfo};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Running apps and processes, matched against `blocked_by_apps` / `blocked_by_process` in rules.
#[derive(Debug, Default, Clone)]
pub struct Running {
    /// App identifiers such as bundle IDs.
    pub app_ids: BTreeSet<String>,
    /// Executable paths.
    pub processes: Vec<String>,
}

impl Running {
    pub fn matches(&self, app_ids: &[String], process_hints: &[String]) -> Vec<String> {
        let mut hit = Vec::new();
        for id in app_ids {
            if self.app_ids.contains(id) {
                hit.push(id.clone());
            }
        }
        for hint in process_hints {
            if self.processes.iter().any(|p| p.contains(hint.as_str())) {
                hit.push(hint.clone());
            }
        }
        hit.sort();
        hit.dedup();
        hit
    }
}

/// Directories reported as changed by a change journal since a token.
#[derive(Debug, Default, Clone)]
pub struct Changes {
    /// Directories whose direct entries changed.
    pub dirs: BTreeSet<PathBuf>,
    /// Directories whose whole subtree must be rescanned (journal dropped events).
    pub recursive: BTreeSet<PathBuf>,
}

/// A filesystem change journal (FSEvents on macOS). Lets a rescan skip untouched subtrees.
pub trait ChangeJournal: Send + Sync {
    /// Opaque position to store with an index.
    fn token(&self, root: &Path) -> Option<String>;
    /// Changes under `root` since `token`, or None if the history is unavailable.
    fn changes_since(&self, root: &Path, token: &str) -> Option<Changes>;
}

/// Per-OS hooks. `GenericPlatform` is the portable default; macOS lives in the `sweep-macos` crate.
pub trait Platform: Send + Sync {
    fn name(&self) -> &'static str;

    /// Roots of the volume(s) holding user data (on macOS: /System/Volumes/Data).
    fn data_roots(&self) -> Vec<PathBuf> {
        vec![PathBuf::from("/")]
    }

    fn volume(&self, path: &Path) -> Option<VolumeInfo> {
        statvfs_volume(path)
    }

    fn journal(&self) -> Option<&dyn ChangeJournal> {
        None
    }

    fn snapshots(&self) -> Vec<Snapshot> {
        Vec::new()
    }

    fn running(&self) -> Running {
        Running {
            app_ids: BTreeSet::new(),
            processes: ps_processes(),
        }
    }

    /// User home directories on this machine.
    fn user_homes(&self) -> Vec<PathBuf> {
        list_dirs(Path::new("/home"))
    }

    /// Explanation for an unreadable path, plus a command the human can run themselves.
    fn deny_hint(&self, path: &Path, reason: DenyReason) -> (String, Option<String>) {
        match reason {
            DenyReason::Permission => (
                "Blocked by Unix permissions. A human can measure it with sudo.".into(),
                Some(format!(
                    "sudo disk-sweep scan {} --depth 2 > disk-sweep-scan.json",
                    shell_quote(path)
                )),
            ),
            DenyReason::Privacy => ("Blocked by OS privacy protection.".into(), None),
            DenyReason::Other => ("Read failed.".into(), None),
        }
    }
}

pub struct GenericPlatform;
impl Platform for GenericPlatform {
    fn name(&self) -> &'static str {
        "generic"
    }
}

#[allow(clippy::unnecessary_cast)] // field widths differ between platforms
pub fn statvfs_volume(path: &Path) -> Option<VolumeInfo> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u64;
    let total = s.f_blocks as u64 * frsize;
    let free = s.f_bavail as u64 * frsize;
    Some(VolumeInfo {
        mount: path.to_path_buf(),
        total_bytes: total,
        free_bytes: free,
        available_important_bytes: None,
        used_bytes: total.saturating_sub(s.f_bfree as u64 * frsize),
    })
}

pub fn ps_processes() -> Vec<String> {
    Command::new("ps")
        .args(["-axo", "comm="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub fn list_dirs(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .filter(|p| {
                    !p.file_name()
                        .map(|n| n.to_string_lossy().starts_with('.'))
                        .unwrap_or(true)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn shell_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub fn current_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

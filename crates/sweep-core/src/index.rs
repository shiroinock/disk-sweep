//! Persistent scan index, in the spirit of ncdu's export and the incremental rescans of
//! GUI disk analysers. Each directory above a size threshold keeps its totals so a later
//! scan can skip whole subtrees that a change journal (FSEvents on macOS) says are
//! untouched, or skip per-file stats where the directory mtime is unchanged.

use crate::model::DenyReason;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub const INDEX_VERSION: u32 = 1;
/// Directories smaller than this are not stored; they are cheap to rescan.
pub const MIN_RECORD_BYTES: u64 = 1 << 20;
/// Files at least this large are tracked for the "largest files" list.
pub const BIG_FILE_BYTES: u64 = 50 << 20;
pub const TOP_FILES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    pub root: PathBuf,
    pub created_unix: u64,
    /// Effective uid the index was built with: a root scan sees more than a user scan.
    pub euid: u32,
    /// Change-journal position taken when the scan started (FSEvents event id + volume UUID).
    pub journal_token: Option<String>,
    pub dirs: HashMap<PathBuf, DirRec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirRec {
    pub mtime_ns: i64,
    pub direct_bytes: u64,
    pub direct_files: u64,
    pub total_bytes: u64,
    pub total_files: u64,
    /// Names of child directories (all of them, stored or not).
    pub subdirs: Vec<String>,
    /// Largest files anywhere in this subtree (absolute paths), descending.
    pub top_files: Vec<(PathBuf, u64)>,
    /// Unreadable paths found inside this subtree.
    pub denied: Vec<(PathBuf, DenyReason)>,
}

pub fn cache_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("DISK_SWEEP_CACHE_DIR") {
        return PathBuf::from(x);
    }
    // A root scan must not drop root-owned files into a normal user's cache.
    if crate::platform::is_root() {
        return if cfg!(target_os = "macos") {
            PathBuf::from("/var/root/Library/Caches/disk-sweep")
        } else {
            PathBuf::from("/root/.cache/disk-sweep")
        };
    }
    let home = crate::platform::current_home();
    if cfg!(target_os = "macos") {
        home.join("Library/Caches/disk-sweep")
    } else if let Some(x) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(x).join("disk-sweep")
    } else {
        home.join(".cache/disk-sweep")
    }
}

pub fn index_path(root: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    unsafe { libc::geteuid() }.hash(&mut h);
    cache_dir().join(format!("index-{:016x}.bin", h.finish()))
}

pub fn load(root: &Path) -> Option<Index> {
    let bytes = std::fs::read(index_path(root)).ok()?;
    let idx: Index = bincode::deserialize(&bytes).ok()?;
    (idx.version == INDEX_VERSION && idx.root == root && idx.euid == unsafe { libc::geteuid() })
        .then_some(idx)
}

pub fn save(idx: &Index) -> std::io::Result<PathBuf> {
    let path = index_path(&idx.root);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = bincode::serialize(idx).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Drop records whose parent no longer lists them (directories that disappeared).
pub fn prune(dirs: &mut HashMap<PathBuf, DirRec>, root: &Path) {
    let mut keys: Vec<PathBuf> = dirs.keys().cloned().collect();
    keys.sort();
    let mut alive: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for k in keys {
        let keep = k == root
            || k.parent().is_some_and(|p| {
                alive.contains(p)
                    && dirs.get(p).is_some_and(|r| {
                        k.file_name().is_some_and(|n| {
                            r.subdirs.iter().any(|s| s.as_str() == n.to_string_lossy())
                        })
                    })
            });
        if keep {
            alive.insert(k);
        } else {
            dirs.remove(&k);
        }
    }
}

pub fn merge_top(into: &mut Vec<(PathBuf, u64)>, from: &[(PathBuf, u64)]) {
    into.extend_from_slice(from);
    into.sort_by_key(|x| std::cmp::Reverse(x.1));
    into.truncate(TOP_FILES);
}

use crate::index::{self, DirRec, Index, BIG_FILE_BYTES, MIN_RECORD_BYTES};
use crate::model::{DenyReason, ScanNode, ScanReport, Unmeasured};
use crate::platform::{Changes, Platform};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    /// Never read or write the index.
    Off,
    /// Reuse the index when it is younger than `max_age_secs`, then refresh it.
    Use,
    /// Ignore the stored index, scan everything, write a new one.
    Refresh,
}

pub struct ScanOptions {
    /// How many levels of children to keep in the output (sizes always cover the full tree).
    pub depth: usize,
    /// Children kept per level, largest first.
    pub top: usize,
    /// Do not cross into other filesystems.
    pub one_filesystem: bool,
    pub cache: CacheMode,
    pub max_age_secs: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            depth: 2,
            top: 15,
            one_filesystem: true,
            cache: CacheMode::Use,
            max_age_secs: 7 * 86400,
        }
    }
}

/// How the index was used, so an agent can judge freshness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheReport {
    /// "fresh" (full walk), "journal" (change journal since last scan) or "mtime" (directory mtimes).
    pub strategy: String,
    pub index_age_secs: Option<u64>,
    pub reused_dirs: u64,
    pub rescanned_dirs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

enum Strategy {
    Fresh,
    Journal(Changes),
    Mtime,
}

struct Ctx<'a> {
    opts: &'a ScanOptions,
    dev: u64,
    old: Option<&'a Index>,
    strategy: Strategy,
    seen_inodes: Mutex<HashSet<(u64, u64)>>,
    updates: Mutex<HashMap<PathBuf, DirRec>>,
    reused: AtomicU64,
    rescanned: AtomicU64,
}

/// Entries directly inside one directory.
struct Direct {
    bytes: u64,
    files: u64,
    nodes: Vec<ScanNode>,
    top_files: Vec<(PathBuf, u64)>,
    denied: Vec<(PathBuf, DenyReason)>,
    subdirs: Vec<PathBuf>,
}

/// Result of walking one directory: the display node plus everything needed for its record.
struct Walked {
    node: ScanNode,
    top_files: Vec<(PathBuf, u64)>,
    denied: Vec<(PathBuf, DenyReason)>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn scan(root: &Path, opts: &ScanOptions, platform: &dyn Platform) -> ScanReport {
    let start = Instant::now();
    let journal = platform.journal();
    let token = journal.and_then(|j| j.token(root));

    let old = if opts.cache == CacheMode::Use {
        index::load(root)
    } else {
        None
    };
    let age = old
        .as_ref()
        .map(|i| now_unix().saturating_sub(i.created_unix));
    let old = old.filter(|_| age.unwrap_or(u64::MAX) <= opts.max_age_secs);

    let mut note = None;
    let strategy = match &old {
        None => Strategy::Fresh,
        Some(idx) => match (journal, idx.journal_token.as_deref()) {
            (Some(j), Some(t)) => match j.changes_since(root, t) {
                Some(ch) => Strategy::Journal(ch),
                None => {
                    note = Some("change journal unavailable for this interval; fell back to directory mtimes".into());
                    Strategy::Mtime
                }
            },
            _ => Strategy::Mtime,
        },
    };
    let strategy_name = match strategy {
        Strategy::Fresh => "fresh",
        Strategy::Journal(_) => "journal",
        Strategy::Mtime => "mtime",
    };
    if matches!(strategy, Strategy::Mtime) && note.is_none() {
        note = Some("files modified in place inside an unchanged directory are not re-measured; use --refresh for exact numbers".into());
    }

    let dev = std::fs::symlink_metadata(root)
        .map(|m| m.dev())
        .unwrap_or(0);
    let ctx = Ctx {
        opts,
        dev,
        old: old.as_ref(),
        strategy,
        seen_inodes: Mutex::new(HashSet::new()),
        updates: Mutex::new(HashMap::new()),
        reused: AtomicU64::new(0),
        rescanned: AtomicU64::new(0),
    };
    let w = walk(root, opts.depth, &ctx, false);
    let Ctx {
        updates,
        reused,
        rescanned,
        ..
    } = ctx;

    let mut index_path = None;
    if opts.cache != CacheMode::Off {
        let mut dirs = old.map(|i| i.dirs).unwrap_or_default();
        dirs.extend(updates.into_inner().unwrap());
        index::prune(&mut dirs, root);
        let idx = Index {
            version: index::INDEX_VERSION,
            root: root.to_path_buf(),
            created_unix: now_unix(),
            euid: unsafe { libc::geteuid() },
            journal_token: token,
            dirs,
        };
        index_path = index::save(&idx).ok();
    }

    let mut raw = w.denied;
    raw.sort();
    let unmeasured = collapse(raw)
        .into_iter()
        .map(|(path, reason)| {
            let (hint, suggested_command) = platform.deny_hint(&path, reason);
            Unmeasured {
                path,
                reason,
                hint,
                suggested_command,
            }
        })
        .collect();

    ScanReport {
        root: w.node,
        unmeasured,
        largest_files: w.top_files,
        elapsed_ms: start.elapsed().as_millis(),
        cache: CacheReport {
            strategy: strategy_name.into(),
            index_age_secs: age,
            reused_dirs: reused.into_inner(),
            rescanned_dirs: rescanned.into_inner(),
            index_path,
            note,
        },
    }
}

/// Drop entries nested under an already-listed unreadable path.
fn collapse(v: Vec<(PathBuf, DenyReason)>) -> Vec<(PathBuf, DenyReason)> {
    let mut out: Vec<(PathBuf, DenyReason)> = Vec::new();
    for (p, r) in v {
        if out.last().map(|(q, _)| p.starts_with(q)).unwrap_or(false) {
            continue;
        }
        out.push((p, r));
    }
    out
}

fn classify_err(e: &std::io::Error) -> DenyReason {
    match e.raw_os_error() {
        Some(libc::EACCES) => DenyReason::Permission,
        Some(libc::EPERM) => DenyReason::Privacy,
        _ => DenyReason::Other,
    }
}

fn leaf(path: &Path, bytes: u64, files: u64) -> ScanNode {
    ScanNode {
        path: path.to_path_buf(),
        bytes,
        files,
        children: vec![],
    }
}

fn denied(path: &Path, e: &std::io::Error, own: u64) -> Walked {
    Walked {
        node: leaf(path, own, 0),
        top_files: vec![],
        denied: vec![(path.to_path_buf(), classify_err(e))],
    }
}

/// Is `p` or anything below it in `set`? Descendants sort right after their ancestor.
fn touches(set: &std::collections::BTreeSet<PathBuf>, p: &Path) -> bool {
    set.range(p.to_path_buf()..)
        .next()
        .is_some_and(|d| d.starts_with(p))
}

fn under_any(set: &std::collections::BTreeSet<PathBuf>, p: &Path) -> bool {
    p.ancestors().any(|a| set.contains(a))
}

/// Rebuild a display node purely from the stored index.
fn node_from_index(
    idx: &Index,
    path: &Path,
    rec: &DirRec,
    depth_left: usize,
    top: usize,
) -> ScanNode {
    let mut children = Vec::new();
    if depth_left > 0 {
        for name in &rec.subdirs {
            let p = path.join(name);
            if let Some(r) = idx.dirs.get(&p) {
                children.push(node_from_index(idx, &p, r, depth_left - 1, top));
            }
        }
        for (f, b) in &rec.top_files {
            if f.parent() == Some(path) {
                children.push(leaf(f, *b, 1));
            }
        }
        children.sort_by_key(|x| std::cmp::Reverse(x.bytes));
        children.truncate(top);
    }
    ScanNode {
        path: path.to_path_buf(),
        bytes: rec.total_bytes,
        files: rec.total_files,
        children,
    }
}

fn walk(path: &Path, depth_left: usize, ctx: &Ctx, force_fresh: bool) -> Walked {
    let old_rec = if force_fresh {
        None
    } else {
        ctx.old.and_then(|i| i.dirs.get(path))
    };

    // Journal says nothing under here changed: reuse the whole subtree without touching disk.
    if let (Strategy::Journal(ch), Some(rec), Some(idx)) = (&ctx.strategy, old_rec, ctx.old) {
        if !touches(&ch.dirs, path)
            && !touches(&ch.recursive, path)
            && !under_any(&ch.recursive, path)
        {
            ctx.reused.fetch_add(1, Ordering::Relaxed);
            return Walked {
                node: node_from_index(idx, path, rec, depth_left, ctx.opts.top),
                top_files: rec.top_files.clone(),
                denied: rec.denied.clone(),
            };
        }
    }
    let force_fresh = force_fresh
        || matches!(&ctx.strategy, Strategy::Journal(ch) if under_any(&ch.recursive, path));

    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => return denied(path, &e, 0),
    };
    let own = meta.blocks() * 512;
    if !meta.is_dir() {
        if meta.nlink() > 1
            && !ctx
                .seen_inodes
                .lock()
                .unwrap()
                .insert((meta.dev(), meta.ino()))
        {
            return Walked {
                node: leaf(path, 0, 0),
                top_files: vec![],
                denied: vec![],
            };
        }
        let top = if own >= BIG_FILE_BYTES {
            vec![(path.to_path_buf(), own)]
        } else {
            vec![]
        };
        return Walked {
            node: leaf(path, own, 1),
            top_files: top,
            denied: vec![],
        };
    }
    if ctx.opts.one_filesystem && meta.dev() != ctx.dev {
        return Walked {
            node: leaf(path, 0, 0),
            top_files: vec![],
            denied: vec![],
        };
    }
    let mtime_ns = meta.mtime() * 1_000_000_000 + meta.mtime_nsec();

    // Can we trust the stored direct-file totals for this directory?
    let reuse_direct = !force_fresh
        && old_rec.is_some_and(|r| match &ctx.strategy {
            Strategy::Journal(ch) => !ch.dirs.contains(path) && r.mtime_ns == mtime_ns,
            Strategy::Mtime => r.mtime_ns == mtime_ns,
            Strategy::Fresh => false,
        });

    let Direct {
        bytes: direct_bytes,
        files: direct_files,
        mut nodes,
        mut top_files,
        denied: mut denied_list,
        subdirs,
    } = if reuse_direct {
        let r = old_rec.unwrap();
        ctx.reused.fetch_add(1, Ordering::Relaxed);
        let direct_top: Vec<(PathBuf, u64)> = r
            .top_files
            .iter()
            .filter(|(f, _)| f.parent() == Some(path))
            .cloned()
            .collect();
        Direct {
            bytes: r.direct_bytes,
            files: r.direct_files,
            nodes: direct_top.iter().map(|(f, b)| leaf(f, *b, 1)).collect(),
            top_files: direct_top,
            denied: vec![],
            subdirs: r.subdirs.iter().map(|s| path.join(s)).collect(),
        }
    } else {
        ctx.rescanned.fetch_add(1, Ordering::Relaxed);
        let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(path) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(e) => return denied(path, &e, own),
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for e in entries {
            match e.file_type() {
                Ok(t) if t.is_dir() => dirs.push(e.path()),
                _ => files.push(e.path()),
            }
        }
        let results: Vec<Walked> = files.par_iter().map(|p| walk(p, 0, ctx, true)).collect();
        let mut d = Direct {
            bytes: 0,
            files: 0,
            nodes: vec![],
            top_files: vec![],
            denied: vec![],
            subdirs: dirs,
        };
        for r in results {
            d.bytes += r.node.bytes;
            d.files += r.node.files;
            d.top_files.extend(r.top_files);
            d.denied.extend(r.denied);
            d.nodes.push(r.node);
        }
        d
    };

    let next = depth_left.saturating_sub(1);
    let kids: Vec<Walked> = subdirs
        .par_iter()
        .map(|p| walk(p, next, ctx, force_fresh))
        .collect();
    let mut total_bytes = own + direct_bytes;
    let mut total_files = direct_files;
    let mut dir_nodes = Vec::with_capacity(kids.len());
    for k in kids {
        total_bytes += k.node.bytes;
        total_files += k.node.files;
        index::merge_top(&mut top_files, &k.top_files);
        denied_list.extend(k.denied);
        dir_nodes.push(k.node);
    }
    top_files.sort_by_key(|x| std::cmp::Reverse(x.1));
    top_files.truncate(index::TOP_FILES);

    if ctx.opts.cache != CacheMode::Off
        && (total_bytes >= MIN_RECORD_BYTES || depth_left == ctx.opts.depth)
    {
        let names = subdirs
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        ctx.updates.lock().unwrap().insert(
            path.to_path_buf(),
            DirRec {
                mtime_ns,
                direct_bytes,
                direct_files,
                total_bytes,
                total_files,
                subdirs: names,
                top_files: top_files.clone(),
                denied: denied_list.clone(),
            },
        );
    }

    let mut children = Vec::new();
    if depth_left > 0 {
        children.append(&mut dir_nodes);
        children.append(&mut nodes);
        children.sort_by_key(|x| std::cmp::Reverse(x.bytes));
        children.truncate(ctx.opts.top);
        children.retain(|k| k.bytes > 0);
    }
    Walked {
        node: ScanNode {
            path: path.to_path_buf(),
            bytes: total_bytes,
            files: total_files,
            children,
        },
        top_files,
        denied: denied_list,
    }
}

/// Total size only, no children, no index.
pub fn size_of(path: &Path) -> (u64, Option<DenyReason>) {
    let opts = ScanOptions {
        depth: 0,
        top: 0,
        one_filesystem: true,
        cache: CacheMode::Off,
        max_age_secs: 0,
    };
    let dev = std::fs::symlink_metadata(path)
        .map(|m| m.dev())
        .unwrap_or(0);
    let ctx = Ctx {
        opts: &opts,
        dev,
        old: None,
        strategy: Strategy::Fresh,
        seen_inodes: Mutex::new(HashSet::new()),
        updates: Mutex::new(HashMap::new()),
        reused: AtomicU64::new(0),
        rescanned: AtomicU64::new(0),
    };
    let w = walk(path, 0, &ctx, true);
    (w.node.bytes, w.denied.first().map(|(_, r)| *r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::GenericPlatform;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ds-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mtime_cache_matches_fresh_and_sees_new_files() {
        let d = tmp("mtime");
        let cache = d.join("cache");
        std::env::set_var("DISK_SWEEP_CACHE_DIR", &cache);
        let data = d.join("data");
        std::fs::create_dir_all(data.join("a/b")).unwrap();
        std::fs::write(data.join("a/b/f1"), vec![1u8; 2 << 20]).unwrap();
        std::fs::write(data.join("a/f2"), vec![1u8; 1 << 20]).unwrap();

        let opts = ScanOptions::default();
        let first = scan(&data, &opts, &GenericPlatform);
        assert_eq!(first.cache.strategy, "fresh");
        let second = scan(&data, &opts, &GenericPlatform);
        assert_eq!(second.cache.strategy, "mtime");
        assert_eq!(first.root.bytes, second.root.bytes);
        assert!(second.cache.reused_dirs > 0);

        std::fs::write(data.join("a/b/f3"), vec![1u8; 3 << 20]).unwrap();
        let third = scan(&data, &opts, &GenericPlatform);
        let fresh = scan(
            &data,
            &ScanOptions {
                cache: CacheMode::Off,
                ..ScanOptions::default()
            },
            &GenericPlatform,
        );
        assert_eq!(third.root.bytes, fresh.root.bytes);
        std::fs::remove_dir_all(&d).unwrap();
    }
}

//! FSEvents history replay. macOS keeps a persistent per-volume event log, so a later
//! scan can ask "which directories changed since event N?" and rescan only those.

use dispatch2::DispatchQueue;
use objc2_core_foundation::{CFArray, CFString, CFUUID};
use objc2_core_services as cs;
use std::ffi::{c_char, c_void, CStr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use sweep_core::platform::{ChangeJournal, Changes};

const DATA_ROOT: &str = "/System/Volumes/Data";
/// Give up replaying history after this long and fall back to mtime validation.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(60);

pub struct FsEventsJournal;

fn volume_uuid(root: &Path) -> Option<String> {
    let dev = std::fs::metadata(root).ok()?.dev() as libc::dev_t;
    let uuid = unsafe { cs::FSEventsCopyUUIDForDevice(dev) }?;
    Some(CFUUID::new_string(None, Some(&uuid))?.to_string())
}

struct State {
    root: PathBuf,
    changes: Changes,
    done: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

/// FSEvents reports firmlinked paths (/Users/..) while scans may use /System/Volumes/Data/Users/..
/// and vice versa; map an event path into the scanned root's namespace.
fn normalize(root: &Path, p: &Path) -> Option<PathBuf> {
    if p.starts_with(root) {
        return Some(p.to_path_buf());
    }
    if root.starts_with(DATA_ROOT) {
        let alt = Path::new(DATA_ROOT).join(p.strip_prefix("/").ok()?);
        if alt.starts_with(root) {
            return Some(alt);
        }
    }
    if let Ok(rest) = p.strip_prefix(DATA_ROOT) {
        let alt = Path::new("/").join(rest);
        if alt.starts_with(root) {
            return Some(alt);
        }
    }
    None
}

fn record(st: &mut State, raw: &str, f: u32) {
    if f & cs::kFSEventStreamEventFlagHistoryDone != 0 {
        st.done = true;
        return;
    }
    if f & (cs::kFSEventStreamEventFlagEventIdsWrapped | cs::kFSEventStreamEventFlagRootChanged)
        != 0
    {
        let root = st.root.clone();
        st.changes.recursive.insert(root);
        return;
    }
    let p = PathBuf::from(raw.trim_end_matches('/'));
    let Some(p) = normalize(&st.root, &p) else {
        return;
    };
    let lost = cs::kFSEventStreamEventFlagMustScanSubDirs
        | cs::kFSEventStreamEventFlagUserDropped
        | cs::kFSEventStreamEventFlagKernelDropped;
    if f & lost != 0 {
        st.changes.recursive.insert(p);
        return;
    }
    if f & cs::kFSEventStreamEventFlagItemIsDir != 0 {
        st.changes.dirs.insert(p.clone());
    }
    if let Some(parent) = p.parent() {
        st.changes.dirs.insert(parent.to_path_buf());
    }
}

unsafe extern "C-unwind" fn callback(
    _stream: cs::ConstFSEventStreamRef,
    info: *mut c_void,
    n: usize,
    paths: NonNull<c_void>,
    flags: NonNull<cs::FSEventStreamEventFlags>,
    _ids: NonNull<cs::FSEventStreamEventId>,
) {
    let shared = unsafe { &*(info as *const Shared) };
    let paths = paths.as_ptr() as *const *const c_char;
    let mut st = shared.state.lock().unwrap();
    for i in 0..n {
        let f = unsafe { *flags.as_ptr().add(i) };
        let raw = unsafe { CStr::from_ptr(*paths.add(i)) }.to_string_lossy();
        record(&mut st, &raw, f);
    }
    if st.done {
        shared.cv.notify_all();
    }
}

impl ChangeJournal for FsEventsJournal {
    fn token(&self, root: &Path) -> Option<String> {
        let id = unsafe { cs::FSEventsGetCurrentEventId() };
        Some(format!("{id}:{}", volume_uuid(root)?))
    }

    fn changes_since(&self, root: &Path, token: &str) -> Option<Changes> {
        let (id, uuid) = token.split_once(':')?;
        let since: u64 = id.parse().ok()?;
        if volume_uuid(root)?.as_str() != uuid {
            return None; // event database was reset; history is meaningless
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                root: root.to_path_buf(),
                changes: Changes::default(),
                done: false,
            }),
            cv: Condvar::new(),
        });
        let mut ctx = cs::FSEventStreamContext {
            version: 0,
            info: Arc::as_ptr(&shared) as *mut c_void,
            retain: None,
            release: None,
            copyDescription: None,
        };
        let path = CFString::from_str(&root.to_string_lossy());
        let watch = CFArray::from_objects(&[&*path]);
        let queue = DispatchQueue::new("disk-sweep.fsevents", None);
        unsafe {
            let stream = cs::FSEventStreamCreate(
                None,
                Some(callback),
                &mut ctx,
                watch.as_opaque(),
                since,
                0.0,
                cs::kFSEventStreamCreateFlagFileEvents | cs::kFSEventStreamCreateFlagNoDefer,
            );
            if stream.is_null() {
                return None;
            }
            cs::FSEventStreamSetDispatchQueue(stream, Some(&queue));
            if !cs::FSEventStreamStart(stream) {
                cs::FSEventStreamInvalidate(stream);
                cs::FSEventStreamRelease(stream);
                return None;
            }
            {
                let st = shared.state.lock().unwrap();
                let _ = shared
                    .cv
                    .wait_timeout_while(st, REPLAY_TIMEOUT, |s| !s.done)
                    .unwrap();
            }
            // Stop delivers no further callbacks once it returns, so `shared` can be dropped after.
            cs::FSEventStreamStop(stream);
            cs::FSEventStreamInvalidate(stream);
            cs::FSEventStreamRelease(stream);
        }
        let st = std::mem::replace(
            &mut *shared.state.lock().unwrap(),
            State {
                root: PathBuf::new(),
                changes: Changes::default(),
                done: false,
            },
        );
        st.done.then_some(st.changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_firmlinks() {
        let data_root = Path::new("/System/Volumes/Data");
        assert_eq!(
            normalize(data_root, Path::new("/Users/a/x")),
            Some(PathBuf::from("/System/Volumes/Data/Users/a/x"))
        );
        let home = Path::new("/Users/a");
        assert_eq!(
            normalize(home, Path::new("/System/Volumes/Data/Users/a/x")),
            Some(PathBuf::from("/Users/a/x"))
        );
        assert_eq!(normalize(home, Path::new("/Users/b/x")), None);
    }

    #[test]
    fn journal_sees_a_new_file() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ds-fsev-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let j = FsEventsJournal;
        let token = j.token(&dir).expect("token");
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("sub/new"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let ch = j.changes_since(&dir, &token).expect("history");
        assert!(ch.dirs.contains(&dir.join("sub")), "{:?}", ch);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

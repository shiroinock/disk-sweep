use crate::fsevents::FsEventsJournal;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use sweep_core::model::{DenyReason, Snapshot, VolumeInfo};
use sweep_core::platform::{self, ChangeJournal, Platform, Running};

pub struct MacPlatform {
    journal: FsEventsJournal,
}

impl Default for MacPlatform {
    fn default() -> Self {
        Self {
            journal: FsEventsJournal,
        }
    }
}

const DATA_ROOT: &str = "/System/Volumes/Data";

fn diskutil_plist(args: &[&str]) -> Option<plist::Value> {
    // diskutil ships with macOS and is not packaged by Nix; look on PATH first.
    let bin = platform::find_program("diskutil", &["/usr/sbin/diskutil"])?;
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    plist::Value::from_reader_xml(&out.stdout[..]).ok()
}

fn dict_u64(d: &plist::Dictionary, k: &str) -> Option<u64> {
    d.get(k).and_then(|v| v.as_unsigned_integer())
}

/// "Available for important usage": what Finder shows as available, including purgeable space.
fn important_capacity(path: &Path) -> Option<u64> {
    use objc2_foundation::{
        NSArray, NSNumber, NSString, NSURLVolumeAvailableCapacityForImportantUsageKey, NSURL,
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let key = unsafe { NSURLVolumeAvailableCapacityForImportantUsageKey };
    let keys = NSArray::from_slice(&[key]);
    let dict = url.resourceValuesForKeys_error(&keys).ok()?;
    let v = dict.objectForKey(key)?;
    let n = v.downcast_ref::<NSNumber>()?;
    Some(n.longLongValue().max(0) as u64)
}

fn running_bundle_ids() -> BTreeSet<String> {
    use objc2_app_kit::NSWorkspace;
    let ws = NSWorkspace::sharedWorkspace();
    let apps = ws.runningApplications();
    apps.iter()
        .filter_map(|a| a.bundleIdentifier().map(|s| s.to_string()))
        .collect()
}

impl Platform for MacPlatform {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn data_roots(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(DATA_ROOT)]
    }

    fn volume(&self, path: &Path) -> Option<VolumeInfo> {
        let mut v = platform::statvfs_volume(path)?;
        // statvfs reports the whole APFS container; diskutil gives this volume's own usage.
        if let Some(plist::Value::Dictionary(d)) =
            diskutil_plist(&["info", "-plist", &path.to_string_lossy()])
        {
            if let Some(total) = dict_u64(&d, "APFSContainerSize") {
                v.total_bytes = total;
            }
            if let Some(free) = dict_u64(&d, "APFSContainerFree") {
                v.free_bytes = free;
            }
            if let Some(used) = dict_u64(&d, "CapacityInUse") {
                v.used_bytes = used;
            }
        }
        v.available_important_bytes = important_capacity(path);
        Some(v)
    }

    fn journal(&self) -> Option<&dyn ChangeJournal> {
        Some(&self.journal)
    }

    fn snapshots(&self) -> Vec<Snapshot> {
        let mut out = Vec::new();
        for vol in ["/", DATA_ROOT] {
            let Some(plist::Value::Dictionary(d)) =
                diskutil_plist(&["apfs", "listSnapshots", "-plist", vol])
            else {
                continue;
            };
            let Some(arr) = d.get("Snapshots").and_then(|s| s.as_array()) else {
                continue;
            };
            for s in arr.iter().filter_map(|s| s.as_dictionary()) {
                out.push(Snapshot {
                    volume: vol.into(),
                    name: s
                        .get("SnapshotName")
                        .and_then(|n| n.as_string())
                        .unwrap_or_default()
                        .into(),
                    purgeable: s.get("Purgeable").and_then(|p| p.as_boolean()),
                });
            }
        }
        out
    }

    fn running(&self) -> Running {
        Running {
            app_ids: running_bundle_ids(),
            processes: platform::ps_processes(),
        }
    }

    fn user_homes(&self) -> Vec<PathBuf> {
        platform::list_dirs(Path::new("/Users"))
            .into_iter()
            .filter(|p| {
                !matches!(
                    p.file_name().and_then(|n| n.to_str()),
                    Some("Shared" | "Guest")
                )
            })
            .collect()
    }

    fn deny_hint(&self, path: &Path, reason: DenyReason) -> (String, Option<String>) {
        match reason {
            DenyReason::Privacy => (
                "Protected by macOS privacy controls (TCC); sudo alone does not help. Grant Full Disk Access to the app that runs disk-sweep (System Settings > Privacy & Security > Full Disk Access), restart it, and rescan.".into(),
                Some("open 'x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles'".into()),
            ),
            DenyReason::Permission => (
                "Blocked by Unix permissions (often another user's home). Ask the human to run the command themselves; never ask for their password.".into(),
                Some(format!(
                    "sudo disk-sweep scan {} --depth 3 --out /tmp/disk-sweep-scan.json",
                    platform::shell_quote(path)
                )),
            ),
            DenyReason::Other => ("Read failed.".into(), None),
        }
    }
}

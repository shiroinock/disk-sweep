use crate::model::{ScanNode, Snapshot, Unmeasured, VolumeInfo};
use crate::platform::Platform;
use crate::scan::{scan, ScanOptions};
use serde::{Deserialize, Serialize};

/// Whole-volume picture: what was measured, what could not be, and the gap between
/// the volume's used space and the measured total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overview {
    pub platform: String,
    pub running_as_root: bool,
    pub volume: Option<VolumeInfo>,
    pub measured_bytes: u64,
    /// used_bytes - measured_bytes. Large values mean unreadable areas (other users,
    /// privacy-protected folders, Trash) or space outside the scanned roots.
    pub unaccounted_bytes: u64,
    pub roots: Vec<ScanNode>,
    pub unmeasured: Vec<Unmeasured>,
    pub snapshots: Vec<Snapshot>,
    pub hints: Vec<String>,
}

pub fn overview(platform: &dyn Platform, opts: &ScanOptions) -> Overview {
    let roots = platform.data_roots();
    let volume = roots.first().and_then(|r| platform.volume(r));
    let mut nodes = Vec::new();
    let mut unmeasured = Vec::new();
    for r in &roots {
        let rep = scan(r, opts, platform);
        nodes.push(rep.root);
        unmeasured.extend(rep.unmeasured);
    }
    let measured: u64 = nodes.iter().map(|n| n.bytes).sum();
    let used = volume.as_ref().map(|v| v.used_bytes).unwrap_or(0);
    let unaccounted = used.saturating_sub(measured);
    let snapshots = platform.snapshots();

    let mut hints = Vec::new();
    let gb = |b: u64| b as f64 / 1e9;
    if unaccounted > used / 10 {
        hints.push(format!(
            "{:.1} GB of used space was not measured. Check the `unmeasured` list; other users' homes and privacy-protected folders are the usual cause.",
            gb(unaccounted)
        ));
    }
    if !crate::platform::is_root()
        && unmeasured
            .iter()
            .any(|u| u.reason == crate::model::DenyReason::Permission)
    {
        hints.push("Some paths need admin rights. Ask the human to run the suggested_command themselves; never ask for their password.".into());
    }
    if unmeasured
        .iter()
        .any(|u| u.reason == crate::model::DenyReason::Privacy)
    {
        hints.push("Some paths are privacy-protected. Granting Full Disk Access to the terminal app lets them be measured.".into());
    }
    Overview {
        platform: platform.name().into(),
        running_as_root: crate::platform::is_root(),
        volume,
        measured_bytes: measured,
        unaccounted_bytes: unaccounted,
        roots: nodes,
        unmeasured,
        snapshots,
        hints,
    }
}

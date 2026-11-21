use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One node of a scan. `bytes` is allocated size on disk (st_blocks * 512), not apparent size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanNode {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<ScanNode>,
}

/// A location that could not be measured. Lets an agent reason about where the unaccounted space is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unmeasured {
    pub path: PathBuf,
    pub reason: DenyReason,
    pub hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_command: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// Unix permissions (EACCES). Readable with sudo.
    Permission,
    /// OS privacy protection (EPERM, e.g. macOS TCC). May stay unreadable even with sudo.
    Privacy,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    pub root: ScanNode,
    pub unmeasured: Vec<Unmeasured>,
    /// Largest individual files in the tree (at least 50 MiB), descending.
    pub largest_files: Vec<(PathBuf, u64)>,
    pub elapsed_ms: u128,
    pub cache: crate::scan::CacheReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub mount: PathBuf,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// macOS "available for important usage" (includes purgeable space). None where unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_important_bytes: Option<u64>,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub volume: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purgeable: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LoginRisk {
    /// Removing it never signs you out.
    #[default]
    None,
    /// Some apps may sign you out.
    Possible,
    /// Cookies, tokens, browser profiles. Removing it signs you out.
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Cache,
    PackageCache,
    BuildArtifact,
    VmImage,
    Media,
    Profile,
    AppData,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Remove the path itself.
    Delete,
    /// Keep the directory, remove its contents.
    DeleteContents,
    /// Never remove. Listed so agents know why it is left alone.
    Keep,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CandidateStatus {
    Ready,
    /// A related app is running: open files would not be freed and the app may misbehave.
    BlockedRunning {
        apps: Vec<String>,
    },
    /// Matched by a protect rule.
    Protected,
    /// Not readable with current privileges.
    Unreadable {
        hint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub rule_id: String,
    pub title: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub kind: Kind,
    pub login_risk: LoginRisk,
    pub regenerable: bool,
    pub action: Action,
    pub status: CandidateStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Home directory this candidate belongs to.
    pub home: PathBuf,
}

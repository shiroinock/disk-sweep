use crate::model::{Action, Candidate, CandidateStatus, LoginRisk};
use crate::rules::Evaluation;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A reviewed, frozen list of deletions. Agents create plans; humans (or an explicitly
/// authorised agent) apply them. `apply` re-evaluates every item against live rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub version: u32,
    pub created_unix: u64,
    pub total_bytes: u64,
    /// Homes the plan was evaluated against; `apply` re-evaluates the same ones.
    #[serde(default)]
    pub homes: Vec<PathBuf>,
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub rule_id: String,
    pub path: PathBuf,
    pub action: Action,
    pub bytes: u64,
    pub login_risk: LoginRisk,
    pub dev: u64,
    pub ino: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Selection {
    /// Rule IDs to include. Empty with no paths means "every ready candidate".
    #[serde(default)]
    pub rule_ids: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    /// Highest login risk allowed. Defaults to `none`.
    #[serde(default = "default_risk")]
    pub max_login_risk: LoginRisk,
    /// Skip candidates smaller than this.
    #[serde(default)]
    pub min_bytes: u64,
    /// Also include things that cannot be rebuilt (VM images, simulator devices).
    /// Only honoured together with explicit `rule_ids` or `paths`.
    #[serde(default)]
    pub include_non_regenerable: bool,
}

fn default_risk() -> LoginRisk {
    LoginRisk::None
}

fn selected(c: &Candidate, s: &Selection) -> bool {
    if c.status != CandidateStatus::Ready
        || c.action == Action::Keep
        || c.login_risk > s.max_login_risk
        || c.bytes < s.min_bytes
    {
        return false;
    }
    let explicit = !(s.rule_ids.is_empty() && s.paths.is_empty());
    if !(c.regenerable || s.include_non_regenerable && explicit) {
        return false;
    }
    if !explicit {
        return true;
    }
    s.rule_ids.iter().any(|r| r == &c.rule_id) || s.paths.iter().any(|p| p == &c.path)
}

pub fn make_plan(eval: &Evaluation, sel: &Selection, homes: &[PathBuf]) -> Plan {
    let items: Vec<PlanItem> = eval
        .candidates
        .iter()
        .filter(|c| selected(c, sel))
        .filter_map(|c| {
            let m = std::fs::symlink_metadata(&c.path).ok()?;
            Some(PlanItem {
                rule_id: c.rule_id.clone(),
                path: c.path.clone(),
                action: c.action,
                bytes: c.bytes,
                login_risk: c.login_risk,
                dev: m.dev(),
                ino: m.ino(),
            })
        })
        .collect();
    Plan {
        version: 1,
        created_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        total_bytes: items.iter().map(|i| i.bytes).sum(),
        homes: homes.to_vec(),
        items,
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ApplyReport {
    pub dry_run: bool,
    pub removed: Vec<Removed>,
    pub skipped: Vec<Skipped>,
    pub removed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Removed {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

/// Refuse obviously dangerous targets no matter what a rule says.
fn sanity_check(path: &Path, homes: &[PathBuf]) -> Result<()> {
    if !path.is_absolute() {
        bail!("not absolute");
    }
    if path.components().count() < 4 {
        bail!("too close to the filesystem root");
    }
    if homes.iter().any(|h| h == path) {
        bail!("is a home directory");
    }
    Ok(())
}

pub fn apply(plan: &Plan, live: &Evaluation, homes: &[PathBuf], dry_run: bool) -> ApplyReport {
    let mut rep = ApplyReport {
        dry_run,
        ..Default::default()
    };
    for item in &plan.items {
        let skip = |rep: &mut ApplyReport, reason: String| {
            rep.skipped.push(Skipped {
                path: item.path.clone(),
                reason,
            })
        };
        let live_c = if item.rule_id.starts_with("project:") {
            crate::projects::revalidate(&item.path, &item.rule_id)
        } else {
            live.candidates
                .iter()
                .find(|c| c.path == item.path && c.rule_id == item.rule_id)
                .cloned()
        };
        let Some(c) = live_c else {
            skip(&mut rep, "no longer matched by its rule".into());
            continue;
        };
        if c.status != CandidateStatus::Ready {
            skip(&mut rep, format!("status is now {:?}", c.status));
            continue;
        }
        match std::fs::symlink_metadata(&item.path) {
            Ok(m) if m.dev() == item.dev && m.ino() == item.ino => {}
            Ok(_) => {
                skip(&mut rep, "path was replaced since the plan was made".into());
                continue;
            }
            Err(e) => {
                skip(&mut rep, format!("stat failed: {e}"));
                continue;
            }
        }
        if let Err(e) = sanity_check(&item.path, homes) {
            skip(&mut rep, format!("refused: {e}"));
            continue;
        }
        if dry_run {
            rep.removed_bytes += c.bytes;
            rep.removed.push(Removed {
                path: item.path.clone(),
                bytes: c.bytes,
            });
            continue;
        }
        let res = match item.action {
            Action::Delete => remove(&item.path),
            Action::DeleteContents => remove_contents(&item.path, &live.protected),
            Action::Keep => Ok(()),
        };
        match res {
            Ok(()) => {
                rep.removed_bytes += c.bytes;
                rep.removed.push(Removed {
                    path: item.path.clone(),
                    bytes: c.bytes,
                });
            }
            Err(e) => skip(&mut rep, format!("remove failed: {e}")),
        }
    }
    rep
}

fn remove(p: &Path) -> std::io::Result<()> {
    let m = std::fs::symlink_metadata(p)?;
    if m.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

fn remove_contents(dir: &Path, protected: &[PathBuf]) -> std::io::Result<()> {
    let mut first_err = None;
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if protected.iter().any(|q| q.starts_with(&p)) {
            continue;
        }
        if let Err(err) = remove(&p) {
            first_err.get_or_insert(err);
        }
    }
    first_err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Candidate, Kind};

    fn cand(path: &Path, risk: LoginRisk, status: CandidateStatus) -> Candidate {
        Candidate {
            rule_id: "t".into(),
            title: "t".into(),
            path: path.into(),
            bytes: 10,
            kind: Kind::Cache,
            login_risk: risk,
            regenerable: true,
            action: Action::DeleteContents,
            status,
            note: None,
            home: "/".into(),
        }
    }

    #[test]
    fn plan_skips_login_risk_and_blocked() {
        let d = std::env::temp_dir().join(format!("ds-test-{}", std::process::id()));
        for n in ["a", "b", "c"] {
            std::fs::create_dir_all(d.join(n)).unwrap();
        }
        let eval = Evaluation {
            candidates: vec![
                cand(&d.join("a"), LoginRisk::None, CandidateStatus::Ready),
                cand(&d.join("b"), LoginRisk::High, CandidateStatus::Ready),
                cand(
                    &d.join("c"),
                    LoginRisk::None,
                    CandidateStatus::BlockedRunning {
                        apps: vec!["x".into()],
                    },
                ),
            ],
            protected: vec![],
        };
        let plan = make_plan(&eval, &Selection::default(), &[]);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].path, d.join("a"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn remove_contents_keeps_protected() {
        let d = std::env::temp_dir().join(format!("ds-test-prot-{}", std::process::id()));
        std::fs::create_dir_all(d.join("cache/junk")).unwrap();
        std::fs::create_dir_all(d.join("cache/profile/Default")).unwrap();
        std::fs::write(d.join("cache/junk/f"), b"x").unwrap();
        remove_contents(&d.join("cache"), &[d.join("cache/profile/Default")]).unwrap();
        assert!(!d.join("cache/junk").exists());
        assert!(d.join("cache/profile/Default").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn sanity_refuses_shallow_paths() {
        assert!(sanity_check(Path::new("/Users/x"), &[]).is_err());
        assert!(sanity_check(Path::new("/Users/x/Library/Caches"), &[]).is_ok());
    }
}

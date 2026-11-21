use crate::model::{Action, Candidate, CandidateStatus, Kind, LoginRisk};
use crate::platform::{Platform, Running};
use crate::scan::size_of;
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Built-in rule packs, embedded at compile time.
pub const BUILTIN: &[(&str, &str)] = &[
    ("protect", include_str!("../../../rules/protect.toml")),
    ("browsers", include_str!("../../../rules/browsers.toml")),
    ("dev-tools", include_str!("../../../rules/dev-tools.toml")),
    (
        "package-managers",
        include_str!("../../../rules/package-managers.toml"),
    ),
    (
        "virtualization",
        include_str!("../../../rules/virtualization.toml"),
    ),
    ("macos", include_str!("../../../rules/macos.toml")),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub title: String,
    /// Paths or globs. A leading `~` expands to each target home.
    pub paths: Vec<String>,
    pub kind: Kind,
    pub login_risk: LoginRisk,
    #[serde(default)]
    pub regenerable: bool,
    pub action: Action,
    /// Empty means every platform. Values: "macos", "linux".
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Bundle IDs (macOS) or app IDs. If one is running the candidate is blocked.
    #[serde(default)]
    pub blocked_by_apps: Vec<String>,
    /// Substrings of running executable paths that block the candidate.
    #[serde(default)]
    pub blocked_by_process: Vec<String>,
    /// Glob patterns (matched against the file name) to leave out.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Among matches that differ only by a trailing version number, keep the newest.
    #[serde(default)]
    pub keep_latest: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize)]
struct RuleFile {
    #[serde(rename = "rule", default)]
    rules: Vec<Rule>,
}

pub fn parse_rules(src: &str, origin: &str) -> Result<Vec<Rule>> {
    let f: RuleFile = toml::from_str(src).with_context(|| format!("parsing rule pack {origin}"))?;
    Ok(f.rules)
}

pub fn load_rules(extra_dirs: &[PathBuf]) -> Result<Vec<Rule>> {
    let mut out = Vec::new();
    for (name, src) in BUILTIN {
        out.extend(parse_rules(src, name)?);
    }
    for dir in extra_dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        files.sort();
        for f in files
            .into_iter()
            .filter(|f| f.extension().map(|e| e == "toml").unwrap_or(false))
        {
            let src = std::fs::read_to_string(&f)?;
            out.extend(parse_rules(&src, &f.to_string_lossy())?);
        }
    }
    Ok(out)
}

fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn expand(pattern: &str, home: &Path) -> String {
    if let Some(rest) = pattern.strip_prefix("~/") {
        format!("{}/{}", home.display(), rest)
    } else {
        pattern.to_string()
    }
}

/// Split "chromium-1194" into ("chromium", [1194]) so versions compare numerically.
fn version_key(name: &str) -> (String, Vec<u64>) {
    let idx = name
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '_')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(name.len());
    let (base, ver) = name.split_at(idx);
    let nums = ver
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (base.to_string(), nums)
}

fn drop_latest(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<(Vec<u64>, PathBuf)>> = BTreeMap::new();
    for p in paths.drain(..) {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (base, ver) = version_key(&name);
        groups.entry(base).or_default().push((ver, p));
    }
    let mut out = Vec::new();
    for (_, mut v) in groups {
        v.sort();
        v.pop();
        out.extend(v.into_iter().map(|(_, p)| p));
    }
    out
}

/// Expand every rule against every home and return matching existing paths.
pub fn matches(rule: &Rule, homes: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let excludes: Vec<glob::Pattern> = rule
        .exclude
        .iter()
        .filter_map(|e| glob::Pattern::new(e).ok())
        .collect();
    let mut out = Vec::new();
    for home in homes {
        for pat in &rule.paths {
            if !pat.starts_with("~/") && home != &homes[0] {
                continue; // absolute paths are expanded once
            }
            let expanded = expand(pat, home);
            let mut found: Vec<PathBuf> = glob::glob(&expanded)
                .map(|g| g.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
            found.retain(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                !excludes.iter().any(|e| e.matches(&name))
            });
            if rule.keep_latest {
                found = drop_latest(found);
            }
            out.extend(found.into_iter().map(|p| (p, home.clone())));
        }
    }
    out
}

pub struct Evaluation {
    pub candidates: Vec<Candidate>,
    /// Paths that must never be removed, even inside a delete-contents target.
    pub protected: Vec<PathBuf>,
}

pub fn evaluate(
    rules: &[Rule],
    homes: &[PathBuf],
    platform: &dyn Platform,
    running: &Running,
) -> Evaluation {
    let os = current_os();
    let active: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.platforms.is_empty() || r.platforms.iter().any(|p| p == os))
        .collect();

    let mut protected: Vec<PathBuf> = active
        .iter()
        .filter(|r| r.action == Action::Keep)
        .flat_map(|r| matches(r, homes).into_iter().map(|(p, _)| p))
        .collect();
    protected.sort();
    protected.dedup();

    let jobs: Vec<(&Rule, PathBuf, PathBuf)> = active
        .iter()
        .flat_map(|r| matches(r, homes).into_iter().map(move |(p, h)| (*r, p, h)))
        .collect();

    let mut candidates: Vec<Candidate> = jobs
        .par_iter()
        .map(|(rule, path, home)| {
            let (bytes, denied) = size_of(path);
            let shields_protected =
                rule.action == Action::Delete && protected.iter().any(|p| p.starts_with(path));
            let status = if rule.action == Action::Keep || shields_protected {
                CandidateStatus::Protected
            } else if let Some(reason) = denied.filter(|_| bytes == 0) {
                CandidateStatus::Unreadable {
                    hint: platform.deny_hint(path, reason).0,
                }
            } else {
                let apps = running.matches(&rule.blocked_by_apps, &rule.blocked_by_process);
                if apps.is_empty() {
                    CandidateStatus::Ready
                } else {
                    CandidateStatus::BlockedRunning { apps }
                }
            };
            Candidate {
                rule_id: rule.id.clone(),
                title: rule.title.clone(),
                path: path.clone(),
                bytes,
                kind: rule.kind,
                login_risk: rule.login_risk,
                regenerable: rule.regenerable,
                action: rule.action,
                status,
                note: rule.note.clone(),
                home: home.clone(),
            }
        })
        .collect();

    // A path matched by several rules: keep the first (most specific packs come first).
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert(c.path.clone()));
    candidates.sort_by_key(|x| std::cmp::Reverse(x.bytes));
    Evaluation {
        candidates,
        protected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_rules_parse() {
        let rules = load_rules(&[]).unwrap();
        assert!(rules.len() > 10);
        let mut ids: Vec<_> = rules.iter().map(|r| &r.id).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(n, ids.len(), "rule ids must be unique");
    }

    #[test]
    fn keep_latest_keeps_newest_per_group() {
        let v = vec![
            "/c/chromium-1179",
            "/c/chromium-1194",
            "/c/chromium-1181",
            "/c/webkit-2182",
            "/c/webkit-2215",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let mut dropped: Vec<String> = drop_latest(v)
            .into_iter()
            .map(|p| p.display().to_string())
            .collect();
        dropped.sort();
        assert_eq!(
            dropped,
            ["/c/chromium-1179", "/c/chromium-1181", "/c/webkit-2182"]
        );
    }

    #[test]
    fn version_key_splits_suffix() {
        assert_eq!(
            version_key("chromium_headless_shell-1187"),
            ("chromium_headless_shell".into(), vec![1187])
        );
        assert_eq!(version_key("v12.20.0"), ("v".into(), vec![12, 20, 0]));
    }
}

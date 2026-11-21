//! Build-artifact discovery in project trees, in the style of kondo / npkill:
//! a marker file identifies the project type, which names the directories that are
//! safe to delete because the build regenerates them.

use crate::model::{Action, Candidate, CandidateStatus, Kind, LoginRisk};
use crate::scan::size_of;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct ProjectType {
    pub id: &'static str,
    /// Any of these files in a directory marks it as a project root.
    pub markers: &'static [&'static str],
    pub artifacts: &'static [&'static str],
}

pub const PROJECT_TYPES: &[ProjectType] = &[
    ProjectType {
        id: "rust",
        markers: &["Cargo.toml"],
        artifacts: &["target"],
    },
    ProjectType {
        id: "node",
        markers: &["package.json"],
        artifacts: &[
            "node_modules",
            ".next",
            ".nuxt",
            ".turbo",
            ".parcel-cache",
            ".svelte-kit",
            ".angular",
        ],
    },
    ProjectType {
        id: "python",
        markers: &["pyproject.toml", "setup.py", "requirements.txt", "Pipfile"],
        artifacts: &[
            ".venv",
            "venv",
            "__pycache__",
            ".pytest_cache",
            ".mypy_cache",
            ".ruff_cache",
            ".tox",
        ],
    },
    ProjectType {
        id: "gradle",
        markers: &["build.gradle", "build.gradle.kts"],
        artifacts: &["build", ".gradle"],
    },
    ProjectType {
        id: "maven",
        markers: &["pom.xml"],
        artifacts: &["target"],
    },
    ProjectType {
        id: "swiftpm",
        markers: &["Package.swift"],
        artifacts: &[".build"],
    },
    ProjectType {
        id: "dart",
        markers: &["pubspec.yaml"],
        artifacts: &[".dart_tool", "build"],
    },
    ProjectType {
        id: "elixir",
        markers: &["mix.exs"],
        artifacts: &["_build", "deps"],
    },
    ProjectType {
        id: "cmake",
        markers: &["CMakeLists.txt"],
        artifacts: &["build", "cmake-build-debug", "cmake-build-release"],
    },
    ProjectType {
        id: "unity",
        markers: &["ProjectSettings/ProjectVersion.txt"],
        artifacts: &["Library", "Temp", "obj"],
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectArtifact {
    #[serde(flatten)]
    pub candidate: Candidate,
    pub project: PathBuf,
    pub project_type: String,
    /// Days since anything at the project's top level (or its git index) changed.
    pub idle_days: u64,
}

const SKIP_DIRS: &[&str] = &["Library", ".Trash", ".git", "Applications"];

fn newest_mtime(project: &Path, artifacts: &[&str]) -> i64 {
    let mut newest = 0;
    if let Ok(rd) = std::fs::read_dir(project) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name();
            if artifacts.iter().any(|a| name.to_string_lossy() == *a) {
                continue;
            }
            if let Ok(m) = e.metadata() {
                newest = newest.max(m.mtime());
            }
        }
    }
    for f in [".git/index", ".git/HEAD"] {
        if let Ok(m) = std::fs::metadata(project.join(f)) {
            newest = newest.max(m.mtime());
        }
    }
    newest
}

fn detect(dir: &Path) -> Vec<&'static ProjectType> {
    PROJECT_TYPES
        .iter()
        .filter(|t| t.markers.iter().any(|m| dir.join(m).exists()))
        .collect()
}

fn collect(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<(PathBuf, &'static ProjectType, PathBuf)>,
) {
    let types = detect(dir);
    let mut artifact_names: Vec<&str> = Vec::new();
    for t in &types {
        for a in t.artifacts {
            let p = dir.join(a);
            if std::fs::symlink_metadata(&p)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                out.push((dir.to_path_buf(), *t, p));
                artifact_names.push(a);
            }
        }
    }
    if depth >= max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if artifact_names.contains(&name.as_str())
            || SKIP_DIRS.contains(&name.as_str())
            || name == "node_modules"
        {
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        collect(&e.path(), depth + 1, max_depth, out);
    }
}

fn to_candidate(project: &Path, t: &ProjectType, artifact: &Path, home: &Path) -> ProjectArtifact {
    let (bytes, _) = size_of(artifact);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let idle_days = ((now - newest_mtime(project, t.artifacts)).max(0) / 86400) as u64;
    let name = artifact
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    ProjectArtifact {
        candidate: Candidate {
            rule_id: format!("project:{}:{}", t.id, name),
            title: format!("{} build output `{}`", t.id, name),
            path: artifact.to_path_buf(),
            bytes,
            kind: Kind::BuildArtifact,
            login_risk: LoginRisk::None,
            regenerable: true,
            action: Action::Delete,
            status: CandidateStatus::Ready,
            note: None,
            home: home.to_path_buf(),
        },
        project: project.to_path_buf(),
        project_type: t.id.into(),
        idle_days,
    }
}

/// Find build artifacts under `root`. `min_idle_days` filters out projects touched recently.
pub fn find(root: &Path, max_depth: usize, min_idle_days: u64) -> Vec<ProjectArtifact> {
    let mut raw = Vec::new();
    collect(root, 0, max_depth, &mut raw);
    let mut v: Vec<ProjectArtifact> = raw
        .par_iter()
        .map(|(proj, t, art)| to_candidate(proj, t, art, root))
        .filter(|a| a.idle_days >= min_idle_days)
        .collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.candidate.bytes));
    v
}

/// Re-derive a project artifact candidate for one path, used when applying a plan.
pub fn revalidate(path: &Path, rule_id: &str) -> Option<Candidate> {
    let project = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    detect(project)
        .into_iter()
        .find(|t| {
            t.artifacts.contains(&name.as_str()) && rule_id == format!("project:{}:{}", t.id, name)
        })
        .map(|t| to_candidate(project, t, path, project).candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_rust_and_node_artifacts() {
        let d = std::env::temp_dir().join(format!("ds-proj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/target/debug")).unwrap();
        std::fs::write(d.join("a/Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(d.join("b/node_modules/x/node_modules")).unwrap();
        std::fs::write(d.join("b/package.json"), "{}").unwrap();
        std::fs::create_dir_all(d.join("c/target")).unwrap(); // no marker: ignored
        let found = find(&d, 4, 0);
        let mut ids: Vec<_> = found.iter().map(|a| a.candidate.rule_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, ["project:node:node_modules", "project:rust:target"]);
        assert!(revalidate(&d.join("a/target"), "project:rust:target").is_some());
        assert!(revalidate(&d.join("c/target"), "project:rust:target").is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }
}

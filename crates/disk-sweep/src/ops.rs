//! Operations shared by the CLI and the MCP server. Every function returns JSON so the
//! two front ends stay identical.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use sweep_core::model::{CandidateStatus, LoginRisk};
use sweep_core::plan::{self, Plan, Selection};
use sweep_core::platform::{self, Platform};
use sweep_core::rules::{self, Evaluation};
use sweep_core::scan::{CacheMode, ScanOptions};
use sweep_core::{overview, projects};

pub fn platform() -> Box<dyn Platform> {
    #[cfg(all(target_os = "macos", feature = "macos"))]
    {
        Box::new(sweep_macos::MacPlatform::default())
    }
    #[cfg(not(all(target_os = "macos", feature = "macos")))]
    {
        Box::new(platform::GenericPlatform)
    }
}

pub fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ScanArgs {
    pub path: Option<PathBuf>,
    pub depth: Option<usize>,
    pub top: Option<usize>,
    /// "use" (default), "refresh" or "off".
    pub cache: Option<String>,
    pub max_age_hours: Option<u64>,
}

impl ScanArgs {
    pub fn options(&self) -> ScanOptions {
        let d = ScanOptions::default();
        ScanOptions {
            depth: self.depth.unwrap_or(d.depth),
            top: self.top.unwrap_or(d.top),
            one_filesystem: true,
            cache: match self.cache.as_deref() {
                Some("refresh") => CacheMode::Refresh,
                Some("off") => CacheMode::Off,
                _ => CacheMode::Use,
            },
            max_age_secs: self
                .max_age_hours
                .map(|h| h * 3600)
                .unwrap_or(d.max_age_secs),
        }
    }
}

pub fn overview(p: &dyn Platform, a: &ScanArgs) -> Result<Value> {
    let ov = overview::overview(p, &a.options());
    let mut v = serde_json::to_value(&ov)?;
    v["summary"] = json!({
        "used": ov.volume.as_ref().map(|x| human(x.used_bytes)),
        "free": ov.volume.as_ref().map(|x| human(x.free_bytes)),
        "available_important": ov.volume.as_ref().and_then(|x| x.available_important_bytes).map(human),
        "measured": human(ov.measured_bytes),
        "unaccounted": human(ov.unaccounted_bytes),
    });
    Ok(v)
}

pub fn scan(p: &dyn Platform, a: &ScanArgs) -> Result<Value> {
    let path = a.path.clone().context("`path` is required")?;
    let rep = sweep_core::scan::scan(&path, &a.options(), p);
    let mut v = serde_json::to_value(&rep)?;
    v["summary"] = json!({ "total": human(rep.root.bytes), "files": rep.root.files });
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TargetArgs {
    /// Evaluate every user's home (needs root to see inside other homes).
    pub all_users: bool,
    /// Explicit homes to evaluate instead of the current user's.
    pub homes: Vec<PathBuf>,
    /// Extra rule pack directories.
    pub rules_dirs: Vec<PathBuf>,
}

impl TargetArgs {
    pub fn homes(&self, p: &dyn Platform) -> Vec<PathBuf> {
        if !self.homes.is_empty() {
            self.homes.clone()
        } else if self.all_users {
            p.user_homes()
        } else {
            vec![platform::current_home()]
        }
    }

    pub fn rules_dirs(&self) -> Vec<PathBuf> {
        let mut v = self.rules_dirs.clone();
        let cfg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| platform::current_home().join(".config"));
        v.push(cfg.join("disk-sweep/rules"));
        v
    }
}

pub fn evaluate(p: &dyn Platform, t: &TargetArgs) -> Result<(Evaluation, Vec<PathBuf>)> {
    let rules = rules::load_rules(&t.rules_dirs())?;
    let homes = t.homes(p);
    let running = p.running();
    Ok((rules::evaluate(&rules, &homes, p, &running), homes))
}

pub fn candidates(p: &dyn Platform, t: &TargetArgs, min_bytes: u64) -> Result<Value> {
    let (eval, homes) = evaluate(p, t)?;
    let list: Vec<_> = eval
        .candidates
        .iter()
        .filter(|c| c.bytes >= min_bytes || c.status == CandidateStatus::Protected)
        .collect();
    let sum = |f: &dyn Fn(&&sweep_core::Candidate) -> bool| {
        list.iter().filter(|c| f(c)).map(|c| c.bytes).sum::<u64>()
    };
    let ready = sum(&|c| {
        c.status == CandidateStatus::Ready && c.login_risk == LoginRisk::None && c.regenerable
    });
    let blocked = sum(&|c| matches!(c.status, CandidateStatus::BlockedRunning { .. }));
    let non_regen = sum(&|c| c.status == CandidateStatus::Ready && !c.regenerable);
    Ok(json!({
        "homes": homes,
        "summary": {
            "safe_ready": human(ready),
            "blocked_by_running_apps": human(blocked),
            "needs_decision_not_regenerable": human(non_regen),
        },
        "candidates": list.iter().map(|c| {
            let mut v = serde_json::to_value(c).unwrap();
            v["size"] = json!(human(c.bytes));
            v
        }).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectArgs {
    pub root: Option<PathBuf>,
    pub max_depth: Option<usize>,
    pub min_idle_days: Option<u64>,
    pub min_mb: Option<u64>,
}

pub fn projects(a: &ProjectArgs) -> Result<Value> {
    let root = a.root.clone().unwrap_or_else(platform::current_home);
    let min = a.min_mb.unwrap_or(10) << 20;
    let found: Vec<_> = projects::find(
        &root,
        a.max_depth.unwrap_or(6),
        a.min_idle_days.unwrap_or(0),
    )
    .into_iter()
    .filter(|p| p.candidate.bytes >= min)
    .collect();
    let total: u64 = found.iter().map(|p| p.candidate.bytes).sum();
    Ok(json!({
        "root": root,
        "summary": { "total": human(total), "count": found.len() },
        "artifacts": found.iter().map(|a| {
            let mut v = serde_json::to_value(a).unwrap();
            v["size"] = json!(human(a.candidate.bytes));
            v
        }).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlanArgs {
    #[serde(flatten)]
    pub target: TargetArgs,
    #[serde(flatten)]
    pub selection: Selection,
    /// Also consider build artifacts under these project roots.
    pub project_roots: Vec<PathBuf>,
    pub project_min_idle_days: Option<u64>,
    pub out: Option<PathBuf>,
}

pub fn default_plan_path() -> PathBuf {
    std::env::temp_dir().join(format!("disk-sweep-{}.plan.json", std::process::id()))
}

pub fn make_plan(p: &dyn Platform, a: &PlanArgs) -> Result<Value> {
    let (mut eval, homes) = evaluate(p, &a.target)?;
    for root in &a.project_roots {
        eval.candidates.extend(
            projects::find(root, 6, a.project_min_idle_days.unwrap_or(30))
                .into_iter()
                .map(|x| x.candidate),
        );
    }
    let plan = plan::make_plan(&eval, &a.selection, &homes);
    let out = a.out.clone().unwrap_or_else(default_plan_path);
    std::fs::write(&out, serde_json::to_vec_pretty(&plan)?)?;
    let needs_sudo = plan.items.iter().any(|i| !writable_parent(&i.path));
    let mut cmd = format!(
        "disk-sweep apply --plan {} --yes",
        platform::shell_quote(&out)
    );
    if needs_sudo {
        cmd = format!("sudo {cmd}");
    }
    Ok(json!({
        "plan_file": out,
        "items": plan.items.len(),
        "total": human(plan.total_bytes),
        "total_bytes": plan.total_bytes,
        "paths": plan.items.iter().map(|i| json!({"path": i.path, "size": human(i.bytes), "rule": i.rule_id})).collect::<Vec<_>>(),
        "apply_command": cmd,
        "next_step": "Show this list to the human. Applying deletes permanently; only run apply after they approve, or let them run apply_command themselves.",
    }))
}

fn writable_parent(p: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let target = p.parent().unwrap_or(p);
    CString::new(target.as_os_str().as_bytes())
        .map(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
        .unwrap_or(false)
}

pub fn apply(p: &dyn Platform, plan_file: &Path, dry_run: bool, t: &TargetArgs) -> Result<Value> {
    let plan: Plan = serde_json::from_slice(
        &std::fs::read(plan_file).with_context(|| format!("reading {}", plan_file.display()))?,
    )?;
    // Re-evaluate against every home the plan touches, so live rules decide, not the file.
    let mut target = t.clone();
    if target.homes.is_empty() && !target.all_users {
        target.homes = plan.homes.clone();
        if target.homes.is_empty() {
            target.homes = p
                .user_homes()
                .into_iter()
                .filter(|h| plan.items.iter().any(|i| i.path.starts_with(h)))
                .collect();
        }
        if target.homes.is_empty() {
            target.homes.push(platform::current_home());
        }
    }
    let (eval, homes) = evaluate(p, &target)?;
    let root = p
        .data_roots()
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("/"));
    let before = p.volume(&root);
    let rep = plan::apply(&plan, &eval, &homes, dry_run);
    let after = if dry_run { None } else { p.volume(&root) };
    let mut v = serde_json::to_value(&rep)?;
    v["summary"] = json!({
        "removed": human(rep.removed_bytes),
        "free_before": before.as_ref().map(|x| human(x.free_bytes)),
        "free_after": after.as_ref().map(|x| human(x.free_bytes)),
    });
    Ok(v)
}

pub fn list_rules(t: &TargetArgs) -> Result<Value> {
    Ok(serde_json::to_value(rules::load_rules(&t.rules_dirs())?)?)
}

/// Compact text for humans; JSON stays the primary format.
pub fn render_human(v: &Value) -> String {
    let mut s = String::new();
    if let Some(sum) = v.get("summary") {
        s.push_str(&format!(
            "{}\n",
            serde_json::to_string_pretty(sum).unwrap_or_default()
        ));
    }
    for key in ["candidates", "artifacts"] {
        if let Some(arr) = v.get(key).and_then(|a| a.as_array()) {
            for outer in arr {
                let c = outer.get("candidate").unwrap_or(outer);
                let state = c
                    .pointer("/status/state")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                s.push_str(&format!(
                    "{:>9}  {:<16} {:<8} {}  [{}]\n",
                    outer.get("size").and_then(|x| x.as_str()).unwrap_or(""),
                    state,
                    c.get("login_risk").and_then(|x| x.as_str()).unwrap_or(""),
                    c.get("path").and_then(|x| x.as_str()).unwrap_or(""),
                    c.get("rule_id").and_then(|x| x.as_str()).unwrap_or(""),
                ));
            }
        }
    }
    if let Some(arr) = v.get("paths").and_then(|a| a.as_array()) {
        for c in arr {
            s.push_str(&format!(
                "{:>9}  {}\n",
                c["size"].as_str().unwrap_or(""),
                c["path"].as_str().unwrap_or("")
            ));
        }
        if let Some(cmd) = v.get("apply_command").and_then(|x| x.as_str()) {
            s.push_str(&format!("\napply with:\n  {cmd}\n"));
        }
    }
    if let Some(arr) = v.get("unmeasured").and_then(|a| a.as_array()) {
        if !arr.is_empty() {
            s.push_str(&format!(
                "\n{} unreadable location(s); first ones:\n",
                arr.len()
            ));
            for u in arr.iter().take(8) {
                s.push_str(&format!(
                    "  {}  ({})\n",
                    u["path"].as_str().unwrap_or(""),
                    u["reason"].as_str().unwrap_or("")
                ));
            }
        }
    }
    if let Some(arr) = v
        .get("steps")
        .or_else(|| v.get("integrations"))
        .and_then(|a| a.as_array())
    {
        for st in arr {
            s.push_str(&format!(
                "{:<15} {:<6} {:<19} -> {:<8} {}{}\n",
                st["agent"].as_str().unwrap_or(""),
                st["kind"].as_str().unwrap_or(""),
                st["state"].as_str().unwrap_or(""),
                st["action"].as_str().unwrap_or(""),
                st["target"].as_str().unwrap_or(""),
                st.get("result")
                    .and_then(|r| r.as_str())
                    .map(|r| format!("  [{r}]"))
                    .unwrap_or_default(),
            ));
            let shows_command =
                ["manual", "install", "remove"].contains(&st["action"].as_str().unwrap_or(""));
            if let Some(c) = st
                .get("command")
                .and_then(|c| c.as_str())
                .filter(|_| shows_command)
            {
                s.push_str(&format!("{:<15} command: {c}\n", ""));
            }
        }
    }
    for key in ["next_steps"] {
        if let Some(arr) = v.get(key).and_then(|a| a.as_array()) {
            for n in arr {
                s.push_str(&format!("next: {}\n", n.as_str().unwrap_or("")));
            }
        }
    }
    if let Some(n) = v.get("next_step").and_then(|x| x.as_str()) {
        s.push_str(&format!("next: {n}\n"));
    }
    if let Some(arr) = v.get("hints").and_then(|a| a.as_array()) {
        for h in arr {
            s.push_str(&format!("hint: {}\n", h.as_str().unwrap_or("")));
        }
    }
    s
}

//! `setup` and `doctor`: wire disk-sweep into AI agents without hand-editing configs.
//!
//! Everything is a dry run unless `--yes` is given, every step is reported as JSON, and
//! config files are backed up before they are changed.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use sweep_core::platform;

pub const SKILL_MD: &str = include_str!("../../../skills/disk-sweep/SKILL.md");
const SERVER: &str = "disk-sweep";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
    ClaudeDesktop,
    Cursor,
}

impl Agent {
    pub const ALL: [Agent; 4] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::ClaudeDesktop,
        Agent::Cursor,
    ];

    fn detected(self) -> bool {
        let home = platform::current_home();
        match self {
            Agent::ClaudeCode => home.join(".claude").is_dir() || which("claude").is_some(),
            Agent::Codex => home.join(".codex").is_dir() || which("codex").is_some(),
            Agent::ClaudeDesktop => {
                desktop_config().is_some_and(|p| p.parent().is_some_and(|d| d.is_dir()))
            }
            Agent::Cursor => home.join(".cursor").is_dir(),
        }
    }

    fn skill_dir(self) -> Option<PathBuf> {
        let home = platform::current_home();
        match self {
            Agent::ClaudeCode => Some(home.join(".claude/skills").join(SERVER)),
            Agent::Codex => Some(home.join(".codex/skills").join(SERVER)),
            // Claude Desktop skills are uploaded in the app; Cursor has no skill folder.
            Agent::ClaudeDesktop | Agent::Cursor => None,
        }
    }
}

fn desktop_config() -> Option<PathBuf> {
    let home = platform::current_home();
    if cfg!(target_os = "macos") {
        Some(home.join("Library/Application Support/Claude/claude_desktop_config.json"))
    } else {
        std::env::var_os("APPDATA")
            .map(|a| PathBuf::from(a).join("Claude/claude_desktop_config.json"))
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    platform::find_program(bin, &[])
}

/// Absolute path of this binary, so GUI apps without the shell PATH can launch it.
///
/// A Nix build lives in `/nix/store/<hash>-...`, which changes on every update and can be
/// garbage-collected, so prefer the stable profile path (`~/.nix-profile/bin`,
/// `/run/current-system/sw/bin`, ...) that PATH resolves to.
pub fn exe() -> PathBuf {
    let raw = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(SERVER));
    let real = raw.canonicalize().unwrap_or_else(|_| raw.clone());
    if real.starts_with("/nix/store") {
        if let Some(p) = which(SERVER) {
            return p;
        }
    }
    real
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub agent: Agent,
    /// "skill" or "mcp".
    pub kind: &'static str,
    pub target: String,
    /// "missing", "outdated", "current", "provided-by-plugin", "managed-elsewhere".
    pub state: String,
    /// What `--yes` does: "install", "update", "remove", "none", or "manual" (run `command` yourself).
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn run_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run(cmd: &str, args: &[String]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok("ok".into())
    } else {
        Err(String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(400)
            .collect())
    }
}

fn shell_line(cmd: &str, args: &[String]) -> String {
    std::iter::once(cmd.to_string())
        .chain(args.iter().map(|a| platform::shell_quote(Path::new(a))))
        .collect::<Vec<_>>()
        .join(" ")
}

fn claude_plugin_installed() -> bool {
    Command::new("claude")
        .args(["plugin", "list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("disk-sweep@"))
        .unwrap_or(false)
}

fn skill_step(agent: Agent, dir: &Path, remove: bool) -> Step {
    let file = dir.join("SKILL.md");
    let is_link = std::fs::symlink_metadata(dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let current = std::fs::read_to_string(&file).ok();
    let (state, action, note) = if is_link {
        (
            "managed-elsewhere",
            "none",
            Some("symlink (likely the `skills` CLI); update it there".into()),
        )
    } else if remove {
        match current {
            Some(_) => ("current", "remove", None),
            None => ("missing", "none", None),
        }
    } else {
        match current.as_deref() {
            None => ("missing", "install", None),
            Some(s) if s == SKILL_MD => ("current", "none", None),
            Some(_) => ("outdated", "update", None),
        }
    };
    Step {
        agent,
        kind: "skill",
        target: file.display().to_string(),
        state: state.into(),
        action,
        command: None,
        result: None,
        note,
    }
}

fn json_config_step(agent: Agent, path: &Path, remove: bool) -> Step {
    let cfg: Value = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}));
    let want = json!({"command": exe(), "args": ["mcp"]});
    let have = cfg.pointer(&format!("/mcpServers/{SERVER}"));
    let (state, action) = match (have, remove) {
        (Some(_), true) => ("current", "remove"),
        (None, true) => ("missing", "none"),
        (None, false) => ("missing", "install"),
        (Some(h), false) if h == &want => ("current", "none"),
        (Some(_), false) => ("outdated", "update"),
    };
    let note = (agent == Agent::ClaudeDesktop)
        .then(|| "restart Claude Desktop to load the change".to_string());
    Step {
        agent,
        kind: "mcp",
        target: path.display().to_string(),
        state: state.into(),
        action,
        command: None,
        result: None,
        note,
    }
}

fn cli_mcp_step(agent: Agent, remove: bool) -> Step {
    let (bin, get, add, rm): (&str, Vec<String>, Vec<String>, Vec<String>) = match agent {
        Agent::ClaudeCode => (
            "claude",
            vec!["mcp".into(), "get".into(), SERVER.into()],
            vec![
                "mcp".into(),
                "add".into(),
                "--transport".into(),
                "stdio".into(),
                "--scope".into(),
                "user".into(),
                SERVER.into(),
                "--".into(),
                exe().display().to_string(),
                "mcp".into(),
            ],
            vec![
                "mcp".into(),
                "remove".into(),
                "--scope".into(),
                "user".into(),
                SERVER.into(),
            ],
        ),
        _ => (
            "codex",
            vec!["mcp".into(), "get".into(), SERVER.into()],
            vec![
                "mcp".into(),
                "add".into(),
                SERVER.into(),
                "--".into(),
                exe().display().to_string(),
                "mcp".into(),
            ],
            vec!["mcp".into(), "remove".into(), SERVER.into()],
        ),
    };
    let mut step = Step {
        agent,
        kind: "mcp",
        target: format!("{bin} mcp ({SERVER})"),
        state: String::new(),
        action: "none",
        command: None,
        result: None,
        note: None,
    };
    if which(bin).is_none() {
        step.state = "missing".into();
        step.action = "manual";
        step.note = Some(format!(
            "`{bin}` is not on PATH; run the command once it is"
        ));
        step.command = Some(shell_line(bin, &add));
        return step;
    }
    if agent == Agent::ClaudeCode && claude_plugin_installed() {
        step.state = "provided-by-plugin".into();
        step.note =
            Some("the disk-sweep Claude Code plugin already registers this MCP server".into());
        return step;
    }
    let present = run_ok(bin, &get.iter().map(String::as_str).collect::<Vec<_>>());
    match (present, remove) {
        (true, true) => {
            step.state = "current".into();
            step.action = "remove";
            step.command = Some(shell_line(bin, &rm));
        }
        (false, true) => step.state = "missing".into(),
        (true, false) => step.state = "current".into(),
        (false, false) => {
            step.state = "missing".into();
            step.action = "install";
            step.command = Some(shell_line(bin, &add));
        }
    }
    step
}

pub struct SetupOpts {
    pub agents: Vec<Agent>,
    pub skill: bool,
    pub mcp: bool,
    pub remove: bool,
    pub apply: bool,
}

pub fn plan_steps(o: &SetupOpts) -> Vec<Step> {
    let agents: Vec<Agent> = if o.agents.is_empty() {
        Agent::ALL.into_iter().filter(|a| a.detected()).collect()
    } else {
        o.agents.clone()
    };
    let mut steps = Vec::new();
    for a in agents {
        if o.skill {
            if let Some(dir) = a.skill_dir() {
                steps.push(skill_step(a, &dir, o.remove));
            }
        }
        if o.mcp {
            match a {
                Agent::ClaudeCode | Agent::Codex => steps.push(cli_mcp_step(a, o.remove)),
                Agent::ClaudeDesktop => {
                    if let Some(p) = desktop_config() {
                        steps.push(json_config_step(a, &p, o.remove));
                    }
                }
                Agent::Cursor => steps.push(json_config_step(
                    a,
                    &platform::current_home().join(".cursor/mcp.json"),
                    o.remove,
                )),
            }
        }
    }
    steps
}

fn backup(path: &Path) -> Result<()> {
    if path.exists() {
        let b = PathBuf::from(format!("{}.disk-sweep-backup", path.display()));
        std::fs::copy(path, &b).with_context(|| format!("backing up {}", path.display()))?;
    }
    Ok(())
}

fn apply_step(s: &Step) -> Result<String> {
    match (s.kind, s.action) {
        (_, "none") => Ok("skipped".into()),
        (_, "manual") => Ok("needs manual step: run `command`".into()),
        ("skill", "install" | "update") => {
            let f = PathBuf::from(&s.target);
            std::fs::create_dir_all(f.parent().unwrap())?;
            std::fs::write(&f, SKILL_MD)?;
            Ok("written".into())
        }
        ("skill", "remove") => {
            let f = PathBuf::from(&s.target);
            std::fs::remove_file(&f)?;
            let _ = std::fs::remove_dir(f.parent().unwrap());
            Ok("removed".into())
        }
        ("mcp", _) if s.agent == Agent::ClaudeDesktop || s.agent == Agent::Cursor => {
            let path = PathBuf::from(&s.target);
            let mut cfg: Value = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(json!({}));
            backup(&path)?;
            let servers = cfg
                .as_object_mut()
                .context("config is not a JSON object")?
                .entry("mcpServers")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .context("mcpServers is not an object")?;
            if s.action == "remove" {
                servers.remove(SERVER);
            } else {
                servers.insert(SERVER.into(), json!({"command": exe(), "args": ["mcp"]}));
            }
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&path, serde_json::to_string_pretty(&cfg)? + "\n")?;
            Ok("updated (backup: .disk-sweep-backup)".into())
        }
        ("mcp", _) => {
            let cmd = s.command.as_deref().context("no command")?;
            let mut parts = shell_words(cmd);
            let bin = parts.remove(0);
            run(&bin, &parts).map_err(|e| anyhow::anyhow!(e))
        }
        _ => Ok("skipped".into()),
    }
}

/// Split a command built by `shell_line` (only single-quoted words) back into argv.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if in_q && chars.peek() == Some(&'\\') => {
                // '\'' sequence: closing quote, escaped quote, reopening quote
                chars.next();
                chars.next();
                chars.next();
                cur.push('\'');
            }
            '\'' => in_q = !in_q,
            ' ' if !in_q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn setup(o: &SetupOpts) -> Result<Value> {
    let mut steps = plan_steps(o);
    if o.apply {
        for s in &mut steps {
            s.result = Some(match apply_step(s) {
                Ok(r) => r,
                Err(e) => format!("error: {e:#}"),
            });
        }
    }
    let pending = steps.iter().filter(|s| s.action != "none").count();
    let manual = steps.iter().filter(|s| s.action == "manual").count();
    Ok(json!({
        "dry_run": !o.apply,
        "exe": exe(),
        "steps": steps,
        "pending": if o.apply { manual } else { pending },
        "next_step": if o.apply && manual > 0 {
            "Done, except steps marked manual: run their `command` once that tool is installed."
        } else if o.apply {
            "Done. Restart agents that were running so they pick up the skill and MCP server."
        } else if pending > 0 {
            "Re-run with --yes to apply these steps."
        } else {
            "Everything is already set up."
        },
    }))
}

/// Can this process read TCC-protected locations?
fn full_disk_access() -> Option<bool> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let probe = platform::current_home().join("Library/Application Support/com.apple.TCC/TCC.db");
    match std::fs::File::open(&probe) {
        Ok(_) => Some(true),
        Err(e)
            if e.raw_os_error() == Some(libc::EPERM) || e.raw_os_error() == Some(libc::EACCES) =>
        {
            Some(false)
        }
        Err(_) => None,
    }
}

pub fn doctor() -> Result<Value> {
    let exe = exe();
    let on_path = which(SERVER).and_then(|p| p.canonicalize().ok());
    let cache = sweep_core::index::cache_dir();
    let cache_bytes: u64 = std::fs::read_dir(&cache)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    let fda = full_disk_access();
    let steps = plan_steps(&SetupOpts {
        agents: vec![],
        skill: true,
        mcp: true,
        remove: false,
        apply: false,
    });
    let mut next = Vec::new();
    if on_path.as_deref() != Some(exe.as_path()) {
        next.push(format!(
            "`disk-sweep` on PATH is {:?}, not this binary; add {} to PATH",
            on_path,
            exe.parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ));
    }
    if steps.iter().any(|s| s.action != "none") {
        next.push(
            "run `disk-sweep setup` to see the integration steps, then `disk-sweep setup --yes`"
                .into(),
        );
    }
    if fda == Some(false) {
        next.push("optional: grant Full Disk Access to the terminal or agent app so privacy-protected folders can be measured".into());
    }
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "exe": exe,
        "on_path": on_path,
        "platform": crate::ops::platform().name(),
        "running_as_root": platform::is_root(),
        "full_disk_access": fda,
        "index": {"dir": cache, "bytes": cache_bytes},
        "integrations": steps,
        "next_steps": next,
        "ok": next.is_empty(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_round_trip() {
        let args: Vec<String> = vec![
            "mcp".into(),
            "add".into(),
            "/a b/it's/disk-sweep".into(),
            "mcp".into(),
        ];
        let line = shell_line("claude", &args);
        let mut back = shell_words(&line);
        assert_eq!(back.remove(0), "claude");
        assert_eq!(back, args);
    }

    #[test]
    fn plugin_version_matches_crate() {
        let plugin: Value =
            serde_json::from_str(include_str!("../../../.claude-plugin/plugin.json")).unwrap();
        assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn embedded_skill_has_frontmatter() {
        assert!(SKILL_MD.starts_with("---\nname: disk-sweep\n"));
    }
}

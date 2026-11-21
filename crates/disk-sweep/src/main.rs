mod mcp;
mod ops;
mod setup;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use std::path::PathBuf;
use sweep_core::model::LoginRisk;
use sweep_core::plan::Selection;

/// Disk cleanup built for AI agents and the humans who approve them.
///
/// Output is JSON by default. Nothing is deleted except by `apply --yes`.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Print a short human summary instead of JSON.
    #[arg(long, global = true)]
    human: bool,
    /// Pretty-print JSON.
    #[arg(long, global = true)]
    pretty: bool,
    /// Also write the JSON result to this file (handy with sudo).
    #[arg(long, global = true)]
    out: Option<PathBuf>,
}

#[derive(Args, Clone)]
struct CacheFlags {
    /// Levels of children to print.
    #[arg(long, default_value_t = 2)]
    depth: usize,
    /// Children per level.
    #[arg(long, default_value_t = 15)]
    top: usize,
    /// Ignore the scan index and rebuild it.
    #[arg(long, conflicts_with = "no_cache")]
    refresh: bool,
    /// Do not read or write the scan index.
    #[arg(long)]
    no_cache: bool,
    /// Ignore an index older than this many hours.
    #[arg(long)]
    max_age_hours: Option<u64>,
}

impl CacheFlags {
    fn to_args(&self, path: Option<PathBuf>) -> ops::ScanArgs {
        ops::ScanArgs {
            path,
            depth: Some(self.depth),
            top: Some(self.top),
            cache: Some(
                if self.refresh {
                    "refresh"
                } else if self.no_cache {
                    "off"
                } else {
                    "use"
                }
                .into(),
            ),
            max_age_hours: self.max_age_hours,
        }
    }
}

#[derive(Args, Clone)]
struct TargetFlags {
    /// Evaluate every user's home (run with sudo to see inside other homes).
    #[arg(long)]
    all_users: bool,
    /// Evaluate this home instead of the current user's (repeatable).
    #[arg(long = "home")]
    homes: Vec<PathBuf>,
    /// Extra rule pack directory (repeatable).
    #[arg(long = "rules")]
    rules_dirs: Vec<PathBuf>,
}

impl TargetFlags {
    fn to_args(&self) -> ops::TargetArgs {
        ops::TargetArgs {
            all_users: self.all_users,
            homes: self.homes.clone(),
            rules_dirs: self.rules_dirs.clone(),
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Volume usage, size tree, unreadable areas and the unaccounted gap.
    Overview {
        #[command(flatten)]
        cache: CacheFlags,
    },
    /// Size tree and largest files of one directory.
    Scan {
        path: PathBuf,
        #[command(flatten)]
        cache: CacheFlags,
    },
    /// Removable locations known to the rule packs.
    Candidates {
        #[command(flatten)]
        target: TargetFlags,
        /// Hide items smaller than this many MiB.
        #[arg(long, default_value_t = 1)]
        min_mb: u64,
    },
    /// Build artifacts (node_modules, target, .venv, ...) in project folders.
    Projects {
        /// Where to search (default: home).
        root: Option<PathBuf>,
        #[arg(long, default_value_t = 6)]
        max_depth: usize,
        /// Only projects untouched for this many days.
        #[arg(long, default_value_t = 0)]
        min_idle_days: u64,
        #[arg(long, default_value_t = 10)]
        min_mb: u64,
    },
    /// Write a deletion plan file. Deletes nothing.
    Plan {
        #[command(flatten)]
        target: TargetFlags,
        /// Include this rule ID (repeatable). Default: every ready, regenerable, sign-in-safe item.
        #[arg(long = "rule")]
        rule_ids: Vec<String>,
        /// Include this exact path (repeatable).
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        #[arg(long, value_enum, default_value = "none")]
        max_login_risk: Risk,
        /// Allow VM images and other non-regenerable items (with explicit --rule/--path).
        #[arg(long)]
        include_non_regenerable: bool,
        /// Also include build artifacts under this root (repeatable).
        #[arg(long = "projects")]
        project_roots: Vec<PathBuf>,
        #[arg(long, default_value_t = 30)]
        project_min_idle_days: u64,
        /// Where to write the plan.
        #[arg(long)]
        plan_file: Option<PathBuf>,
    },
    /// Apply a plan. Without --yes this is a dry run.
    Apply {
        #[arg(long)]
        plan: PathBuf,
        /// Actually delete.
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        target: TargetFlags,
    },
    /// List rule packs.
    Rules {
        #[command(flatten)]
        target: TargetFlags,
    },
    /// Install the skill and register the MCP server for detected AI agents. Dry run without --yes.
    Setup {
        /// Only these agents (repeatable). Default: every detected agent.
        #[arg(long = "agent", value_enum)]
        agents: Vec<setup::Agent>,
        /// Skip installing the skill.
        #[arg(long)]
        no_skill: bool,
        /// Skip registering the MCP server.
        #[arg(long)]
        no_mcp: bool,
        /// Remove what setup installed instead.
        #[arg(long)]
        uninstall: bool,
        /// Apply the steps.
        #[arg(long)]
        yes: bool,
    },
    /// Check the installation: binary, PATH, Full Disk Access, index, agent integrations.
    Doctor,
    /// Print the bundled skill (SKILL.md) to stdout.
    Skill,
    /// Run as an MCP server on stdio.
    Mcp {
        /// Let agents call apply_plan. Off by default: agents get the command for a human instead.
        #[arg(long)]
        allow_apply: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Risk {
    None,
    Possible,
    High,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let p = ops::platform();
    let p = p.as_ref();
    let v: Value = match &cli.cmd {
        Cmd::Mcp { allow_apply } => return mcp::serve(*allow_apply),
        Cmd::Skill => {
            print!("{}", setup::SKILL_MD);
            return Ok(());
        }
        Cmd::Doctor => setup::doctor()?,
        Cmd::Setup {
            agents,
            no_skill,
            no_mcp,
            uninstall,
            yes,
        } => setup::setup(&setup::SetupOpts {
            agents: agents.clone(),
            skill: !no_skill,
            mcp: !no_mcp,
            remove: *uninstall,
            apply: *yes,
        })?,
        Cmd::Overview { cache } => ops::overview(p, &cache.to_args(None))?,
        Cmd::Scan { path, cache } => ops::scan(p, &cache.to_args(Some(path.clone())))?,
        Cmd::Candidates { target, min_mb } => ops::candidates(p, &target.to_args(), min_mb << 20)?,
        Cmd::Projects {
            root,
            max_depth,
            min_idle_days,
            min_mb,
        } => ops::projects(&ops::ProjectArgs {
            root: root.clone(),
            max_depth: Some(*max_depth),
            min_idle_days: Some(*min_idle_days),
            min_mb: Some(*min_mb),
        })?,
        Cmd::Plan {
            target,
            rule_ids,
            paths,
            max_login_risk,
            include_non_regenerable,
            project_roots,
            project_min_idle_days,
            plan_file,
        } => ops::make_plan(
            p,
            &ops::PlanArgs {
                target: target.to_args(),
                selection: Selection {
                    rule_ids: rule_ids.clone(),
                    paths: paths.clone(),
                    max_login_risk: match max_login_risk {
                        Risk::None => LoginRisk::None,
                        Risk::Possible => LoginRisk::Possible,
                        Risk::High => LoginRisk::High,
                    },
                    min_bytes: 0,
                    include_non_regenerable: *include_non_regenerable,
                },
                project_roots: project_roots.clone(),
                project_min_idle_days: Some(*project_min_idle_days),
                out: plan_file.clone(),
            },
        )?,
        Cmd::Apply { plan, yes, target } => ops::apply(p, plan, !yes, &target.to_args())?,
        Cmd::Rules { target } => ops::list_rules(&target.to_args())?,
    };
    let text = if cli.pretty {
        serde_json::to_string_pretty(&v)?
    } else {
        v.to_string()
    };
    if let Some(out) = &cli.out {
        std::fs::write(out, &text)?;
    }
    if cli.human {
        print!("{}", ops::render_human(&v));
    } else {
        println!("{text}");
    }
    Ok(())
}

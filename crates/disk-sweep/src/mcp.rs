//! Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0). No SDK dependency:
//! the protocol surface we need is small and this keeps the binary lean and stable.

use crate::ops;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "disk-sweep finds reclaimable disk space. Workflow: \
1) `overview` to see volume usage and what could not be measured; \
2) `candidates` (and `projects` for code folders) to list removable items with login risk and whether apps are running; \
3) `make_plan` with the items the human approved; \
4) show the plan to the human. Deletion is permanent. Never ask for passwords: when a path needs admin rights, give the human the suggested command to run themselves. \
Items with login_risk other than `none`, or `regenerable: false` (VM images, simulator data), need an explicit human decision.";

fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn tools(allow_apply: bool) -> Value {
    let scan_props = json!({
        "depth": {"type": "integer", "description": "Levels of children to return (default 2)."},
        "top": {"type": "integer", "description": "Children kept per level (default 15)."},
        "cache": {"type": "string", "enum": ["use", "refresh", "off"], "description": "Reuse the scan index (default), rebuild it, or skip it."},
        "max_age_hours": {"type": "integer", "description": "Ignore an index older than this (default 168)."}
    });
    let target_props = json!({
        "all_users": {"type": "boolean", "description": "Evaluate every user's home. Other homes are only readable when running as root."},
        "homes": {"type": "array", "items": {"type": "string"}, "description": "Explicit home directories to evaluate."}
    });
    let mut scan_with_path = scan_props.clone();
    scan_with_path["path"] = json!({"type": "string", "description": "Directory to scan."});
    let mut plan_props = target_props.clone();
    for (k, v) in [
        (
            "rule_ids",
            json!({"type": "array", "items": {"type": "string"}, "description": "Rule IDs to include."}),
        ),
        (
            "paths",
            json!({"type": "array", "items": {"type": "string"}, "description": "Exact candidate paths to include."}),
        ),
        (
            "max_login_risk",
            json!({"type": "string", "enum": ["none", "possible", "high"], "description": "Default none."}),
        ),
        ("min_bytes", json!({"type": "integer"})),
        (
            "include_non_regenerable",
            json!({"type": "boolean", "description": "Allow VM images etc. Only with explicit rule_ids/paths."}),
        ),
        (
            "project_roots",
            json!({"type": "array", "items": {"type": "string"}, "description": "Also include build artifacts found under these roots."}),
        ),
        (
            "project_min_idle_days",
            json!({"type": "integer", "description": "Only projects idle this long (default 30)."}),
        ),
    ] {
        plan_props[k] = v;
    }
    let mut list = vec![
        json!({"name": "overview", "description": "Volume capacity, a size tree of the data volume, unreadable locations with hints, APFS snapshots, and the gap between used and measured space.", "inputSchema": schema(scan_props, &[]), "annotations": {"readOnlyHint": true}}),
        json!({"name": "scan", "description": "Size tree of one directory plus its largest files. Uses a persistent index (FSEvents on macOS) so repeat scans are fast.", "inputSchema": schema(scan_with_path, &["path"]), "annotations": {"readOnlyHint": true}}),
        json!({"name": "candidates", "description": "Known removable locations (caches, package caches, build outputs, VM images, protected profiles) with size, login risk, regenerability and whether a related app is running.", "inputSchema": schema({
            let mut p = target_props.clone(); p["min_mb"] = json!({"type": "integer", "description": "Hide items smaller than this (default 1)."}); p
        }, &[]), "annotations": {"readOnlyHint": true}}),
        json!({"name": "projects", "description": "Build artifacts (node_modules, target, .venv, ...) in project folders, with days since the project was last touched.", "inputSchema": schema(json!({
            "root": {"type": "string", "description": "Where to search (default: home)."},
            "max_depth": {"type": "integer"}, "min_idle_days": {"type": "integer"}, "min_mb": {"type": "integer"}
        }), &[]), "annotations": {"readOnlyHint": true}}),
        json!({"name": "make_plan", "description": "Freeze a deletion plan to a file. Deletes nothing. Returns the list and the exact command for the human to apply it.", "inputSchema": schema(plan_props, &[])}),
        json!({"name": "rules", "description": "List the rule packs that classify locations.", "inputSchema": schema(json!({}), &[]), "annotations": {"readOnlyHint": true}}),
    ];
    list.push(json!({"name": "apply_plan", "description": if allow_apply {
        "Apply a plan file. Re-checks every item against live rules first. dry_run defaults to true; pass dry_run=false only after the human approved this exact plan."
    } else {
        "Disabled on this server: returns the command for the human to run. Start the server with --allow-apply to enable."
    }, "inputSchema": schema(json!({"plan_file": {"type": "string"}, "dry_run": {"type": "boolean"}}), &["plan_file"]), "annotations": {"destructiveHint": true}}));
    Value::Array(list)
}

fn call(name: &str, args: Value, allow_apply: bool) -> Result<Value> {
    let p = ops::platform();
    let p = p.as_ref();
    match name {
        "overview" => ops::overview(p, &serde_json::from_value(args)?),
        "scan" => ops::scan(p, &serde_json::from_value(args)?),
        "candidates" => {
            let min = args.get("min_mb").and_then(|v| v.as_u64()).unwrap_or(1) << 20;
            let mut a = args.clone();
            if let Some(o) = a.as_object_mut() {
                o.remove("min_mb");
            }
            ops::candidates(p, &serde_json::from_value(a)?, min)
        }
        "projects" => ops::projects(&serde_json::from_value(args)?),
        "make_plan" => ops::make_plan(p, &serde_json::from_value(args)?),
        "rules" => ops::list_rules(&Default::default()),
        "apply_plan" => {
            let file = PathBuf::from(
                args.get("plan_file")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
            );
            if !allow_apply {
                return Ok(json!({
                    "applied": false,
                    "reason": "apply is disabled for agents on this server",
                    "command_for_human": format!("disk-sweep apply --plan {} --yes", sweep_core::platform::shell_quote(&file)),
                }));
            }
            let dry = args
                .get("dry_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            ops::apply(p, &file, dry, &Default::default())
        }
        _ => anyhow::bail!("unknown tool {name}"),
    }
}

fn respond(
    out: &mut impl Write,
    id: &Value,
    result: Result<Value, (i64, String)>,
) -> std::io::Result<()> {
    let msg = match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, m)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}})
        }
    };
    writeln!(out, "{msg}")?;
    out.flush()
}

pub fn serve(allow_apply: bool) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            respond(&mut out, &Value::Null, Err((-32700, "parse error".into())))?;
            continue;
        };
        let Some(id) = req.get("id").cloned() else {
            continue;
        }; // notification
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let res = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "disk-sweep", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools(allow_apply)})),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match call(&name, args, allow_apply) {
                    Ok(v) => {
                        json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v})
                    }
                    Err(e) => {
                        json!({"content": [{"type": "text", "text": format!("error: {e:#}")}], "isError": true})
                    }
                })
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        respond(&mut out, &id, res)?;
    }
    Ok(())
}

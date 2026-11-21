# disk-sweep

Disk cleanup built for AI agents and the humans who approve them.

[日本語](README.ja.md)

Most cleaners are built for a person clicking through a UI. `disk-sweep` is built for an agent (Claude Code, Codex, an MCP client) working *with* a person:

- **JSON everywhere**, with human-readable sizes alongside raw bytes.
- **Sign-in aware.** Every candidate carries a `login_risk`. Browser profiles, Playwright / DevTools MCP profiles and keychains are explicitly protected, even when they live under `Caches`.
- **Running-app aware.** Caches of open apps are reported as `blocked_running` instead of being deleted under the app's feet (macOS uses NSWorkspace bundle IDs).
- **Honest about what it cannot see.** `overview` compares the volume's used space with what it measured and lists every unreadable location with the reason (Unix permissions vs. macOS privacy/TCC) and a command the *human* can run. Agents are told never to ask for passwords.
- **Plan, then apply.** Agents write a plan file; applying re-evaluates every item against live rules and skips anything that changed. The MCP server cannot delete unless started with `--allow-apply`.
- **Fast repeat scans.** A persistent index stores per-directory totals. On macOS, FSEvents history tells the next scan exactly which directories changed, so a full-disk rescan drops from about a minute to a few seconds. Elsewhere, unchanged directory mtimes skip per-file stats.
- **Multi-user.** Under `sudo`, `--all-users` evaluates every account's home.

## Install

```sh
cargo install --git https://github.com/shiroinock/disk-sweep disk-sweep
```

The macOS layer is the default `macos` feature; on other platforms a portable implementation is used.

## Use

```sh
disk-sweep overview --depth 1 --human     # where is the space, and what couldn't be measured?
disk-sweep scan ~/Library --depth 2       # size tree + largest files (indexed)
disk-sweep candidates --human             # known caches, build outputs, VM images, protected profiles
disk-sweep projects ~/code --min-idle-days 30   # node_modules / target / .venv in idle projects
disk-sweep plan --rule npm-cache --rule xcode-derived-data
disk-sweep apply --plan /tmp/disk-sweep-123.plan.json        # dry run
disk-sweep apply --plan /tmp/disk-sweep-123.plan.json --yes  # delete
```

### As an MCP server

```json
{ "mcpServers": { "disk-sweep": { "command": "disk-sweep", "args": ["mcp"] } } }
```

Tools: `overview`, `scan`, `candidates`, `projects`, `make_plan`, `apply_plan`, `rules`.

### As a Claude skill

Copy `skill/disk-sweep` into your skills directory (for Claude Code: `~/.claude/skills/`).

## Rules

Rule packs are TOML files in [`rules/`](rules), embedded at build time. Add your own in `~/.config/disk-sweep/rules/`.

```toml
[[rule]]
id = "chrome-cache"
title = "Google Chrome cache"
paths = ["~/Library/Caches/Google/Chrome", "~/.cache/google-chrome"]
kind = "cache"              # cache | package-cache | build-artifact | vm-image | media | profile | app-data
login_risk = "none"         # none | possible | high
regenerable = true
action = "delete-contents"  # delete | delete-contents | keep
blocked_by_apps = ["com.google.Chrome"]
note = "Cookies are in the profile, so this does not sign you out."
```

`keep_latest = true` keeps the newest of versioned siblings (`chromium-1179`, `chromium-1194` → keeps 1194). `keep` rules protect paths even inside a `delete-contents` target.

## Layout

| crate | role |
|---|---|
| `sweep-core` | scanner, index, rules, project artifacts, plan/apply, `Platform` trait |
| `sweep-macos` | APFS capacity (`CapacityInUse`, important-usage), snapshots, NSWorkspace, FSEvents journal, TCC hints |
| `disk-sweep` | CLI and MCP server |

## Related tools

[Mole](https://github.com/tw93/Mole), [ncdu](https://dev.yorhel.nl/ncdu), [dua](https://github.com/Byron/dua-cli), [kondo](https://github.com/tbillington/kondo), [clean-dev-dirs](https://crates.io/crates/clean-dev-dirs), DaisyDisk, GrandPerspective. disk-sweep borrows their ideas (persistent index, project-artifact detection, largest-files view) and focuses on the agent-plus-human workflow.

## License

MIT OR Apache-2.0

---
name: disk-sweep
description: Free disk space safely with the disk-sweep CLI. Use when the user asks to clean up disk space, find what is using storage, clear caches, or investigate a nearly full disk. Covers measuring, explaining unreadable areas, choosing what is safe to delete (without signing the user out), and handing deletions to the human for approval.
---

# disk-sweep

`disk-sweep` prints JSON for every command. Nothing is deleted unless a plan is applied with `--yes`.

## Before you start

1. `disk-sweep --version`. If the command is missing, install it (no sudo, prints one JSON line at the end):
   `curl -fsSL https://raw.githubusercontent.com/shiroinock/disk-sweep/main/install.sh | sh`
   If the result says `"on_path":false`, call the binary by the `path` it reports.
2. `disk-sweep doctor --human` checks PATH, Full Disk Access and agent integrations, and lists `next_steps`.
   `disk-sweep setup` (dry run) / `disk-sweep setup --yes` installs this skill and the MCP server for the agents it finds.

## Workflow

1. **Measure.** `disk-sweep overview --depth 1`
   - `summary.unaccounted` is used space that could not be measured. If it is large, read `unmeasured`:
     - `reason: permission` (often another user's home): give the human `suggested_command` to run in their own terminal. Never ask for or type their password. Read the `--out` file afterwards.
     - `reason: privacy` (macOS TCC): sudo does not help. Tell the human to grant Full Disk Access to the app running disk-sweep.
   - `snapshots` lists APFS snapshots. `com.apple.os.update-*` on the system volume are managed by macOS; do not touch them.
2. **Drill down.** `disk-sweep scan <dir> --depth 2` for any large directory. `largest_files` lists files ≥ 50 MiB.
   Repeat scans reuse an index (FSEvents on macOS) and take seconds. Use `--refresh` if numbers look stale.
3. **List candidates.** `disk-sweep candidates --human` (add `--all-users` under sudo for every account).
   For code folders: `disk-sweep projects ~/code --min-idle-days 30`.
4. **Decide with the human.** Explain each group in plain words:
   - `status.state: ready`, `login_risk: none`, `regenerable: true`: safe; rebuilt automatically.
   - `blocked_running`: the related app is open. Ask the human to quit it first; deleting now would not free open files.
   - `protected`: holds sign-ins (browser profiles, Playwright/DevTools MCP profiles, keychains). Never delete.
   - `regenerable: false` (VM images, simulator devices): data inside is lost. Only with an explicit yes, and mention alternatives in `note` (e.g. `docker system prune`).
   - Anything in Downloads, Documents, Desktop or project sources is the human's data: show it, never plan it.
5. **Plan.** `disk-sweep plan --rule <id> ... [--path <p> ...] [--projects <root>]`
   Default selection is every ready, regenerable, sign-in-safe candidate. The result has `paths`, `total` and `apply_command`.
6. **Apply.** Show the plan list and total, get a clear yes, then either run `apply_command`, or, if it starts with `sudo`, give it to the human to run.
   `disk-sweep apply --plan <file>` without `--yes` is a dry run. Apply re-checks every item against live rules and skips anything that changed.
7. **Report** `summary.free_before` / `free_after` and anything in `skipped`.

## MCP

`disk-sweep mcp` exposes the same operations as tools (`overview`, `scan`, `candidates`, `projects`, `make_plan`, `apply_plan`, `rules`). `apply_plan` only returns a command for the human unless the server was started with `--allow-apply`.

## Custom rules

Drop TOML rule packs in `~/.config/disk-sweep/rules/` (same format as `rules/*.toml` in the repo). Use `action = "keep"` with `login_risk = "high"` for anything that holds sign-ins.

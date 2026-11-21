# Working on disk-sweep

Notes for AI agents (and humans) changing this repository. To *use* disk-sweep, see `skills/disk-sweep/SKILL.md`.

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
claude plugin validate .        # if you touched .claude-plugin/
```

CI runs the first three on macOS and Ubuntu with the latest stable Rust, which may lint more than your local toolchain.

## Layout

- `crates/sweep-core`: portable logic (scan, index, rules, projects, plan/apply). No OS-specific APIs beyond libc.
- `crates/sweep-macos`: everything macOS-only, behind `#[cfg(target_os = "macos")]`.
- `crates/disk-sweep`: CLI, MCP server (`mcp.rs`), agent setup (`setup.rs`). CLI and MCP must call the same functions in `ops.rs`.
- `rules/*.toml`: rule packs, embedded at build time. `skills/disk-sweep/SKILL.md`: embedded too (`disk-sweep skill`).

## Rules of the road

- Nothing may delete data outside `plan::apply`. New features produce candidates; they never remove files themselves.
- A rule that could sign someone out gets `login_risk = "possible"` or `"high"`. Profiles and credentials get `action = "keep"`.
- Anything that cannot be rebuilt gets `regenerable = false`.
- Tests and docs use temporary directories and generic paths. Never commit real usernames, home paths, emails or machine details.
- `setup` must stay a dry run without `--yes`, back up files before editing them, and be reversible with `--uninstall`.
- Keep `.claude-plugin/plugin.json` `version` equal to the crate version (a test checks this).

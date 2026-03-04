# CLAUDE.md

## Project overview

kuriboh is a Rust CLI that wraps Claude Code (`claude` binary) to run automated security reviews of Rust codebases. It installs subagent definition files into the target project, spawns a single Claude Code session with a phased orchestration prompt, streams NDJSON events, and produces a structured report.

## Build and run

```bash
# Source cargo if not on PATH (common in Docker sandbox)
source "$HOME/.cargo/env"

# Build
cargo build

# Run (requires `claude` CLI on PATH)
kuriboh --target /path/to/crate --no-sandbox
```

The project has no tests yet. Verify changes with `cargo build` (must produce 0 errors, 0 warnings).

## Architecture

### Execution flow

`main.rs`: `agents::install()` -> `runner::run()` -> `report::parse()` -> `report::write()` -> `agents::cleanup()`

### 5-phase orchestration prompt (in `runner.rs::build_prompt()`)

1. **Exploration** -- built-in Explore subagent surveys codebase -> `.kuriboh/exploration.md`
2. **Scouting** -- scout subagent (Haiku, parallel) per `.rs` file -> `.kuriboh/scores.json`
3. **Deep Review** -- reviewer agents with git worktrees, weighted-random file assignment, DFS approach
4. **Appraisal** -- appraiser per reviewer validates findings, tests PoCs, assigns verdicts
5. **Compilation** -- lead deduplicates and produces final report

### Key modules

- `runner.rs` -- Spawns Claude Code subprocess, streams stdout/stderr concurrently with `tokio::select!`, builds the orchestration prompt. TUI hook point: replace `Vec<ClaudeEvent>` with a channel sender.
- `events.rs` -- `ClaudeEvent` enum modeling Claude Code's `--output-format stream-json` NDJSON. Types: System, Assistant, User, Result.
- `sandbox.rs` -- `SandboxConfig::build_command()` returns `(program, argv)`. Conditionally adds `--dangerously-skip-permissions` based on CLI flag.
- `agents/templates.rs` -- 6 subagent definitions as `pub const &str` with YAML frontmatter + Markdown prompt. Agents: scout, reviewer, appraiser, unsafe-auditor, dep-checker, crypto-reviewer.
- `agents/mod.rs` -- `BUILTIN_AGENTS` registry, `install()` writes `.claude/agents/*.md` and creates `.kuriboh/{findings,worktrees,pocs}`, `cleanup()` handles git worktree removal then deletes `.kuriboh/`.
- `report.rs` -- `Report` and `Finding` structs with serde. `Finding` includes call_chain, poc_available/validated/path, verdict, appraiser_notes, independent_reviewers. Renders Markdown or JSON.
- `cli.rs` -- clap-derived Args. Notable: `--reviewers` (Option<u32>, dynamic default), `--max-turns` (400), `--keep-workspace`, `--dangerously-skip-permissions`, `--verbose`.

### `.kuriboh/` workspace layout

```
.kuriboh/
  exploration.md         # Phase 1
  scores.json            # Phase 2
  scouting-summary.md    # Phase 2
  task-assignments.json  # Phase 3
  findings/              # Phase 3-4 (reviewer-N.json, appraised-N.json)
  worktrees/             # Phase 3 (git worktrees per reviewer)
  pocs/                  # Phase 3 (PoC files per reviewer)
  compiled-findings.json # Phase 5
```

## Coding conventions

- Use `anyhow::Result` for error handling throughout.
- Use `tracing` for logging (`info!` for milestones, `debug!` for details, `warn!` for recoverable issues).
- Subagent templates are embedded as `pub const &str` in `templates.rs`. Each uses YAML frontmatter (`name`, `description`, `tools`, `model`, optional `background: true`).
- Adding a new subagent: (1) add `pub const` in `templates.rs`, (2) add `AgentDef` entry in `BUILTIN_AGENTS` in `mod.rs`.
- The orchestration prompt uses `{{{{ }}}}` for literal braces in `format!()` strings (double-escaped because it's inside `r#"..."#`).
- `cleanup()` must `git worktree remove --force` before `rm -rf .kuriboh/`.

## Important context

- Running `claude` inside another Claude Code session requires unsetting `CLAUDECODE=1` env var — handled by `.env_remove("CLAUDECODE")` in runner.rs.
- `--dangerously-skip-permissions` is passed through to the inner Claude Code session only when explicitly requested. It's safe when an isolation boundary is in place (native sandbox, Docker, container).
- `--verbose` is required when using `--output-format stream-json` with `--print` mode.
- Scout scoring: weighted linear sum (0-100) of 10 heuristic metrics. See `templates.rs::SCOUT` for the full rubric.
- Reviewer count default: `ceil(sqrt(total_scored_files))` clamped to [3, 12].

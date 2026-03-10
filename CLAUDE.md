# CLAUDE.md

## Project overview

kuriboh is a Rust CLI that wraps Claude Code (`claude` binary) to run automated security reviews of Rust codebases. A deterministic Rust outer scheduler drives a 5-phase pipeline, spawning 4 separate Claude Code sessions for semantic judgment only. Phases are idempotent and tracked in `state.json` with filesystem sentinels, enabling `--resume` from any failure point.

## Build and run

```bash
# Build
cargo build

# Run (requires `claude` CLI on PATH)
kuriboh --target /path/to/crate --dangerously-skip-permissions

# Resume a failed run
kuriboh --target /path/to/crate --resume

# Reproducible task assignments
kuriboh --target /path/to/crate --seed 42

# Review changes between two commits
kuriboh --target /path/to/crate --diff main..feature --dangerously-skip-permissions

# Estimate cost for diff review
kuriboh --target /path/to/crate --diff main..feature --estimate

# Review a GitHub pull request (requires `gh` CLI)
kuriboh --target /path/to/crate --pr 123 --dangerously-skip-permissions
kuriboh --target /path/to/crate --pr https://github.com/owner/repo/pull/123 --dangerously-skip-permissions
```

Run `cargo test` to execute tests. Verify changes with `cargo build` (must produce 0 errors, 0 warnings).

## Architecture

### Execution flow (deterministic outer scheduler)

```
main.rs phase loop:
  agents::install()
  load/create State from .kuriboh/state.json
  ├─ Phase 1: Exploration     → 1 claude session → sentinel: exploration.md
  ├─ Phase 2: Scouting        → Rust computes 7 static metrics (scanner.rs)
  │                            → 1 claude session (scout subagents for 3 LLM metrics)
  │                            → Rust merges + applies weighting → scores.json
  ├─ Phase 3: Deep Review     → Rust creates worktrees + task assignments
  │                            → 1 claude agent team session (lead + reviewer teammates)
  │                            → sentinel: all findings/reviewer-N.json exist
  ├─ Phase 4+5: Appraisal &   → 1 claude session (appraiser subagents + compilation)
  │  Compilation               → sentinel: compiled-findings.json
  └─ Report                   → Rust reads compiled-findings.json → writes report
  agents::cleanup()
```

4 Claude Code sessions total. Rust handles all deterministic work (file enumeration, static metrics, score merging, task assignment, worktree creation, sentinel checking, report generation). Claude handles semantic judgment only.

With `--diff base..head`: scouting/review scope narrows to changed `.rs` files only; diff hunks are injected into reviewer prompts. Worktrees use detached HEAD at the `head` ref.

### Key modules

- `main.rs` -- Phase loop: iterates phases, checks sentinels via `state::check_sentinel()`, manages `PhaseStatus` transitions, persists state after each phase. Contains per-phase async functions (`run_exploration`, `run_scouting`, `run_deep_review`, `run_appraisal_compilation`).
- `state.rs` -- `State` struct persisted to `.kuriboh/state.json`. `PhaseStatus` enum (Pending/Running/Done/Failed). `ReviewMode` enum (Full/Diff) with `FileStatus` and `DiffFile` types. `check_sentinel()` validates phase outputs. Atomic save via tmp+rename.
- `diff.rs` -- Git diff extraction: `resolve_diff()` parses `--diff base..head` range, extracts changed `.rs` files via `git diff --name-status`, collects unified diff hunks. `DiffContext` struct holds files + hunks. Rejects three-dot syntax, applies same SKIP_DIRS as scanner.
- `scanner.rs` -- File enumeration (`enumerate_files`), 7 static metrics (`compute_static_metrics`), LLM score loading/merging (`load_llm_scores`, `merge_scores`), weighted scoring (`compute_weighted_score`), seeded task assignment (`generate_assignments`), reviewer count calculation.
- `prompts.rs` -- Per-phase prompt builders: `exploration()`, `llm_scouting()`, `deep_review()`, `appraisal_and_compilation()`. Each returns a focused single-phase prompt.
- `runner.rs` -- Generic Claude Code session spawner. `run_session(args, SessionOpts)` takes a prompt and `agent_teams` flag. Streams NDJSON events.
- `events.rs` -- `ClaudeEvent` enum modeling Claude Code's `--output-format stream-json` NDJSON.
- `agents/templates.rs` -- 5 subagent definitions: scout (3 LLM metrics only), appraiser, unsafe-auditor, dep-checker, crypto-reviewer.
- `agents/mod.rs` -- `BUILTIN_AGENTS` registry, `install()` writes `.claude/agents/*.md`, `cleanup()` handles worktree removal then deletes `.kuriboh/`.
- `report.rs` -- `Report` and `Finding` structs. `parse_from_workspace()` reads compiled-findings.json directly. Renders Markdown or JSON.
- `cli.rs` -- clap-derived Args. Notable: `--reviewers`, `--max-turns` (400), `--resume`, `--seed`, `--keep-workspace`, `--dangerously-skip-permissions`, `--verbose`, `--estimate`, `--diff <base..head>`, `--pr <number_or_url>`.

### `.kuriboh/` workspace layout

```
.kuriboh/
  state.json               # Pipeline state (phases, files, assignments, seed)
  exploration.md           # Phase 1 output
  llm-scores.json          # Phase 2b (3 LLM metrics per file)
  scores.json              # Phase 2c (merged 10-metric scores)
  findings/                # Phase 3-4 (reviewer-N.json, appraised-N.json)
  worktrees/               # Phase 3 (git worktrees per reviewer)
  pocs/                    # Phase 3 (PoC files per reviewer)
  frontier/                # Phase 3 (reviewer-N.json priority queues)
  compiled-findings.json   # Phase 5 output
```

## Coding conventions

- Use `anyhow::Result` for error handling throughout.
- Use `tracing` for logging (`info!` for milestones, `debug!` for details, `warn!` for recoverable issues).
- Subagent templates are embedded as `pub const &str` in `templates.rs`. Each uses YAML frontmatter (`name`, `description`, `tools`, `model`, optional `background: true`).
- Adding a new specialist subagent: (1) add `pub const` in `templates.rs`, (2) add `AgentDef` entry in `BUILTIN_AGENTS` in `mod.rs`.
- Reviewers are agent team **teammates**, not subagents. Their instructions live in `prompts.rs::deep_review()`.
- Per-phase prompts use `{{{{ }}}}` for literal braces in `format!()` strings.
- `cleanup()` must `git worktree remove --force` before `rm -rf .kuriboh/`.
- Scout scoring: 7 metrics computed by Rust (scanner.rs), 3 by LLM (scout subagent). Weighted linear sum (0-100). Weights are inlined in `scanner.rs::compute_weighted_score()`.

## Important context

- Running `claude` inside another Claude Code session requires unsetting `CLAUDECODE=1` env var — handled by `.env_remove("CLAUDECODE")` in runner.rs.
- `--dangerously-skip-permissions` is passed through to inner Claude Code sessions only when explicitly requested.
- `--verbose` is required when using `--output-format stream-json` with `--print` mode.
- Reviewer count default: `ceil(sqrt(total_scored_files))` clamped to [3, 12].
- `--resume` loads existing `state.json`, validates target match, skips done phases whose sentinels pass, re-runs running/failed phases.
- Task assignments use a seeded RNG (`--seed` or random) for reproducibility.
- Agent teams are enabled unconditionally for Phase 3 (`agent_teams: true` in `SessionOpts`). There is no user-facing flag — `runner.rs` adds `--teammate-mode in-process` and sets `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` automatically. This is required because reviewers are teammates (full Claude Code sessions) that spawn specialist subagents (unsafe-auditor, dep-checker, crypto-reviewer). Claude Code prevents nested subagents, so reviewers must be teammates rather than subagents to use specialists.

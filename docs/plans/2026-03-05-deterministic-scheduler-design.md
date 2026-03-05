# Deterministic Outer Scheduler

Move deterministic workflow control from the LLM orchestration prompt into the Rust harness. Claude Code sessions handle semantic judgment only.

## Architecture

**Before:** Single `claude -p <monolithic_prompt>` call. The LLM controls file enumeration, metric computation, task assignment, worktree creation, and phase sequencing.

**After:** Rust outer scheduler drives the pipeline. Each phase is an idempotent step tracked in `.kuriboh/state.json`. Rust handles all deterministic work. Claude handles semantic judgment.

```
main.rs
  ├─ Phase 1: Exploration     → 1 claude session → sentinel: exploration.md
  ├─ Phase 2: Scouting        → Rust computes 7 static metrics
  │                            → 1 claude session (subagents per file for 3 LLM metrics)
  │                            → Rust merges + applies weighting formula → scores.json
  ├─ Phase 3: Deep Review     → Rust creates worktrees + task assignments
  │                            → 1 claude agent team session (lead + reviewer teammates)
  │                            → sentinel: all findings/reviewer-N.json exist
  ├─ Phase 4+5: Appraisal &   → 1 claude session
  │  Compilation               → appraiser subagents + final report synthesis
  │                            → sentinel: compiled-findings.json exists
  └─ Report                   → Rust parses compiled-findings.json → writes report
```

4 Claude Code sessions total.

## Module Structure

```
src/
  main.rs          — Phase loop: iterate phases, check sentinels, resume logic
  cli.rs           — Add --resume, --seed flags
  state.rs         — NEW: State, PhaseStatus, load/save state.json, sentinel checks
  scanner.rs       — NEW: File enumeration, static metrics, score merging,
                     weighting formula, task assignment generation
  runner.rs        — Refactor: generic session spawner + per-phase prompt builders
  agents/          — Remove scout template. Keep: unsafe-auditor, dep-checker,
                     crypto-reviewer, appraiser
  events.rs        — Unchanged
  report.rs        — Parse compiled-findings.json directly instead of raw Markdown
```

### state.rs

Owns `State` struct, serialization, sentinel verification. Schema:

```json
{
  "version": 1,
  "started_at": "ISO 8601",
  "target": "/path/to/crate",
  "seed": 12345,
  "phases": {
    "exploration":  { "status": "done", "session_id": "abc", "cost_usd": 0.15 },
    "scouting":     { "status": "done", "cost_usd": 0.0 },
    "deep_review":  { "status": "running", "session_id": "def", "cost_usd": 3.20 },
    "appraisal":    { "status": "pending" },
    "compilation":  { "status": "pending" }
  },
  "files": ["src/foo.rs", "src/bar.rs"],
  "reviewer_count": 5,
  "task_assignments": [
    { "reviewer_id": 1, "starting_file": "src/foo.rs", "scout_score": 87 }
  ]
}
```

Phase statuses: `pending`, `running`, `done`, `failed`. Updated atomically (write tmp + rename) after each phase.

### scanner.rs

All deterministic computation:

- `enumerate_files()` — walks target, skips `target/`, `vendor/`, `.git/`, `.kuriboh/`, `.claude/`, test files if >300 `.rs` files
- `compute_static_metrics()` — reads each file once, counts 7 metrics:
  - `loc` (non-blank, non-comment lines)
  - `unsafe_density` (unsafe blocks per 100 LoC)
  - `unwrap_density` (unwrap/expect calls per 100 LoC)
  - `raw_pointer_usage` (*mut/*const per 100 LoC)
  - `ffi_declarations` (extern blocks/fns)
  - `todo_fixme_hack` (TODO/FIXME/HACK comments)
  - `max_nesting_depth` (brace/keyword depth heuristic)
- `merge_scores()` — combines static + LLM metrics (error_handling_risk, macro_density, generic_complexity), applies weighting formula with combination bonus
- `generate_assignments()` — seeded RNG weighted-random sampling, writes to state

### runner.rs

Thin wrapper: `run_session(prompt, args) -> Vec<ClaudeEvent>`. Per-phase prompt builders as separate functions.

## Phase Details

### Phase 1: Exploration

- Rust creates `.kuriboh/` workspace dirs
- Spawns `claude -p` with focused exploration-only prompt
- Sentinel: `.kuriboh/exploration.md` exists and is >100 bytes

### Phase 2: Scouting

**2a: Static metrics (Rust, instant)**
- `scanner::enumerate_files()` + `scanner::compute_static_metrics()`
- Results stored in state

**2b: LLM metrics (Claude)**
- One session, spawns scout subagent per file for 3 metrics: `error_handling_risk`, `macro_density`, `generic_complexity`
- Scout subagent template trimmed to only these 3 metrics
- Sentinel: `.kuriboh/llm-scores.json` with entries for every file
- Missing entries filled with default score of 50

**2c: Merge (Rust, instant)**
- `scanner::merge_scores()` → `.kuriboh/scores.json`
- `scanner::generate_assignments()` with seeded RNG → stored in `state.json`

### Phase 3: Deep Review

- Rust creates git worktrees and PoC dirs per task assignment
- One `claude -p` session with agent teams enabled (`--teammate-mode in-process`)
- Prompt embeds pre-computed task assignments; lead spawns reviewer teammates
- Sentinel: all `findings/reviewer-N.json` files exist
- Partial completion (some reviewers missing): mark `failed`, `--resume` re-runs entire agent team session

### Phase 4+5: Appraisal & Compilation

- One `claude -p` session
- Spawns appraiser subagents per reviewer, then compiles deduplicated findings
- Sentinel: `.kuriboh/compiled-findings.json` exists and is valid JSON

### Report Generation (Rust, no Claude)

- `report.rs` reads `compiled-findings.json` directly
- Also reads `exploration.md`, `scores.json` for summaries
- Generates Markdown or JSON report

## `--resume` Semantics

- Loads existing `.kuriboh/state.json` instead of creating fresh state
- Validates `--target` matches stored target (error if mismatch)
- Reuses stored `seed`, `files`, `task_assignments`
- Skips `done` phases whose sentinels still pass
- Re-runs `running` or `failed` phases from scratch
- If a `done` phase's sentinel fails (file deleted), re-runs that phase

## Error Handling

- Non-zero claude exit: mark phase `failed`, log stderr
- Missing sentinel after successful exit: mark `failed`, reason "sentinel check failed"
- Partial LLM scout results: fill missing with default 50, still mark `done`
- Partial reviewer findings: mark `failed`, `--resume` re-runs agent team session

## New CLI Flags

- `--resume` — resume from existing state.json
- `--seed <N>` — explicit seed for reproducible task assignments (random if omitted)

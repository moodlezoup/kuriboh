use std::io::Write;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::cli::Args;
use crate::events::{self, ClaudeEvent, ContentBlock};

/// Spawn Claude Code, stream its `stream-json` output line-by-line, and
/// return the full sequence of parsed [`ClaudeEvent`]s.
///
/// The caller can process events in real time (e.g. to feed a TUI) before this
/// function returns, by instead adapting this to yield events via a channel —
/// the streaming architecture makes that straightforward.
pub async fn run(args: &Args) -> Result<Vec<ClaudeEvent>> {
    let prompt = build_prompt(args);

    let mut claude_args = Vec::new();
    if args.dangerously_skip_permissions {
        claude_args.push("--dangerously-skip-permissions".to_string());
    }
    if let Some(budget) = args.max_budget_usd {
        claude_args.extend(["--max-budget-usd".to_string(), budget.to_string()]);
    }
    claude_args.extend([
        "--model".to_string(),
        args.model.clone(),
        "--max-turns".to_string(),
        args.max_turns.to_string(),
        // --verbose is required for stream-json with --print.
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        // Run teammate sessions in-process (no tmux/iTerm2 required).
        "--teammate-mode".to_string(),
        "in-process".to_string(),
        // Non-interactive print mode.
        "-p".to_string(),
        prompt,
    ]);

    let program = "claude";

    tracing::info!(
        %program,
        model = %args.model,
        max_turns = args.max_turns,
        skip_permissions = args.dangerously_skip_permissions,
        "Spawning Claude Code"
    );
    // Full command at DEBUG to avoid leaking the orchestration prompt in CI logs.
    tracing::debug!(
        cmd = %format!("{program} {}", claude_args.iter().map(|a| {
            if a.contains(' ') || a.contains('"') { format!("'{a}'") } else { a.clone() }
        }).collect::<Vec<_>>().join(" ")),
        "Full command"
    );

    let mut child = Command::new(program)
        .args(&claude_args)
        // Prevent nested Claude Code session detection from blocking the subprocess.
        .env_remove("CLAUDECODE")
        // Enable the agent teams feature for Phase 3 reviewer teammates.
        .env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{program}` — is it installed and on PATH?"))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    // Stream stdout line-by-line. Each line is one NDJSON event.
    // This is the hook point for a future TUI: replace the Vec with a channel
    // sender and call `.send(event)` here to get real-time event delivery.
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();
    let mut collected: Vec<ClaudeEvent> = Vec::new();
    let mut stderr_buf = String::new();

    loop {
        tokio::select! {
            line = stdout_lines.next_line() => {
                match line.context("reading claude stdout")? {
                    None => break,
                    Some(l) => {
                        if let Some(ev) = events::parse_line(&l) {
                            if args.verbose {
                                print_event_text(&ev);
                            }
                            tracing::debug!(?ev, "event");
                            collected.push(ev);
                        }
                    }
                }
            }
            line = stderr_lines.next_line() => {
                if let Ok(Some(l)) = line {
                    tracing::debug!(stderr = %l);
                    stderr_buf.push_str(&l);
                    stderr_buf.push('\n');
                }
            }
        }
    }

    // Drain any remaining stderr after stdout closes.
    while let Ok(Some(l)) = stderr_lines.next_line().await {
        stderr_buf.push_str(&l);
        stderr_buf.push('\n');
    }

    let status = child.wait().await.context("waiting for claude to exit")?;
    if !status.success() {
        tracing::warn!(
            exit_code = status.code().unwrap_or(-1),
            "claude exited non-zero"
        );
    }

    if collected.is_empty() {
        bail!("claude produced no events. Stderr:\n{stderr_buf}");
    }

    Ok(collected)
}

/// Print assistant text content to stderr for `--verbose` mode.
fn print_event_text(ev: &ClaudeEvent) {
    let blocks = match ev {
        ClaudeEvent::Assistant { message, .. } => &message.content,
        _ => return,
    };
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    for block in blocks {
        if let ContentBlock::Text { text } = block {
            let _ = lock.write_all(text.as_bytes());
            let _ = lock.flush();
        }
    }
}

/// Builds the phased orchestration prompt for the agent team lead.
///
/// The review proceeds in five sequential phases:
/// 1. **Exploration** — bird's-eye survey using built-in Explore subagents
/// 2. **Scouting** — per-file complexity scoring using scout subagents (Haiku)
/// 3. **Deep Review** — reviewer **teammates** (agent team) with git worktrees;
///    each reviewer can spawn unsafe-auditor/dep-checker/crypto-reviewer subagents
/// 4. **Appraisal** — per-reviewer validation of findings, PoC testing (subagents)
/// 5. **Compilation** — deduplication and final report generation
fn build_prompt(args: &Args) -> String {
    let reviewers_directive = match args.reviewers {
        Some(n) => format!("{n}"),
        None => "0".to_string(),
    };
    format!(
        r#"You are the lead of a security review for a Rust codebase. You will
conduct the review in five sequential phases. Complete each phase fully before
moving to the next. Save intermediate results as files in `.kuriboh/` so that
subsequent phases and agents can reference them.

================================================================================
PHASE 1: EXPLORATION
================================================================================

Use the built-in **Explore** subagent (Claude Code's fast read-only agent) to
get a bird's-eye view of the codebase. Your exploration should identify:

1. Project structure (crate layout, module tree, entry points).
2. A catalog of every `.rs` file and its approximate purpose.
3. Architectural patterns: async runtime, FFI layers, unsafe hotspots, crypto
   usage, notable dependencies.
4. Build configuration (workspace vs single crate, feature flags).

After exploration, write the results to `.kuriboh/exploration.md`:

```markdown
# Codebase Exploration

## Project Structure
<module tree / crate layout>

## File Catalog
| File | Purpose | Notable Patterns |
|------|---------|-----------------|
| ...  | ...     | ...             |

## Architectural Notes
<async runtime, FFI layers, unsafe patterns, crypto usage, etc.>

## Initial Risk Indicators
<anything that stood out during exploration>
```

================================================================================
PHASE 2: SCOUTING
================================================================================

After Phase 1 is complete, spawn a **scout** subagent for **every `.rs` file**
listed in the file catalog. The scout agent is defined in `.claude/agents/scout.md`.

For each file:
- Spawn the scout with the prompt: "Score this file: <path>"
- Scouts run in background (parallel), use Haiku, and are read-only.
- Each scout returns a JSON object with per-metric scores and a weighted total.

**Scalability guard**: if the codebase has more than 300 `.rs` files, pre-filter
using exploration results — skip test files (`*_test.rs`, `tests/`), generated
code, and vendored dependencies. Scout only production source files.

After ALL scouts have reported, collect their results and write
`.kuriboh/scores.json`:

```json
{{{{
  "total_files": 0,
  "scored_at": "<ISO 8601>",
  "scores": [
    {{{{ "file": "src/foo.rs", "weighted_score": 87, "metrics": {{}}, "top_concerns": [] }}}},
    ...
  ],
  "priority_tiers": {{{{
    "critical": ["files with score >= 70"],
    "high":     ["files with score 50-69"],
    "medium":   ["files with score 30-49"],
    "low":      ["files with score < 30"]
  }}}}
}}}}
```

Also write a human-readable `.kuriboh/scouting-summary.md` with tier counts
and the top 10 highest-scoring files.

If a scout returns malformed JSON, log a warning and assign a default score of
50 for that file. Do not let one failed scout block the pipeline.

================================================================================
PHASE 3: DEEP REVIEW (AGENT TEAM)
================================================================================

Phase 3 uses an **agent team** for parallel deep review. Unlike subagents,
reviewer **teammates** are independent Claude Code sessions with their own full
context windows. This lets each reviewer spawn specialist subagents
(unsafe-auditor, dep-checker, crypto-reviewer) for targeted deep dives without
hitting the subagent nesting limit.

### Step 1: Determine reviewer count

The configured reviewer count is: {reviewers}

- If the value is 0: calculate dynamically as ceil(sqrt(total_scored_files)),
  clamped to the range [3, 12].
- Otherwise use the configured value directly.

### Step 2: Generate task assignments

1. Read `.kuriboh/scores.json` and collect all scored files with `weighted_score`.
2. For each reviewer task, randomly select a starting file using scores as weights:
   - probability(file_i) = max(score_i, 1) / sum(max(score_j, 1) for all j)
   - Sample WITH replacement: the same file CAN appear multiple times. This is
     intentional — high-risk files deserve multiple independent reviews.
3. Write `.kuriboh/task-assignments.json`:
   ```json
   [
     {{{{"reviewer_id": 1, "starting_file": "src/foo.rs", "scout_score": 87}}}},
     {{{{"reviewer_id": 2, "starting_file": "src/bar.rs", "scout_score": 72}}}},
     ...
   ]
   ```

### Step 3: Create git worktrees

For each task assignment:

1. Create a git worktree for the reviewer:
   ```bash
   git worktree add .kuriboh/worktrees/reviewer-N -b kuriboh-review-N
   ```
2. Create the PoC directory:
   ```bash
   mkdir -p .kuriboh/pocs/reviewer-N
   ```

### Step 4: Spawn reviewer teammates

Spawn a **reviewer teammate** (not a subagent) for each task assignment using
the agent team system. Teammates run as independent Claude Code sessions in
parallel, each with their own full context window.

Give each reviewer teammate the following spawn prompt (substitute their
specific N, path, and score values):

---BEGIN REVIEWER SPAWN PROMPT (substitute N, path, score)---
You are reviewer N in a parallel Rust security review.

Your assignment:
- Starting file: <path>
- Scout score: <score> (files rated 70+ are critical risk)
- Git worktree: .kuriboh/worktrees/reviewer-N  (work here to avoid conflicts)
- Findings output: .kuriboh/findings/reviewer-N.json
- PoC directory: .kuriboh/pocs/reviewer-N/

## Context

Read these two files first for codebase context:
- `.kuriboh/exploration.md` — architectural overview from Phase 1
- `.kuriboh/scores.json` — per-file risk scores from Phase 2

## Review Method: Depth-First Search

Starting from your assigned file:

1. Read the file thoroughly.
2. Identify vulnerabilities using all dimensions below.
3. Follow call chains: for any function/trait/macro that looks insecure, read
   the callee's source and recurse.
4. Stop recursing when you reach: standard library or well-audited external
   crates (unless misused), files you have already reviewed, or files with
   score < 20 and no suspicious patterns.

## Review Dimensions

### Memory Safety
- `unsafe` blocks: are invariants upheld? Could the block be made safe?
- Raw pointer arithmetic: overflow, alignment, provenance
- Use-after-free, double-free, aliasing violations
- Unsound `Send`/`Sync` impls

### Error Handling
- `unwrap()`/`expect()` on user-controlled or network-sourced data
- Swallowed errors (empty catch, `let _ = result`)
- Panic paths in library code

### Cryptography
- Weak algorithms (MD5, SHA-1, DES, RC4, ECB)
- Nonce/IV reuse, predictable RNG for secrets
- Missing authentication, incorrect key derivation
- Side-channel risks (non-constant-time secret comparisons)

### Input Validation & Injection
- SQL/command injection via string formatting
- Path traversal, symlink following
- Integer overflow leading to buffer miscalculation
- Deserialization of untrusted input without bounds

### Dependencies
- Known CVEs in transitively-included crates
- Unnecessary `unsafe` feature flags enabled
- Dependency confusion / typosquat risks

### Concurrency
- Data races (shared mutable state without synchronization)
- Deadlock potential (lock ordering violations)
- TOCTOU race conditions in file/network operations

## Specialist Subagents

You are a full Claude Code session and CAN spawn subagents for targeted deep
dives. Use these when you encounter code warranting specialized analysis:
- **unsafe-auditor**: files with `unsafe` blocks, raw pointers, or FFI
- **dep-checker**: Cargo.toml/Cargo.lock CVE and supply-chain analysis
- **crypto-reviewer**: cryptographic code, hashing, signing, or RNG usage

Spawn them with the file path or a specific question as the prompt.

## Proof of Concepts

When you find a vulnerability, attempt a PoC in your git worktree:
- File: `.kuriboh/pocs/reviewer-N/poc-<short-title>.rs` (or `.sh`)
- If it compiles and demonstrates the issue, set `poc_available: true`
- If you cannot write a PoC, explain why in the finding description.

## Output

Write findings to `.kuriboh/findings/reviewer-N.json` as a JSON array:

```json
[
  {{{{
    "severity": "CRITICAL|HIGH|MEDIUM|LOW|INFO",
    "title": "Short descriptive title",
    "file": "path/to/file.rs:line",
    "description": "What the vulnerability is and why it is dangerous",
    "recommendation": "How to fix or mitigate",
    "call_chain": ["file_a.rs:fn_x", "file_b.rs:fn_y"],
    "poc_available": false,
    "poc_path": null,
    "scout_score": 72,
    "files_reviewed": ["src/foo.rs", "src/bar.rs"]
  }}}}
]
```

If no vulnerabilities found, write `[]`.

## Completion

When done, message the lead: "Reviewer N complete: <total> findings
(<critical> critical, <high> high, <medium> medium, <low> low, <info> info).
Files reviewed: <count>."
Then shut down.
---END REVIEWER SPAWN PROMPT---

**Wait for all reviewer teammates to send their completion messages** before
proceeding to Phase 4.

================================================================================
PHASE 4: APPRAISAL
================================================================================

For each completed reviewer, spawn an **appraiser** subagent to validate their
findings. Appraisers may also run in parallel.

For each reviewer N:

1. Verify `.kuriboh/findings/reviewer-N.json` exists and is valid JSON.
   If the file is missing or empty, skip appraisal for this reviewer.
2. Spawn the **appraiser** subagent (defined in `.claude/agents/appraiser.md`)
   with this prompt:
   ```
   Appraise the findings from reviewer N.
   Findings file: .kuriboh/findings/reviewer-N.json
   Worktree path: .kuriboh/worktrees/reviewer-N
   Write appraised findings to: .kuriboh/findings/appraised-N.json
   ```
3. The appraiser validates each finding, tests PoCs, adjusts severity, and
   filters false positives. It assigns a verdict to each finding:
   - **confirmed**: vulnerability is real and severity is accurate
   - **adjusted**: vulnerability is real but severity was changed
   - **rejected**: false positive
   - **needs-review**: requires human judgment
4. If ALL findings were rejected, the appraiser removes the reviewer's worktree.

**Wait for ALL appraisers to complete** before proceeding to Phase 5.

================================================================================
PHASE 5: COMPILATION
================================================================================

Compile all appraised findings into a single deduplicated report.

### Step 1: Collect findings

Read all `.kuriboh/findings/appraised-*.json` files. Collect all findings with
verdict "confirmed", "adjusted", or "needs-review". Discard "rejected" findings.

### Step 2: Deduplicate

Group findings by (file, title). If multiple reviewers independently found the
same vulnerability:
- Keep the most detailed description and recommendation.
- Use the highest severity rating.
- Note the number of independent reviewers who flagged this issue.

### Step 3: Sort

Sort findings by severity (CRITICAL > HIGH > MEDIUM > LOW > INFO), then by
scout_score descending within the same severity level.

### Step 4: Coverage statistics

Calculate and report:
- Total number of reviewers that ran
- Total unique files reviewed across all reviewers
- Coverage of critical-tier files (% reviewed by at least one reviewer)
- Coverage of high-tier files
- List any critical/high-tier files NOT reached by any reviewer

### Step 5: Write compiled report

Write `.kuriboh/compiled-findings.json` with the deduplicated, sorted findings.

================================================================================
FINAL OUTPUT
================================================================================

Synthesize the compiled findings into a single Markdown report:

```
## Executive Summary
<2-4 sentence overview: how many reviewers ran, how many findings survived
appraisal, top risk areas referencing scouting scores>

## Scouting Overview
<tier counts, total files scored, top risk patterns>

## Review Coverage
<N reviewers, N unique files reviewed, coverage of critical/high tier files,
any critical/high files missed>

## Findings

### [SEVERITY] <title>
- **File**: `path/to/file.rs:line`
- **Scout Score**: <weighted_score from scouting>
- **Call Chain**: file_a.rs:fn_x -> file_b.rs:fn_y -> ...
- **Description**: ...
- **Recommendation**: ...
- **PoC**: <available / validated / none>
- **Independent Reviewers**: <N reviewers found this>
- **Appraiser Notes**: <any adjustments or validation notes>

(sorted CRITICAL -> HIGH -> MEDIUM -> LOW -> INFO)

## Needs Review
<findings with verdict "needs-review" that require human judgment, listed in
the same format as above>

## Remediation Roadmap
<prioritized action list informed by scouting scores and finding severity>
```

Target codebase: {target}
Max turns: {max_turns}{user_guidance}"#,
        reviewers = reviewers_directive,
        target = args.target.display(),
        max_turns = args.max_turns,
        user_guidance = match &args.prompt {
            Some(p) => format!("\n\n================================================================================\nUSER GUIDANCE\n================================================================================\n\nThe user has provided the following guidance for this review. Apply it across\nall phases — exploration should pay special attention to the areas mentioned,\nscouting should weight relevant files higher, reviewers should prioritize the\nspecified concerns, and appraisers should evaluate findings in this context:\n\n{p}"),
            None => String::new(),
        },
    )
}

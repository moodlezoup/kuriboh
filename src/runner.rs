use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::cli::Args;
use crate::events::{self, ClaudeEvent};
use crate::sandbox::SandboxConfig;

/// Spawn Claude Code (via Docker AI Sandbox), stream its `stream-json` output
/// line-by-line, and return the full sequence of parsed [`ClaudeEvent`]s.
///
/// The caller can process events in real time (e.g. to feed a TUI) before this
/// function returns, by instead adapting this to yield events via a channel —
/// the streaming architecture makes that straightforward.
pub async fn run(args: &Args, sandbox: &SandboxConfig) -> Result<Vec<ClaudeEvent>> {
    let prompt = build_prompt(args);

    // Flags passed to `claude` (sandbox.build_command may prepend more).
    let claude_args = vec![
        "--model".to_string(),
        args.model.clone(),
        "--max-turns".to_string(),
        args.max_turns.to_string(),
        // --verbose is required for stream-json with --print.
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        // Non-interactive print mode.
        "-p".to_string(),
        prompt,
    ];

    let (program, argv) = sandbox.build_command(&args.target, claude_args);

    tracing::info!(
        %program,
        model = %args.model,
        max_turns = args.max_turns,
        sandbox = sandbox.enabled,
        "Spawning Claude Code"
    );
    // Full command at DEBUG to avoid leaking the orchestration prompt in CI logs.
    tracing::debug!(
        cmd = %format!("{program} {}", argv.iter().map(|a| {
            if a.contains(' ') || a.contains('"') { format!("'{a}'") } else { a.clone() }
        }).collect::<Vec<_>>().join(" ")),
        "Full command"
    );

    let mut child = Command::new(&program)
        .args(&argv)
        // Prevent nested Claude Code session detection from blocking the subprocess.
        .env_remove("CLAUDECODE")
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
        tracing::warn!(exit_code = status.code().unwrap_or(-1), "claude exited non-zero");
    }

    if collected.is_empty() {
        bail!("claude produced no events. Stderr:\n{stderr_buf}");
    }

    Ok(collected)
}

/// Builds the phased orchestration prompt for the agent team lead.
///
/// The review proceeds in five sequential phases:
/// 1. **Exploration** — bird's-eye survey using built-in Explore subagents
/// 2. **Scouting** — per-file complexity scoring using scout subagents (Haiku)
/// 3. **Deep Review** — weighted-random reviewer agents with git worktrees
/// 4. **Appraisal** — per-reviewer validation of findings, PoC testing
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
PHASE 3: DEEP REVIEW
================================================================================

Using the scouting scores from `.kuriboh/scores.json`, create review tasks using
weighted random sampling, then spawn reviewer agents to execute them.

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

### Step 3: Create git worktrees and spawn reviewers

For each task assignment:

1. Create a git worktree for the reviewer:
   ```bash
   git worktree add .kuriboh/worktrees/reviewer-N -b kuriboh-review-N
   ```
2. Create the PoC directory:
   ```bash
   mkdir -p .kuriboh/pocs/reviewer-N
   ```
3. Spawn the **reviewer** subagent (defined in `.claude/agents/reviewer.md`) with
   this prompt:
   ```
   You are reviewer N. Your starting file is: <path>
   Scout score for this file: <score>
   Your git worktree is at: .kuriboh/worktrees/reviewer-N
   Write findings to: .kuriboh/findings/reviewer-N.json
   Write any PoCs to: .kuriboh/pocs/reviewer-N/
   ```
4. Reviewers run in **parallel** (background subagents).

The specialist subagents (unsafe-auditor, dep-checker, crypto-reviewer) are
available in `.claude/agents/`. Reviewers may spawn them as nested subagents
when they encounter files warranting deep specialized analysis.

**Wait for ALL reviewers to complete** before proceeding to Phase 4.

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
Max turns: {max_turns}"#,
        reviewers = reviewers_directive,
        target = args.target.display(),
        max_turns = args.max_turns,
    )
}

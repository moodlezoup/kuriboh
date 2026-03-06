use crate::state::TaskAssignment;

/// Phase 1: Exploration prompt. Focused on codebase survey only.
pub fn exploration(target: &str, user_guidance: Option<&str>) -> String {
    let guidance = match user_guidance {
        Some(g) => format!(
            "\n\nUSER GUIDANCE:\n{g}\n\nPay special attention to the areas mentioned above during exploration."
        ),
        None => String::new(),
    };
    format!(
        r"You are performing Phase 1 (Exploration) of a security review for a Rust codebase.

Use the built-in **Explore** subagent (Claude Code's fast read-only agent) to
get a bird's-eye view of the codebase. Your exploration should identify:

1. Project structure (crate layout, module tree, entry points).
2. A catalog of every `.rs` file and its approximate purpose.
3. Architectural patterns: async runtime, FFI layers, unsafe hotspots, crypto
   usage, notable dependencies.
4. Build configuration (workspace vs single crate, feature flags).

Write the results to `{target}/.kuriboh/exploration.md`:

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

Target codebase: {target}{guidance}"
    )
}

/// Phase 2b: LLM scouting prompt. Only asks for the 3 LLM metrics.
pub fn llm_scouting(target: &str, files: &[String]) -> String {
    let file_list = files
        .iter()
        .map(|f| format!("  - {f}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are performing Phase 2 (Scouting) of a security review. The Rust harness
has already computed static metrics for each file. You need to score 3 semantic
metrics that require reading the code.

For each file listed below, spawn a **scout** subagent (defined in
`.claude/agents/scout.md`) with the prompt: "Score this file: <path>"

Scouts run in background (parallel), use Haiku, and are read-only.

Files to score:
{file_list}

After ALL scouts have reported, collect their results and write
`{target}/.kuriboh/llm-scores.json` as a JSON array:

```json
[
  {{"file": "path/to/file.rs", "error_handling_risk": 50, "macro_density": 30, "generic_complexity": 20}},
  ...
]
```

If a scout returns malformed JSON, use default score of 50 for all 3 metrics
for that file. Do not let one failed scout block the pipeline.

## JSON Validity Gate

After writing ANY JSON file, immediately validate it:
  `python3 -m json.tool <file> > /dev/null`
If validation fails, read the file, fix the JSON, rewrite, and re-validate.
NEVER proceed to the next step with invalid JSON on disk."#
    )
}

/// Phase 3: Deep review prompt for the agent team lead.
///
/// Task assignments are pre-computed by Rust and embedded here.
pub fn deep_review(
    assignments: &[TaskAssignment],
    target: &str,
    max_turns: u32,
    user_guidance: Option<&str>,
) -> String {
    let guidance = match user_guidance {
        Some(g) => format!(
            "\n\nUSER GUIDANCE:\n{g}\n\nReviewers should prioritize the concerns mentioned above."
        ),
        None => String::new(),
    };

    let primary: Vec<&TaskAssignment> = assignments.iter().filter(|a| !a.reserve).collect();
    let reserves: Vec<&TaskAssignment> = assignments.iter().filter(|a| a.reserve).collect();

    let format_assignment = |a: &TaskAssignment| -> String {
        let lens_str = a
            .lens
            .as_ref()
            .map(|l| format!(", lens: {} — {}", l.name(), l.description()))
            .unwrap_or_default();
        let mandatory_str = if a.mandatory { ", mandatory" } else { "" };
        format!(
            "  - Reviewer {}: starting file `{}` (score: {}{}{}){}",
            a.reviewer_id,
            a.starting_file,
            a.scout_score,
            lens_str,
            mandatory_str,
            if a.reserve { " [RESERVE]" } else { "" }
        )
    };

    let primary_list = primary
        .iter()
        .map(|a| format_assignment(a))
        .collect::<Vec<_>>()
        .join("\n");

    let reserve_list = if reserves.is_empty() {
        String::new()
    } else {
        let items = reserves
            .iter()
            .map(|a| format_assignment(a))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\n## Reserve Slots (for adaptive allocation)\n\n{items}")
    };

    format!(
        r#"You are the lead of Phase 3 (Deep Review) of a security review for a Rust
codebase. The Rust harness has already created git worktrees and computed task
assignments. Your job is to spawn reviewer teammates and coordinate their work.

## Pre-computed Task Assignments

{primary_list}{reserve_list}

## Instructions

## Reviewer Agent Definition

The full reviewer methodology is defined in `.claude/agents/reviewer.md`
(frontier-based search, review dimensions, specialist subagent usage, output
schema, and completion protocol). Each reviewer teammate reads this file.
Pass ONLY the assignment-specific parameters in your spawn prompts.

For each **primary** assignment above, spawn a **reviewer teammate** (not a
subagent) using the agent team system. Teammates run as independent Claude Code
sessions in parallel, each with their own full context window.

Give each reviewer teammate the following spawn prompt (substitute their
specific N, path, score, lens, and lens_description values):

---BEGIN REVIEWER SPAWN PROMPT (substitute N, path, score, lens, lens_description)---
You are reviewer N in a parallel Rust security review. Read your full
methodology from `.claude/agents/reviewer.md` before starting.

Repo root: {target}
ALL paths below are absolute. Always use them as-is, even if you cd elsewhere.

Your assignment:
- Starting file: <path>
- Scout score: <score> (files rated 70+ are critical risk)
- Git worktree: {target}/.kuriboh/worktrees/reviewer-N  (work here to avoid conflicts)
- Findings output: {target}/.kuriboh/findings/reviewer-N.json
- PoC directory: {target}/.kuriboh/pocs/reviewer-N/
- Frontier file: {target}/.kuriboh/frontier/reviewer-N.json

Primary lens: **<lens>** — <lens_description>
While you must check ALL six review dimensions, spend ~40% of your effort on
your primary lens.

Context files (read these first):
- `{target}/.kuriboh/exploration.md` — architectural overview from Phase 1
- `{target}/.kuriboh/scores.json` — per-file risk scores from Phase 2
---END REVIEWER SPAWN PROMPT---

## Adaptive Allocation (Reserve Slots)

You have {reserve_count} reserve reviewer slots pre-created with worktrees and
PoC directories. Use them to strengthen coverage during the review.

**Frontier-informed reserve decisions:**
After 2+ primary reviewers have completed, read `{target}/.kuriboh/frontier/reviewer-*.json`
from all reviewers (files are written incrementally, readable while reviewers
are still running). Analyze the frontiers:
1. Collect all `pending` items across all reviewer frontiers.
2. Collect all `done` items to understand current coverage.
3. Identify high-priority unclaimed nodes (priority >= 70) that appear as
   `pending` in multiple frontiers — these are high-value unexplored leads.
4. Find module clusters where multiple pending items share a common path prefix.

**When to spawn a reserve reviewer:**
- High-priority frontier items pending across multiple reviewers (priority >= 70)
- A CRITICAL or HIGH finding has `repro_status: partial` and needs a dedicated
  PoC attempt
- Findings cluster in a module that needs deeper investigation
- A high-scoring module cluster has no primary coverage and appears as `pending`
  in multiple frontiers

**How to spawn:** Use the same reviewer spawn prompt template above, substituting
the reserve slot's N, path, score, lens, and lens_description values. Append an
"Additional Starting Points" section listing the top 5-10 pending items from
cross-frontier analysis:

```
## Additional Starting Points (from cross-frontier analysis)
- `src/parser.rs:parse_input` (priority: 85, reason: "Called from unsafe block",
  originally queued by: Reviewer 2)
- `src/net/tls.rs:handshake` (priority: 78, reason: "Tainted data from network",
  originally queued by: Reviewers 1, 4)
...
```

This turns reserves into "cleanup" reviewers that target the most promising
unexplored leads across all primary reviewers.

**For unused reserves:** When all primary reviewers are done and you decide not
to use a reserve slot, write `[]` to its findings file
(`{target}/.kuriboh/findings/reviewer-N.json`).

**Wait for all primary and any spawned reserve reviewer teammates to send their
completion messages** before reporting that Phase 3 is complete.

## JSON Validity Gate

After writing ANY JSON file (including `[]` for unused reserves), immediately
validate it:
  `python3 -m json.tool <file> > /dev/null`
If validation fails, read the file, fix the JSON, rewrite, and re-validate.
NEVER proceed to the next step with invalid JSON on disk.

Target codebase: {target}
Max turns: {max_turns}{guidance}"#,
        reserve_count = reserves.len()
    )
}

/// Phase 4+5: Appraisal and compilation prompt.
pub fn appraisal_and_compilation(reviewer_ids: &[u32], target: &str, max_turns: u32) -> String {
    let reviewer_list = reviewer_ids
        .iter()
        .map(|id| {
            format!("  - Reviewer {id}: findings at `{target}/.kuriboh/findings/reviewer-{id}.json`, worktree at `{target}/.kuriboh/worktrees/reviewer-{id}`")
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are performing Phases 4-5 (Appraisal & Compilation) of a security review.

## Phase 4: Appraisal

For each completed reviewer below, spawn an **appraiser** subagent (defined in
`.claude/agents/appraiser.md`) to validate their findings. Appraisers may run
in parallel.

Reviewers:
{reviewer_list}

For each reviewer N:
1. Verify `{target}/.kuriboh/findings/reviewer-N.json` exists and is valid JSON.
   If the file is missing or empty, skip appraisal for this reviewer.
2. Spawn the appraiser subagent with this prompt:
   "Appraise the findings from reviewer N.
   Findings file: {target}/.kuriboh/findings/reviewer-N.json
   Worktree path: {target}/.kuriboh/worktrees/reviewer-N
   Write appraised findings to: {target}/.kuriboh/findings/appraised-N.json"

**Wait for ALL appraisers to complete** before proceeding to Phase 5.

## Phase 5: Compilation

Compile all appraised findings into a single deduplicated report.

### Step 1: Collect findings
Read all `{target}/.kuriboh/findings/appraised-*.json` files. Collect all findings with
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

### Step 4: Write compiled report
Write `{target}/.kuriboh/compiled-findings.json` with the deduplicated, sorted findings
as a JSON array using this schema:

```json
[
  {{{{
    "severity": "CRITICAL",
    "original_severity": "HIGH",
    "title": "Short title",
    "file": "path/to/file.rs:line",
    "description": "...",
    "reachability": "...",
    "evidence": "...",
    "exploit_sketch": "...",
    "repro_status": "working",
    "recommendation": "...",
    "call_chain": ["..."],
    "poc_available": false,
    "poc_validated": null,
    "poc_path": null,
    "scout_score": 72,
    "verdict": "confirmed|adjusted|needs-review",
    "appraiser_notes": "...",
    "independent_reviewers": 2
  }}}}
]
```

Target codebase: {target}
Max turns: {max_turns}"#
    )
}

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

Write the results to `.kuriboh/exploration.md`:

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
pub fn llm_scouting(files: &[String]) -> String {
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
`.kuriboh/llm-scores.json` as a JSON array:

```json
[
  {{"file": "path/to/file.rs", "error_handling_risk": 50, "macro_density": 30, "generic_complexity": 20}},
  ...
]
```

If a scout returns malformed JSON, use default score of 50 for all 3 metrics
for that file. Do not let one failed scout block the pipeline."#
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

For each **primary** assignment above, spawn a **reviewer teammate** (not a
subagent) using the agent team system. Teammates run as independent Claude Code
sessions in parallel, each with their own full context window.

Give each reviewer teammate the following spawn prompt (substitute their
specific N, path, score, lens, and lens_description values):

---BEGIN REVIEWER SPAWN PROMPT (substitute N, path, score, lens, lens_description)---
You are reviewer N in a parallel Rust security review.

Your assignment:
- Starting file: <path>
- Scout score: <score> (files rated 70+ are critical risk)
- Git worktree: .kuriboh/worktrees/reviewer-N  (work here to avoid conflicts)
- Findings output: .kuriboh/findings/reviewer-N.json
- PoC directory: .kuriboh/pocs/reviewer-N/

## Primary Lens

Your primary lens is **<lens>**: <lens_description>
While you must check ALL six review dimensions, spend ~40% of your effort on
your primary lens. Investigate deeper, trace more call chains, and attempt PoCs
first for findings in your lens domain.

## Context

Read these two files first for codebase context:
- `.kuriboh/exploration.md` — architectural overview from Phase 1
- `.kuriboh/scores.json` — per-file risk scores from Phase 2

## Review Method: Frontier-Based Search

Maintain a live priority queue at `.kuriboh/frontier/reviewer-N.json`:

```json
[{{{{
  "node": "src/parser.rs:parse_input",
  "priority": 85,
  "reason": "Called from unsafe block, handles user input",
  "source": "call_chain|score_based|taint_propagation|pattern_match",
  "tainted_sources": ["stdin", "network_socket"],
  "status": "pending|exploring|done|pruned"
}}}}]
```

Workflow:
1. **Seed**: Read your starting file; add its functions + high-score neighbors
   (>= 50 in scores.json) to the frontier as `pending`.
2. **Pop**: Take the highest-priority `pending` item, set it to `exploring`.
3. **Examine**: Read the code, discover new leads (callees, callers, trait
   impls), add them to the frontier with computed priority. No duplicates —
   update priority if higher.
4. **Record**: Mark current item `done`, write any findings.
5. **Backtrack**: Compare the next call-chain target's priority vs the top
   `pending` item in the frontier. Switch to the higher-value target instead
   of blindly going deeper.
6. **Prune**: Mark safe items `pruned` with a reason (e.g. "data validated
   before reaching this node", "wrapper around safe stdlib call").
7. **Write incrementally**: Update the frontier file after each node, not just
   at end. The lead reads these files for reserve allocation decisions.
8. **Stop adding**: stdlib functions, items with score < 20, items already
   `done`.

Priority heuristic (0-100):
- Base: file's `weighted_score` from scores.json
- +20 if called from unsafe context
- +15 if handles tainted/user-controlled data
- +10 if in your primary lens domain
- -10 if already partially covered by starting file analysis
- Clamp to [0, 100]

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

Write findings to `.kuriboh/findings/reviewer-N.json` as a JSON array.
All fields marked * are **required**:

```json
[
  {{{{
    "severity": "CRITICAL|HIGH|MEDIUM|LOW|INFO",
    "title": "Short descriptive title",
    "file": "path/to/file.rs:line",
    "description": "What the vulnerability is and why it is dangerous",
    "reachability": "* How attacker-controlled input flows from entry point to the vulnerable sink (e.g. 'HTTP body → deserialize() → foo() → unsafe write at bar.rs:42')",
    "evidence": "* Exact file:line + 1-3 line snippet obtained via Read or `rg -n`. E.g. 'src/foo.rs:42: ptr.write(val)  // val is user-controlled'",
    "exploit_sketch": "* Minimal conditions needed to trigger: e.g. 'Send POST /api with JSON field `len` > usize::MAX; server reads len bytes from attacker body'",
    "repro_status": "* not_tried|partial|working|not_reproducible",
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

## Adaptive Allocation (Reserve Slots)

You have {reserve_count} reserve reviewer slots pre-created with worktrees and
PoC directories. Use them to strengthen coverage during the review.

**Frontier-informed reserve decisions:**
After 2+ primary reviewers have completed, read `.kuriboh/frontier/reviewer-*.json`
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
(`.kuriboh/findings/reviewer-N.json`).

**Wait for all primary and any spawned reserve reviewer teammates to send their
completion messages** before reporting that Phase 3 is complete.

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
            format!("  - Reviewer {id}: findings at `.kuriboh/findings/reviewer-{id}.json`, worktree at `.kuriboh/worktrees/reviewer-{id}`")
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
1. Verify `.kuriboh/findings/reviewer-N.json` exists and is valid JSON.
   If the file is missing or empty, skip appraisal for this reviewer.
2. Spawn the appraiser subagent with this prompt:
   "Appraise the findings from reviewer N.
   Findings file: .kuriboh/findings/reviewer-N.json
   Worktree path: .kuriboh/worktrees/reviewer-N
   Write appraised findings to: .kuriboh/findings/appraised-N.json"

**Wait for ALL appraisers to complete** before proceeding to Phase 5.

## Phase 5: Compilation

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

### Step 4: Write compiled report
Write `.kuriboh/compiled-findings.json` with the deduplicated, sorted findings
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

Also write a final Markdown report as the session output with sections:
- Executive Summary
- Scouting Overview
- Review Coverage
- Findings (sorted CRITICAL -> INFO)
- Needs Review
- Remediation Roadmap

Target codebase: {target}
Max turns: {max_turns}"#
    )
}

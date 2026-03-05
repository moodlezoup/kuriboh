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
        r#"You are performing Phase 1 (Exploration) of a security review for a Rust codebase.

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

Target codebase: {target}{guidance}"#
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

    let assignment_list = assignments
        .iter()
        .map(|a| {
            format!(
                "  - Reviewer {}: starting file `{}` (scout score: {})",
                a.reviewer_id, a.starting_file, a.scout_score
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are the lead of Phase 3 (Deep Review) of a security review for a Rust
codebase. The Rust harness has already created git worktrees and computed task
assignments. Your job is to spawn reviewer teammates and coordinate their work.

## Pre-computed Task Assignments

{assignment_list}

## Instructions

For each assignment above, spawn a **reviewer teammate** (not a subagent) using
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
reporting that Phase 3 is complete.

Target codebase: {target}
Max turns: {max_turns}{guidance}"#
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
    "title": "Short title",
    "file": "path/to/file.rs:line",
    "description": "...",
    "recommendation": "...",
    "call_chain": ["..."],
    "poc_available": false,
    "poc_validated": null,
    "poc_path": null,
    "scout_score": 72,
    "verdict": "confirmed",
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

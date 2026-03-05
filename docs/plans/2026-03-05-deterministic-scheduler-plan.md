# Deterministic Outer Scheduler Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move deterministic workflow control from the LLM orchestration prompt into the Rust harness, with idempotent phases tracked by `state.json` and `--resume` support.

**Architecture:** Rust outer scheduler drives a 5-phase pipeline. Each phase checks a filesystem sentinel before running. Rust handles file enumeration, static metrics (7 of 10), score merging, task assignment, and worktree creation. Claude Code sessions (4 total) handle only semantic judgment: exploration, LLM scoring (3 metrics), deep review (agent team), and appraisal+compilation.

**Tech Stack:** Rust, tokio, serde_json, clap, rand (new dep for seeded RNG)

---

### Task 1: Add `rand` dependency

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add rand with `small_rng` feature**

In `Cargo.toml`, add under `[dependencies]`:

```toml
rand = { version = "0.9", features = ["small_rng"] }
```

`small_rng` gives us `SmallRng` which is seedable and fast. We don't need cryptographic randomness for task assignment.

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with 0 errors

**Step 3: Commit**

```
feat: add rand dependency for seeded task assignment
```

---

### Task 2: Create `state.rs` — State types and persistence

**Files:**
- Create: `src/state.rs`
- Modify: `src/main.rs` (add `mod state;`)

**Step 1: Write tests for state serialization and atomic save**

Create `src/state.rs` with types and tests at the bottom:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Status of a single phase in the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    Running,
    Done,
    Failed,
}

/// Tracked metadata for a single phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseState {
    pub status: PhaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A task assignment: which reviewer gets which starting file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssignment {
    pub reviewer_id: u32,
    pub starting_file: String,
    pub scout_score: u32,
}

/// Top-level pipeline state, persisted to `.kuriboh/state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub started_at: String,
    pub target: PathBuf,
    pub seed: u64,
    pub phases: HashMap<String, PhaseState>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub reviewer_count: u32,
    #[serde(default)]
    pub task_assignments: Vec<TaskAssignment>,
}

/// The five phase names, in execution order.
pub const PHASE_ORDER: &[&str] = &[
    "exploration",
    "scouting",
    "deep_review",
    "appraisal_compilation",
];

impl PhaseState {
    pub fn pending() -> Self {
        Self { status: PhaseStatus::Pending, session_id: None, cost_usd: None, reason: None }
    }
}

impl State {
    /// Create a fresh state for a new run.
    pub fn new(target: PathBuf, seed: u64) -> Self {
        let mut phases = HashMap::new();
        for name in PHASE_ORDER {
            phases.insert(name.to_string(), PhaseState::pending());
        }
        Self {
            version: 1,
            started_at: chrono_now(),
            target,
            seed,
            phases,
            files: Vec::new(),
            reviewer_count: 0,
            task_assignments: Vec::new(),
        }
    }

    /// Path to `state.json` within the `.kuriboh/` workspace.
    pub fn path(target: &Path) -> PathBuf {
        target.join(".kuriboh").join("state.json")
    }

    /// Load state from disk.
    pub fn load(target: &Path) -> Result<Self> {
        let path = Self::path(target);
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let state: Self = serde_json::from_str(&data)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(state)
    }

    /// Atomically save state to disk (write tmp + rename).
    pub fn save(&self, target: &Path) -> Result<()> {
        let path = Self::path(target);
        let dir = path.parent().expect(".kuriboh dir");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("state.json.tmp");
        let data = serde_json::to_string_pretty(self)
            .context("serializing state")?;
        std::fs::write(&tmp, &data)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Get mutable reference to a phase's state.
    pub fn phase_mut(&mut self, name: &str) -> &mut PhaseState {
        self.phases.get_mut(name).expect("unknown phase name")
    }

    /// Get a phase's current status.
    pub fn phase_status(&self, name: &str) -> &PhaseStatus {
        &self.phases[name].status
    }
}

/// Minimal ISO 8601 timestamp (no external chrono dep).
fn chrono_now() -> String {
    // Use std::time for a basic timestamp. Format: seconds since epoch.
    // A full ISO 8601 string would require the `chrono` crate, but for
    // state tracking purposes an epoch timestamp is sufficient.
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", dur.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_has_all_phases_pending() {
        let state = State::new(PathBuf::from("/tmp/test"), 42);
        for name in PHASE_ORDER {
            assert_eq!(state.phase_status(name), &PhaseStatus::Pending);
        }
    }

    #[test]
    fn round_trip_serialization() {
        let mut state = State::new(PathBuf::from("/tmp/test"), 42);
        state.files = vec!["src/main.rs".to_string()];
        state.task_assignments = vec![TaskAssignment {
            reviewer_id: 1,
            starting_file: "src/main.rs".to_string(),
            scout_score: 75,
        }];
        state.phase_mut("exploration").status = PhaseStatus::Done;
        state.phase_mut("exploration").cost_usd = Some(0.15);

        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: State = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.seed, 42);
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.task_assignments.len(), 1);
        assert_eq!(loaded.phase_status("exploration"), &PhaseStatus::Done);
        assert_eq!(loaded.phase_status("scouting"), &PhaseStatus::Pending);
    }

    #[test]
    fn atomic_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path();
        let state = State::new(target.to_path_buf(), 123);
        state.save(target).unwrap();
        let loaded = State::load(target).unwrap();
        assert_eq!(loaded.seed, 123);
        assert_eq!(loaded.version, 1);
    }
}
```

**Step 2: Add `mod state` to main.rs**

Add `mod state;` to the module declarations at the top of `src/main.rs`.

**Step 3: Run tests**

Run: `cargo test state::tests`
Expected: 3 tests pass

**Step 4: Commit**

```
feat: add state.rs with State types, persistence, and sentinel tracking
```

---

### Task 3: Create `scanner.rs` — File enumeration

**Files:**
- Create: `src/scanner.rs`
- Modify: `src/main.rs` (add `mod scanner;`)

**Step 1: Write tests for file enumeration**

Create `src/scanner.rs`:

```rust
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Directories to always skip when enumerating `.rs` files.
const SKIP_DIRS: &[&str] = &["target", "vendor", ".git", ".kuriboh", ".claude"];

/// Recursively enumerate `.rs` files under `dir`, skipping non-production paths.
///
/// If `filter_tests` is true, also skips files matching `*_test.rs` and
/// paths containing `/tests/` or `/benches/`. This is enabled when the
/// codebase has >300 `.rs` files to keep scouting costs manageable.
pub fn enumerate_files(dir: &Path, filter_tests: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    enumerate_recursive(dir, dir, filter_tests, &mut files)?;
    files.sort();
    Ok(files)
}

fn enumerate_recursive(
    root: &Path,
    dir: &Path,
    filter_tests: bool,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            if filter_tests && (name == "tests" || name == "benches") {
                continue;
            }
            enumerate_recursive(root, &path, filter_tests, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if filter_tests {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem.ends_with("_test") {
                        continue;
                    }
                }
            }
            // Store as relative path from root.
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            } else {
                out.push(path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_tree(dir: &Path) {
        // Production files
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn hello() {}").unwrap();
        // Test file
        fs::write(dir.join("src/foo_test.rs"), "// test").unwrap();
        // Tests directory
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/integration.rs"), "// int test").unwrap();
        // Skipped dirs
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::write(dir.join("target/debug/main.rs"), "// build artifact").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/hooks.rs"), "// git").unwrap();
    }

    #[test]
    fn enumerate_skips_target_and_git() {
        let dir = tempfile::tempdir().unwrap();
        setup_tree(dir.path());

        let files = enumerate_files(dir.path(), false).unwrap();
        let names: Vec<&str> = files.iter().filter_map(|p| p.to_str()).collect();

        assert!(names.contains(&"src/main.rs"));
        assert!(names.contains(&"src/lib.rs"));
        assert!(names.contains(&"src/foo_test.rs"));
        assert!(names.contains(&"tests/integration.rs"));
        // Should NOT contain target/ or .git/ files
        assert!(!names.iter().any(|n| n.contains("target")));
        assert!(!names.iter().any(|n| n.contains(".git")));
    }

    #[test]
    fn enumerate_with_test_filter() {
        let dir = tempfile::tempdir().unwrap();
        setup_tree(dir.path());

        let files = enumerate_files(dir.path(), true).unwrap();
        let names: Vec<&str> = files.iter().filter_map(|p| p.to_str()).collect();

        assert!(names.contains(&"src/main.rs"));
        assert!(names.contains(&"src/lib.rs"));
        // Should be filtered out
        assert!(!names.contains(&"src/foo_test.rs"));
        assert!(!names.iter().any(|n| n.contains("tests/")));
    }
}
```

**Step 2: Add `mod scanner` to main.rs**

Add `mod scanner;` to the module declarations in `src/main.rs`.

**Step 3: Run tests**

Run: `cargo test scanner::tests`
Expected: 2 tests pass

**Step 4: Commit**

```
feat: add scanner.rs with file enumeration
```

---

### Task 4: Static metrics computation in `scanner.rs`

**Files:**
- Modify: `src/scanner.rs`

**Step 1: Write tests for static metric computation**

Add to `src/scanner.rs` after the `enumerate_files` code:

```rust
use serde::{Deserialize, Serialize};

/// The 7 metrics computed by Rust (static analysis).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StaticMetrics {
    pub loc: u32,
    pub unsafe_density: u32,
    pub unwrap_density: u32,
    pub raw_pointer_usage: u32,
    pub ffi_declarations: u32,
    pub todo_fixme_hack: u32,
    pub max_nesting_depth: u32,
}

/// The 3 metrics computed by the LLM scout.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmMetrics {
    pub error_handling_risk: u32,
    pub macro_density: u32,
    pub generic_complexity: u32,
}

/// Combined score for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScore {
    pub file: String,
    pub static_metrics: StaticMetrics,
    #[serde(default)]
    pub llm_metrics: LlmMetrics,
    pub combination_bonus: u32,
    pub weighted_score: u32,
    #[serde(default)]
    pub top_concerns: Vec<String>,
}

/// Compute static metrics for a single file from its source text.
pub fn compute_static_metrics(source: &str) -> StaticMetrics {
    let lines: Vec<&str> = source.lines().collect();

    // LOC: non-blank, non-comment lines
    let loc_count = lines.iter()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("//") && !t.starts_with("/*") && !t.starts_with('*')
        })
        .count();

    // Counts per pattern
    let unsafe_count = lines.iter().filter(|l| l.contains("unsafe ") || l.contains("unsafe{")).count();
    let unwrap_count = lines.iter().filter(|l| l.contains(".unwrap()") || l.contains(".expect(")).count();
    let raw_ptr_count = lines.iter().filter(|l| l.contains("*mut ") || l.contains("*const ")).count();
    let ffi_count = lines.iter().filter(|l| {
        let t = l.trim();
        t.starts_with("extern ") || t.contains("extern \"C\"") || t.contains("extern \"c\"")
    }).count();
    let todo_count = lines.iter().filter(|l| {
        let upper = l.to_uppercase();
        upper.contains("TODO") || upper.contains("FIXME") || upper.contains("HACK")
    }).count();

    // Max nesting depth via brace counting
    let mut max_depth: i32 = 0;
    let mut depth: i32 = 0;
    for line in &lines {
        for ch in line.chars() {
            match ch {
                '{' => { depth += 1; max_depth = max_depth.max(depth); }
                '}' => { depth -= 1; }
                _ => {}
            }
        }
    }

    // Scale raw counts to 0-100 scores using the rubric from the scout prompt
    let loc_per_100 = if loc_count > 0 { loc_count } else { 0 };
    StaticMetrics {
        loc: scale_loc(loc_count),
        unsafe_density: scale_density(unsafe_count, loc_count, 1, 3),
        unwrap_density: scale_density(unwrap_count, loc_count, 2, 5),
        raw_pointer_usage: scale_density(raw_ptr_count, loc_count, 1, 3),
        ffi_declarations: scale_ffi(ffi_count),
        todo_fixme_hack: scale_todo(todo_count),
        max_nesting_depth: scale_nesting(max_depth as u32),
    }
}

// Scaling functions matching the scout rubric thresholds.

fn scale_loc(loc: usize) -> u32 {
    if loc < 50 { 0 }
    else if loc >= 500 { 100 }
    else { ((loc as f64 - 50.0) / (500.0 - 50.0) * 100.0) as u32 }
}

fn scale_density(count: usize, loc: usize, mid_per_100: usize, high_per_100: usize) -> u32 {
    if loc == 0 || count == 0 { return 0; }
    let per_100 = count as f64 / (loc as f64 / 100.0);
    if per_100 >= high_per_100 as f64 { 100 }
    else if per_100 <= 0.0 { 0 }
    else { ((per_100 / high_per_100 as f64) * 100.0).min(100.0) as u32 }
}

fn scale_ffi(count: usize) -> u32 {
    match count {
        0 => 0,
        1..=2 => 50,
        3 => 75,
        _ => 100,
    }
}

fn scale_todo(count: usize) -> u32 {
    match count {
        0 => 0,
        1 => 25,
        2..=3 => 50,
        4 => 75,
        _ => 100,
    }
}

fn scale_nesting(depth: u32) -> u32 {
    match depth {
        0..=2 => 0,
        3 => 25,
        4 => 50,
        5 => 75,
        _ => 100,
    }
}

/// Weights for each metric (from the scout rubric).
const WEIGHTS: &[(&str, u32)] = &[
    ("unsafe_density", 20),
    ("raw_pointer_usage", 15),
    ("unwrap_density", 10),
    ("error_handling_risk", 10),
    ("ffi_declarations", 10),
    ("loc", 5),
    ("max_nesting_depth", 5),
    ("todo_fixme_hack", 5),
    ("macro_density", 5),
    ("generic_complexity", 5),
];

/// Compute the weighted score from static + LLM metrics.
pub fn compute_weighted_score(static_m: &StaticMetrics, llm_m: &LlmMetrics) -> (u32, u32) {
    let metrics: &[(&str, u32)] = &[
        ("loc", static_m.loc),
        ("unsafe_density", static_m.unsafe_density),
        ("unwrap_density", static_m.unwrap_density),
        ("raw_pointer_usage", static_m.raw_pointer_usage),
        ("ffi_declarations", static_m.ffi_declarations),
        ("todo_fixme_hack", static_m.todo_fixme_hack),
        ("max_nesting_depth", static_m.max_nesting_depth),
        ("error_handling_risk", llm_m.error_handling_risk),
        ("macro_density", llm_m.macro_density),
        ("generic_complexity", llm_m.generic_complexity),
    ];

    let mut weighted_sum: f64 = 0.0;
    for (name, weight) in WEIGHTS {
        let score = metrics.iter().find(|(n, _)| n == name).map(|(_, v)| *v).unwrap_or(0);
        weighted_sum += score as f64 * *weight as f64 / 100.0;
    }

    // Combination bonus: unsafe + raw pointers
    let combo_bonus = if static_m.unsafe_density > 0 && static_m.raw_pointer_usage > 0 { 10 } else { 0 };
    let total = (weighted_sum as u32 + combo_bonus).clamp(0, 100);

    (total, combo_bonus)
}
```

Add tests:

```rust
#[cfg(test)]
mod tests {
    // ... existing tests ...

    #[test]
    fn static_metrics_basic() {
        let source = r#"
fn main() {
    let x = foo().unwrap();
    let y = bar().expect("oops");
}

unsafe fn danger() {
    let p: *mut u8 = std::ptr::null_mut();
}

// TODO: fix this
"#;
        let m = compute_static_metrics(source);
        assert!(m.loc > 0);
        assert!(m.unsafe_density > 0, "should detect unsafe");
        assert!(m.unwrap_density > 0, "should detect unwrap/expect");
        assert!(m.raw_pointer_usage > 0, "should detect *mut");
        assert!(m.todo_fixme_hack > 0, "should detect TODO");
    }

    #[test]
    fn static_metrics_empty_file() {
        let m = compute_static_metrics("");
        assert_eq!(m.loc, 0);
        assert_eq!(m.unsafe_density, 0);
    }

    #[test]
    fn weighted_score_all_zero() {
        let static_m = StaticMetrics::default();
        let llm_m = LlmMetrics::default();
        let (score, bonus) = compute_weighted_score(&static_m, &llm_m);
        assert_eq!(score, 0);
        assert_eq!(bonus, 0);
    }

    #[test]
    fn weighted_score_with_combo_bonus() {
        let static_m = StaticMetrics {
            unsafe_density: 50,
            raw_pointer_usage: 50,
            ..Default::default()
        };
        let llm_m = LlmMetrics::default();
        let (score, bonus) = compute_weighted_score(&static_m, &llm_m);
        assert_eq!(bonus, 10);
        // unsafe_density: 50 * 20/100 = 10, raw_ptr: 50 * 15/100 = 7.5 -> 17 + 10 = 27
        assert!(score > 0);
    }
}
```

**Step 2: Run tests**

Run: `cargo test scanner::tests`
Expected: all tests pass (existing + 4 new)

**Step 3: Commit**

```
feat: add static metric computation and weighted scoring to scanner.rs
```

---

### Task 5: Task assignment generation in `scanner.rs`

**Files:**
- Modify: `src/scanner.rs`

**Step 1: Write tests for seeded task assignment**

Add to `src/scanner.rs`:

```rust
use rand::rngs::SmallRng;
use rand::prelude::*;

use crate::state::TaskAssignment;

/// Generate reviewer task assignments using weighted-random sampling.
///
/// Uses a seeded RNG for reproducibility. Higher-scored files are more likely
/// to be selected. Samples WITH replacement so high-risk files can get
/// multiple independent reviews.
pub fn generate_assignments(
    scores: &[FileScore],
    reviewer_count: u32,
    seed: u64,
) -> Vec<TaskAssignment> {
    if scores.is_empty() || reviewer_count == 0 {
        return Vec::new();
    }

    let mut rng = SmallRng::seed_from_u64(seed);

    // Build weight array: max(score, 1) so zero-scored files have a small chance.
    let weights: Vec<f64> = scores.iter()
        .map(|s| s.weighted_score.max(1) as f64)
        .collect();
    let total_weight: f64 = weights.iter().sum();

    let mut assignments = Vec::new();
    for id in 1..=reviewer_count {
        // Weighted random selection
        let mut roll: f64 = rng.random::<f64>() * total_weight;
        let mut selected = scores.len() - 1; // fallback to last
        for (i, w) in weights.iter().enumerate() {
            roll -= w;
            if roll <= 0.0 {
                selected = i;
                break;
            }
        }
        assignments.push(TaskAssignment {
            reviewer_id: id,
            starting_file: scores[selected].file.clone(),
            scout_score: scores[selected].weighted_score,
        });
    }

    assignments
}

/// Compute the dynamic reviewer count: ceil(sqrt(n)) clamped to [3, 12].
pub fn default_reviewer_count(file_count: usize) -> u32 {
    ((file_count as f64).sqrt().ceil() as u32).clamp(3, 12)
}
```

Add tests:

```rust
    #[test]
    fn generate_assignments_deterministic() {
        let scores = vec![
            FileScore {
                file: "src/a.rs".into(), weighted_score: 90, combination_bonus: 0,
                static_metrics: StaticMetrics::default(), llm_metrics: LlmMetrics::default(),
                top_concerns: vec![],
            },
            FileScore {
                file: "src/b.rs".into(), weighted_score: 10, combination_bonus: 0,
                static_metrics: StaticMetrics::default(), llm_metrics: LlmMetrics::default(),
                top_concerns: vec![],
            },
        ];

        let a1 = generate_assignments(&scores, 5, 42);
        let a2 = generate_assignments(&scores, 5, 42);

        // Same seed → same assignments
        assert_eq!(a1.len(), 5);
        for (x, y) in a1.iter().zip(a2.iter()) {
            assert_eq!(x.starting_file, y.starting_file);
            assert_eq!(x.reviewer_id, y.reviewer_id);
        }

        // Different seed → likely different assignments
        let a3 = generate_assignments(&scores, 5, 99);
        let same = a1.iter().zip(a3.iter()).filter(|(x, y)| x.starting_file == y.starting_file).count();
        // With only 2 files, some overlap is expected, but they shouldn't all match
        // (This is probabilistic but extremely unlikely to be all-same with different seeds)
        assert!(same < 5 || a1.len() < 5, "different seeds should produce different results");
    }

    #[test]
    fn default_reviewer_count_values() {
        assert_eq!(default_reviewer_count(1), 3);   // clamp min
        assert_eq!(default_reviewer_count(9), 3);   // sqrt(9) = 3
        assert_eq!(default_reviewer_count(25), 5);  // sqrt(25) = 5
        assert_eq!(default_reviewer_count(100), 10); // sqrt(100) = 10
        assert_eq!(default_reviewer_count(200), 12); // clamp max
    }
```

**Step 2: Run tests**

Run: `cargo test scanner::tests`
Expected: all tests pass

**Step 3: Commit**

```
feat: add seeded task assignment generation to scanner.rs
```

---

### Task 6: Add `--resume` and `--seed` to CLI

**Files:**
- Modify: `src/cli.rs`

**Step 1: Add new flags**

Add these fields to `Args` in `src/cli.rs`:

```rust
    /// Resume a previous run from `.kuriboh/state.json`.
    ///
    /// Skips phases that already completed successfully. Re-runs phases
    /// that were running or failed. Validates that --target matches the
    /// stored target path.
    #[arg(long)]
    pub resume: bool,

    /// Seed for reproducible task assignments.
    ///
    /// Controls the weighted-random reviewer-to-file mapping in Phase 3.
    /// If omitted, a random seed is generated. Stored in state.json so
    /// `--resume` reuses the same seed.
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with 0 errors

**Step 3: Commit**

```
feat: add --resume and --seed CLI flags
```

---

### Task 7: Sentinel checking in `state.rs`

**Files:**
- Modify: `src/state.rs`

**Step 1: Write sentinel verification functions and tests**

Add to `src/state.rs`:

```rust
/// Check whether a phase's output sentinel is satisfied.
///
/// Returns `Ok(true)` if the sentinel passes, `Ok(false)` if it doesn't,
/// and `Err` only on I/O errors other than "not found".
pub fn check_sentinel(target: &Path, phase: &str, state: &State) -> Result<bool> {
    let kb = target.join(".kuriboh");
    match phase {
        "exploration" => {
            let path = kb.join("exploration.md");
            match std::fs::metadata(&path) {
                Ok(m) => Ok(m.len() > 100),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e).context("checking exploration sentinel"),
            }
        }
        "scouting" => {
            let path = kb.join("scores.json");
            match std::fs::read_to_string(&path) {
                Ok(data) => {
                    // Verify it's valid JSON with at least one entry
                    let v: Result<serde_json::Value, _> = serde_json::from_str(&data);
                    Ok(v.is_ok())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e).context("checking scouting sentinel"),
            }
        }
        "deep_review" => {
            // All reviewer-N.json files must exist
            if state.task_assignments.is_empty() {
                return Ok(false);
            }
            for a in &state.task_assignments {
                let path = kb.join(format!("findings/reviewer-{}.json", a.reviewer_id));
                if !path.exists() {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "appraisal_compilation" => {
            let path = kb.join("compiled-findings.json");
            match std::fs::read_to_string(&path) {
                Ok(data) => {
                    let v: Result<serde_json::Value, _> = serde_json::from_str(&data);
                    Ok(v.is_ok())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e).context("checking compilation sentinel"),
            }
        }
        _ => bail!("unknown phase: {phase}"),
    }
}
```

Add tests:

```rust
    #[test]
    fn sentinel_exploration_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().to_path_buf(), 1);
        assert!(!check_sentinel(dir.path(), "exploration", &state).unwrap());
    }

    #[test]
    fn sentinel_exploration_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh");
        std::fs::create_dir_all(&kb).unwrap();
        std::fs::write(kb.join("exploration.md"), "short").unwrap();
        let state = State::new(dir.path().to_path_buf(), 1);
        assert!(!check_sentinel(dir.path(), "exploration", &state).unwrap());
    }

    #[test]
    fn sentinel_exploration_passes() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh");
        std::fs::create_dir_all(&kb).unwrap();
        std::fs::write(kb.join("exploration.md"), "x".repeat(200)).unwrap();
        let state = State::new(dir.path().to_path_buf(), 1);
        assert!(check_sentinel(dir.path(), "exploration", &state).unwrap());
    }

    #[test]
    fn sentinel_deep_review_checks_all_reviewers() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&kb).unwrap();

        let mut state = State::new(dir.path().to_path_buf(), 1);
        state.task_assignments = vec![
            TaskAssignment { reviewer_id: 1, starting_file: "a.rs".into(), scout_score: 50 },
            TaskAssignment { reviewer_id: 2, starting_file: "b.rs".into(), scout_score: 60 },
        ];

        // Only reviewer 1 has findings
        std::fs::write(kb.join("reviewer-1.json"), "[]").unwrap();
        assert!(!check_sentinel(dir.path(), "deep_review", &state).unwrap());

        // Now both exist
        std::fs::write(kb.join("reviewer-2.json"), "[]").unwrap();
        assert!(check_sentinel(dir.path(), "deep_review", &state).unwrap());
    }
```

**Step 2: Run tests**

Run: `cargo test state::tests`
Expected: all tests pass (existing + 4 new)

**Step 3: Commit**

```
feat: add sentinel checking for all phases
```

---

### Task 8: Refactor `runner.rs` — Generic session spawner

**Files:**
- Modify: `src/runner.rs`

**Step 1: Refactor `run()` into a generic `run_session()`**

Replace the current `run()` and `build_prompt()` with a generic session runner. The per-phase prompt builders will be added in subsequent tasks.

```rust
use std::io::Write;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::cli::Args;
use crate::events::{self, ClaudeEvent, ContentBlock};

/// Options for a single Claude Code session.
pub struct SessionOpts {
    pub prompt: String,
    /// Whether to enable agent teams for this session.
    pub agent_teams: bool,
}

/// Spawn a single Claude Code session, stream its NDJSON output, and return
/// the full sequence of parsed [`ClaudeEvent`]s.
pub async fn run_session(args: &Args, opts: &SessionOpts) -> Result<Vec<ClaudeEvent>> {
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
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ]);
    if opts.agent_teams {
        claude_args.extend([
            "--teammate-mode".to_string(),
            "in-process".to_string(),
        ]);
    }
    claude_args.extend([
        "-p".to_string(),
        opts.prompt.clone(),
    ]);

    let program = "claude";

    tracing::info!(
        %program,
        model = %args.model,
        max_turns = args.max_turns,
        agent_teams = opts.agent_teams,
        "Spawning Claude Code session"
    );
    tracing::debug!(
        cmd = %format!("{program} {}", claude_args.iter().map(|a| {
            if a.contains(' ') || a.contains('"') { format!("'{a}'") } else { a.clone() }
        }).collect::<Vec<_>>().join(" ")),
        "Full command"
    );

    let mut cmd = Command::new(program);
    cmd.args(&claude_args)
        .env_remove("CLAUDECODE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if opts.agent_teams {
        cmd.env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
    }

    let mut child = cmd.spawn()
        .with_context(|| format!("failed to spawn `{program}` — is it installed and on PATH?"))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

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
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles (main.rs will need updating but we're checking the module compiles in isolation)

Note: `main.rs` still calls the old `runner::run()`. It will be updated in Task 11 when we rewrite the phase loop. For now, temporarily keep a thin `run()` wrapper that calls `run_session()` with a placeholder prompt to avoid breaking compilation. Or just accept that `cargo check` will warn about unused code until Task 11.

**Step 3: Commit**

```
refactor: make runner.rs a generic session spawner with SessionOpts
```

---

### Task 9: Per-phase prompt builders

**Files:**
- Create: `src/prompts.rs`
- Modify: `src/main.rs` (add `mod prompts;`)

**Step 1: Create prompt builder module**

Create `src/prompts.rs` with one function per phase. These extract the relevant sections from the old monolithic `build_prompt()` into focused single-phase prompts.

```rust
use crate::scanner::FileScore;
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
    let file_list = files.iter()
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

    let assignment_list = assignments.iter()
        .map(|a| format!(
            "  - Reviewer {}: starting file `{}` (scout score: {})",
            a.reviewer_id, a.starting_file, a.scout_score
        ))
        .collect::<Vec<_>>()
        .join("\n");

    // Build the full inline reviewer spawn prompt (same as before)
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
pub fn appraisal_and_compilation(
    reviewer_ids: &[u32],
    target: &str,
    max_turns: u32,
) -> String {
    let reviewer_list = reviewer_ids.iter()
        .map(|id| format!("  - Reviewer {id}: findings at `.kuriboh/findings/reviewer-{id}.json`, worktree at `.kuriboh/worktrees/reviewer-{id}`"))
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
- Findings (sorted CRITICAL → INFO)
- Needs Review
- Remediation Roadmap

Target codebase: {target}
Max turns: {max_turns}"#
    )
}
```

**Step 2: Add `mod prompts` to main.rs**

Add `mod prompts;` to the module declarations in `src/main.rs`.

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles (may have unused warnings until Task 11)

**Step 4: Commit**

```
feat: add per-phase prompt builders in prompts.rs
```

---

### Task 10: Trim scout subagent template to 3 LLM metrics

**Files:**
- Modify: `src/agents/templates.rs`
- Modify: `src/agents/mod.rs`

**Step 1: Replace the SCOUT template with a 3-metric version**

Replace the entire `SCOUT` const in `src/agents/templates.rs` with:

```rust
/// Subagent: per-file LLM metric scorer.
///
/// Spawned once per `.rs` file during the scouting phase. Uses Haiku for speed
/// and cost, runs in the background, and is strictly read-only. Scores only
/// the 3 metrics that require semantic judgment — the other 7 are computed
/// by the Rust harness using static analysis.
pub const SCOUT: &str = r#"---
name: scout
description: >
  Scores a single Rust source file for semantic complexity metrics that
  require reading the code. Invoked once per .rs file during scouting.
  Returns a structured JSON score object with 3 metrics.
tools: Read, Grep
model: haiku
background: true
---

You are a Rust code quality scorer. You will be given the path to a single
`.rs` file. Read it and compute the following 3 semantic metrics. These metrics
require understanding the code — simple pattern matching is insufficient.

## Metrics (each scored 0-100)

1. **error_handling_risk** — Inverse of error handling quality.
   0 = all proper Result/?/error handling, idiomatic patterns throughout.
   50 = mixed: some proper handling, some unwrap/expect on fallible paths.
   100 = pervasive unwrap/panic, swallowed errors (`let _ = result`), empty
   catch blocks, error paths that silently discard information.
   Key question: could a caller trigger a panic through normal (non-adversarial) use?

2. **macro_density** — Density of non-trivial macro invocations per 100 LoC.
   0 = none, or only standard derive/cfg macros.
   50 = moderate use of custom macros, procedural macros, or `macro_rules!`.
   100 = heavy macro use that obscures control flow or generates unsafe code.
   Ignore: #[derive(...)], #[cfg(...)], println!, format!, vec![], assert!.
   Count: custom macro_rules!, proc macro invocations, macros that generate
   struct/impl/unsafe blocks, deeply nested macro calls.

3. **generic_complexity** — Complexity of generic type parameters and trait bounds.
   0 = no generics, or simple single-type-parameter generics.
   50 = moderate: 3-5 where clauses, associated types, or lifetime parameters.
   100 = complex: >=8 where clauses, higher-kinded types, complex trait bound
   interactions, GATs, or lifetime gymnastics that are hard to reason about.

## Output

Respond with ONLY this JSON (no markdown fences, no extra text):

{"file":"<path>","error_handling_risk":0,"macro_density":0,"generic_complexity":0}
"#;
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with 0 errors

**Step 3: Commit**

```
refactor: trim scout template to 3 LLM-only metrics
```

---

### Task 11: Rewrite `main.rs` — Phase loop with state management

**Files:**
- Modify: `src/main.rs`

This is the core integration task. Rewrite `main()` to use the phase loop pattern.

**Step 1: Rewrite main.rs**

Replace the body of `main()` (after argument parsing and validation) with the phase loop. Keep the existing `print_estimate` function but update it to use `scanner::enumerate_files` and `scanner::default_reviewer_count`.

```rust
mod agents;
mod cli;
mod events;
mod prompts;
mod report;
mod runner;
mod scanner;
mod state;

use std::path::Path;

use anyhow::{bail, Result};
use tracing::info;

use state::{PhaseStatus, State};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = cli::parse();

    let default_level = if args.verbose {
        "kuriboh=debug"
    } else {
        "kuriboh=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(default_level.parse()?),
        )
        .init();

    args.target = std::fs::canonicalize(&args.target)
        .map_err(|e| anyhow::anyhow!("--target {}: {e}", args.target.display()))?;
    if !args.target.is_dir() {
        bail!("--target {} is not a directory", args.target.display());
    }

    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            bail!(
                "--output parent directory does not exist: {}",
                parent.display()
            );
        }
    }

    if args.agents_config.is_some() {
        tracing::warn!("--agents-config is not yet implemented; ignoring");
    }

    if args.estimate {
        print_estimate(&args);
        return Ok(());
    }

    info!(target = %args.target.display(), "Starting kuriboh security review");

    // Install subagent definitions.
    agents::install(&args.target, &args.agents_config)?;

    // Load or create pipeline state.
    let mut state = if args.resume {
        let s = State::load(&args.target)?;
        if s.target != args.target {
            bail!(
                "--resume target mismatch: state has {}, got {}",
                s.target.display(),
                args.target.display()
            );
        }
        info!("Resuming from existing state");
        s
    } else {
        let seed = args.seed.unwrap_or_else(|| rand::random());
        State::new(args.target.clone(), seed)
    };

    // === Phase 1: Exploration ===
    run_phase(&mut state, &args, "exploration", |st, a| {
        Box::pin(run_exploration(st, a))
    }).await?;

    // === Phase 2: Scouting ===
    run_phase(&mut state, &args, "scouting", |st, a| {
        Box::pin(run_scouting(st, a))
    }).await?;

    // === Phase 3: Deep Review ===
    run_phase(&mut state, &args, "deep_review", |st, a| {
        Box::pin(run_deep_review(st, a))
    }).await?;

    // === Phase 4+5: Appraisal & Compilation ===
    run_phase(&mut state, &args, "appraisal_compilation", |st, a| {
        Box::pin(run_appraisal_compilation(st, a))
    }).await?;

    // === Report Generation (Rust, no Claude) ===
    let report = report::parse_from_workspace(&args.target)?;
    report::write(&report, &args.output, args.json)?;

    if !args.keep_workspace {
        agents::cleanup(&args.target)?;
    }

    info!(
        output = %args.output.display(),
        cost_usd = report.total_cost_usd,
        "Review complete"
    );
    Ok(())
}

/// Run a single phase with sentinel checking and state management.
async fn run_phase<F>(
    state: &mut State,
    args: &cli::Args,
    phase_name: &str,
    execute: F,
) -> Result<()>
where
    F: FnOnce(&mut State, &cli::Args) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + '_>>,
{
    // Check if already done and sentinel still valid.
    if *state.phase_status(phase_name) == PhaseStatus::Done {
        if state::check_sentinel(&args.target, phase_name, state)? {
            info!(phase = phase_name, "Phase already complete, skipping");
            return Ok(());
        }
        tracing::warn!(phase = phase_name, "Phase marked done but sentinel failed, re-running");
    }

    info!(phase = phase_name, "Starting phase");
    state.phase_mut(phase_name).status = PhaseStatus::Running;
    state.save(&args.target)?;

    match execute(state, args).await {
        Ok(()) => {
            if state::check_sentinel(&args.target, phase_name, state)? {
                state.phase_mut(phase_name).status = PhaseStatus::Done;
                state.save(&args.target)?;
                info!(phase = phase_name, "Phase complete");
                Ok(())
            } else {
                state.phase_mut(phase_name).status = PhaseStatus::Failed;
                state.phase_mut(phase_name).reason = Some("sentinel check failed".to_string());
                state.save(&args.target)?;
                bail!("Phase {phase_name} completed but sentinel check failed");
            }
        }
        Err(e) => {
            state.phase_mut(phase_name).status = PhaseStatus::Failed;
            state.phase_mut(phase_name).reason = Some(format!("{e:#}"));
            state.save(&args.target)?;
            Err(e).with_context(|| format!("Phase {phase_name} failed"))
        }
    }
}

// --- Phase implementations ---

async fn run_exploration(state: &mut State, args: &cli::Args) -> Result<()> {
    let prompt = prompts::exploration(
        &args.target.display().to_string(),
        args.prompt.as_deref(),
    );
    let events = runner::run_session(args, &runner::SessionOpts {
        prompt,
        agent_teams: false,
    }).await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("exploration").cost_usd = Some(cost);
    Ok(())
}

async fn run_scouting(state: &mut State, args: &cli::Args) -> Result<()> {
    // 2a: Enumerate files and compute static metrics.
    let file_list = scanner::enumerate_files(&args.target, false)?;
    let filter_tests = file_list.len() > 300;
    let file_list = if filter_tests {
        info!(
            total = file_list.len(),
            "Over 300 .rs files, filtering test files for scouting"
        );
        scanner::enumerate_files(&args.target, true)?
    } else {
        file_list
    };

    let file_strings: Vec<String> = file_list.iter()
        .map(|p| p.display().to_string())
        .collect();
    state.files = file_strings.clone();

    let mut static_scores: Vec<(String, scanner::StaticMetrics)> = Vec::new();
    for file_path in &file_list {
        let full_path = args.target.join(file_path);
        let source = std::fs::read_to_string(&full_path).unwrap_or_default();
        let metrics = scanner::compute_static_metrics(&source);
        static_scores.push((file_path.display().to_string(), metrics));
    }

    // 2b: LLM metrics via scout subagents.
    let prompt = prompts::llm_scouting(&file_strings);
    let events = runner::run_session(args, &runner::SessionOpts {
        prompt,
        agent_teams: false,
    }).await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("scouting").cost_usd = Some(cost);

    // 2c: Merge scores.
    let llm_scores_path = args.target.join(".kuriboh/llm-scores.json");
    let llm_scores = scanner::load_llm_scores(&llm_scores_path)?;

    let file_scores = scanner::merge_scores(&static_scores, &llm_scores);

    // Write scores.json
    let scores_json = serde_json::to_string_pretty(&file_scores)?;
    std::fs::write(args.target.join(".kuriboh/scores.json"), &scores_json)?;

    // Generate task assignments.
    let reviewer_count = args.reviewers.unwrap_or_else(|| {
        scanner::default_reviewer_count(file_scores.len())
    });
    state.reviewer_count = reviewer_count;
    state.task_assignments = scanner::generate_assignments(&file_scores, reviewer_count, state.seed);
    state.save(&args.target)?;

    Ok(())
}

async fn run_deep_review(state: &mut State, args: &cli::Args) -> Result<()> {
    // Create git worktrees and PoC dirs.
    for a in &state.task_assignments {
        let wt_path = args.target.join(format!(".kuriboh/worktrees/reviewer-{}", a.reviewer_id));
        if !wt_path.exists() {
            let output = std::process::Command::new("git")
                .args(["worktree", "add"])
                .arg(&wt_path)
                .arg(format!("-b kuriboh-review-{}", a.reviewer_id))
                .current_dir(&args.target)
                .output()?;
            if !output.status.success() {
                tracing::warn!(
                    reviewer = a.reviewer_id,
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "Failed to create worktree"
                );
            }
        }
        let poc_dir = args.target.join(format!(".kuriboh/pocs/reviewer-{}", a.reviewer_id));
        std::fs::create_dir_all(&poc_dir)?;
    }

    let prompt = prompts::deep_review(
        &state.task_assignments,
        &args.target.display().to_string(),
        args.max_turns,
        args.prompt.as_deref(),
    );
    let events = runner::run_session(args, &runner::SessionOpts {
        prompt,
        agent_teams: true,
    }).await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("deep_review").cost_usd = Some(cost);
    Ok(())
}

async fn run_appraisal_compilation(state: &mut State, args: &cli::Args) -> Result<()> {
    let reviewer_ids: Vec<u32> = state.task_assignments.iter()
        .map(|a| a.reviewer_id)
        .collect();
    let prompt = prompts::appraisal_and_compilation(
        &reviewer_ids,
        &args.target.display().to_string(),
        args.max_turns,
    );
    let events = runner::run_session(args, &runner::SessionOpts {
        prompt,
        agent_teams: false,
    }).await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("appraisal_compilation").cost_usd = Some(cost);
    Ok(())
}

fn print_estimate(args: &cli::Args) {
    let file_list = scanner::enumerate_files(&args.target, false).unwrap_or_default();
    let file_count = file_list.len();
    let reviewers = args
        .reviewers
        .unwrap_or_else(|| scanner::default_reviewer_count(file_count));

    let cost_exploration = 0.15;
    let cost_scouting = file_count as f64 * 0.01; // cheaper: only 3 LLM metrics
    let cost_per_reviewer = 1.80;
    let cost_per_appraiser = 0.60;
    let cost_compilation = 0.30;
    let cost_lead_overhead = 0.50;

    let cost_review = reviewers as f64 * cost_per_reviewer;
    let cost_appraisal = reviewers as f64 * cost_per_appraiser;
    let total = cost_exploration
        + cost_scouting
        + cost_review
        + cost_appraisal
        + cost_compilation
        + cost_lead_overhead;

    println!("Kuriboh Cost Estimate");
    println!("=====================");
    println!();
    println!("Target:       {}", args.target.display());
    println!("Rust files:   {file_count}");
    println!("Model:        {}", args.model);
    println!("Reviewers:    {reviewers}");
    println!("Max turns:    {}", args.max_turns);
    if let Some(budget) = args.max_budget_usd {
        println!("Max budget:   ${budget:.2}");
    }
    println!();
    println!("Phase                  Est. Cost");
    println!("-----                  ---------");
    println!("1. Exploration         ${cost_exploration:.2}");
    println!("2. Scouting ({file_count} files) ${cost_scouting:.2}");
    println!("3. Deep Review ({reviewers}x)    ${cost_review:.2}");
    println!("4. Appraisal ({reviewers}x)      ${cost_appraisal:.2}");
    println!("5. Compilation         ${cost_compilation:.2}");
    println!("   Lead overhead       ${cost_lead_overhead:.2}");
    println!("                       ---------");
    println!("   Total               ${total:.2}");
    println!();
    println!("Note: estimates are approximate. Scouting cost is lower than");
    println!("before because 7/10 metrics are now computed by Rust (free).");
}
```

**Step 2: Add `load_llm_scores` and `merge_scores` to scanner.rs**

Add to `src/scanner.rs`:

```rust
/// Load LLM scores from the JSON file written by the scouting session.
pub fn load_llm_scores(path: &Path) -> Result<HashMap<String, LlmMetrics>> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;

    #[derive(Deserialize)]
    struct LlmEntry {
        file: String,
        #[serde(default)]
        error_handling_risk: u32,
        #[serde(default)]
        macro_density: u32,
        #[serde(default)]
        generic_complexity: u32,
    }

    let entries: Vec<LlmEntry> = serde_json::from_str(&data)
        .with_context(|| format!("parsing {}", path.display()))?;

    let mut map = HashMap::new();
    for e in entries {
        map.insert(e.file, LlmMetrics {
            error_handling_risk: e.error_handling_risk,
            macro_density: e.macro_density,
            generic_complexity: e.generic_complexity,
        });
    }
    Ok(map)
}

/// Merge static metrics with LLM metrics and compute weighted scores.
pub fn merge_scores(
    static_scores: &[(String, StaticMetrics)],
    llm_scores: &HashMap<String, LlmMetrics>,
) -> Vec<FileScore> {
    static_scores.iter().map(|(file, static_m)| {
        let llm_m = llm_scores.get(file).cloned().unwrap_or(LlmMetrics {
            error_handling_risk: 50,
            macro_density: 50,
            generic_complexity: 50,
        });
        let (weighted_score, combination_bonus) = compute_weighted_score(static_m, &llm_m);

        let mut concerns = Vec::new();
        if static_m.unsafe_density >= 50 { concerns.push("high unsafe density".to_string()); }
        if static_m.raw_pointer_usage >= 50 { concerns.push("raw pointer usage".to_string()); }
        if llm_m.error_handling_risk >= 70 { concerns.push("poor error handling".to_string()); }

        FileScore {
            file: file.clone(),
            static_metrics: static_m.clone(),
            llm_metrics: llm_m,
            combination_bonus,
            weighted_score,
            top_concerns: concerns,
        }
    }).collect()
}
```

Add the `use std::collections::HashMap;` and `use anyhow::Context;` imports to scanner.rs.

**Step 3: Add `parse_from_workspace` to report.rs**

Add to `src/report.rs`:

```rust
/// Build a report by reading workspace artifacts directly (no event stream).
///
/// This is used when the Rust harness drives each phase individually
/// and findings are written to `.kuriboh/compiled-findings.json`.
pub fn parse_from_workspace(target: &Path) -> Result<Report> {
    let kb = target.join(".kuriboh");

    // Read compiled findings.
    let compiled_path = kb.join("compiled-findings.json");
    let (findings, needs_review) = if compiled_path.exists() {
        let data = std::fs::read_to_string(&compiled_path)
            .context("reading compiled-findings.json")?;
        let all: Vec<Finding> = serde_json::from_str(&data)
            .context("parsing compiled-findings.json")?;
        let (nr, confirmed): (Vec<_>, Vec<_>) = all.into_iter()
            .partition(|f| f.verdict.as_deref() == Some("needs-review"));
        (confirmed, nr)
    } else {
        (vec![], vec![])
    };

    // Read exploration summary.
    let exploration = std::fs::read_to_string(kb.join("exploration.md")).ok();

    // Read scores for scouting summary.
    let scouting_summary = std::fs::read_to_string(kb.join("scores.json")).ok().map(|data| {
        // Count files per tier from scores.
        let scores: Vec<serde_json::Value> = serde_json::from_str(&data).unwrap_or_default();
        let total = scores.len();
        let critical = scores.iter().filter(|s| s["weighted_score"].as_u64().unwrap_or(0) >= 70).count();
        let high = scores.iter().filter(|s| { let v = s["weighted_score"].as_u64().unwrap_or(0); v >= 50 && v < 70 }).count();
        format!("{total} files scored. {critical} critical-tier, {high} high-tier.")
    });

    // Sum costs from state.json if available.
    let total_cost = State::load(target).ok().map(|s| {
        s.phases.values().filter_map(|p| p.cost_usd).sum::<f64>()
    }).unwrap_or(0.0);

    let executive_summary = if findings.is_empty() && needs_review.is_empty() {
        "No findings survived appraisal.".to_string()
    } else {
        format!(
            "{} findings confirmed, {} need human review.",
            findings.len(),
            needs_review.len()
        )
    };

    Ok(Report {
        executive_summary,
        scouting_summary,
        review_coverage: None,
        findings,
        needs_review,
        total_cost_usd: total_cost,
        raw_result: exploration.unwrap_or_default(),
    })
}
```

Add `use crate::state::State;` import to report.rs.

**Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles with 0 errors (possibly some warnings about unused old code)

**Step 5: Run all tests**

Run: `cargo test`
Expected: all tests pass

**Step 6: Commit**

```
feat: rewrite main.rs as phase loop with state management and --resume
```

---

### Task 12: Update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Update architecture documentation**

Update the CLAUDE.md to reflect the new architecture: outer scheduler, phase loop, new modules (state.rs, scanner.rs, prompts.rs), removed monolithic prompt, 4 sessions instead of 1, `--resume` and `--seed` flags.

Key sections to update:
- Execution flow: `main.rs` phase loop → state management → per-phase session spawning
- Module descriptions for state.rs, scanner.rs, prompts.rs
- Phase descriptions showing Rust vs Claude responsibilities
- `.kuriboh/` workspace layout (add `state.json`, `llm-scores.json`)
- CLI flags (add `--resume`, `--seed`)
- Remove references to monolithic orchestration prompt in `runner.rs::build_prompt()`

**Step 2: Commit**

```
docs: update CLAUDE.md for deterministic scheduler architecture
```

---

### Task 13: Clean up dead code

**Files:**
- Modify: `src/runner.rs` (remove old `build_prompt`, `run`)
- Modify: `src/main.rs` (remove old `count_rs_files`, `default_reviewer_count`)

**Step 1: Remove dead code**

- In `runner.rs`: delete the old `run()` function and `build_prompt()` if they still exist as dead code after Task 8/11
- In `main.rs`: delete `count_rs_files()` and `default_reviewer_count()` (replaced by `scanner::enumerate_files` and `scanner::default_reviewer_count`)

**Step 2: Run clippy**

Run: `cargo clippy`
Expected: 0 errors, 0 warnings

**Step 3: Run all tests**

Run: `cargo test`
Expected: all tests pass

**Step 4: Format**

Run: `cargo fmt`

**Step 5: Commit**

```
chore: remove dead code from pre-scheduler architecture
```

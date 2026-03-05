use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::prelude::*;
use rand::rngs::SmallRng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::state::{ReviewerLens, TaskAssignment};

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

    let loc_count = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("//") && !t.starts_with("/*") && !t.starts_with('*')
        })
        .count();

    let unsafe_count = lines
        .iter()
        .filter(|l| l.contains("unsafe ") || l.contains("unsafe{"))
        .count();
    let unwrap_count = lines
        .iter()
        .filter(|l| l.contains(".unwrap()") || l.contains(".expect("))
        .count();
    let raw_ptr_count = lines
        .iter()
        .filter(|l| l.contains("*mut ") || l.contains("*const "))
        .count();
    let ffi_count = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("extern ") || t.contains("extern \"C\"") || t.contains("extern \"c\"")
        })
        .count();
    let todo_count = lines
        .iter()
        .filter(|l| {
            let upper = l.to_uppercase();
            upper.contains("TODO") || upper.contains("FIXME") || upper.contains("HACK")
        })
        .count();

    let mut max_depth: i32 = 0;
    let mut depth: i32 = 0;
    for line in &lines {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    max_depth = max_depth.max(depth);
                }
                '}' => {
                    depth -= 1;
                }
                _ => {}
            }
        }
    }

    StaticMetrics {
        loc: scale_loc(loc_count),
        unsafe_density: scale_density(unsafe_count, loc_count, 3),
        unwrap_density: scale_density(unwrap_count, loc_count, 5),
        raw_pointer_usage: scale_density(raw_ptr_count, loc_count, 3),
        ffi_declarations: scale_ffi(ffi_count),
        todo_fixme_hack: scale_todo(todo_count),
        max_nesting_depth: scale_nesting(max_depth as u32),
    }
}

fn scale_loc(loc: usize) -> u32 {
    if loc < 50 {
        0
    } else if loc >= 500 {
        100
    } else {
        ((loc as f64 - 50.0) / (500.0 - 50.0) * 100.0) as u32
    }
}

fn scale_density(count: usize, loc: usize, high_per_100: usize) -> u32 {
    if loc == 0 || count == 0 {
        return 0;
    }
    let per_100 = count as f64 / (loc as f64 / 100.0);
    if per_100 >= high_per_100 as f64 {
        100
    } else {
        ((per_100 / high_per_100 as f64) * 100.0).min(100.0) as u32
    }
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

/// Compute the weighted score from static + LLM metrics.
pub fn compute_weighted_score(static_m: &StaticMetrics, llm_m: &LlmMetrics) -> (u32, u32) {
    let weighted_sum: f64 = [
        (static_m.unsafe_density, 20),
        (static_m.raw_pointer_usage, 15),
        (static_m.unwrap_density, 10),
        (llm_m.error_handling_risk, 10),
        (static_m.ffi_declarations, 10),
        (static_m.loc, 5),
        (static_m.max_nesting_depth, 5),
        (static_m.todo_fixme_hack, 5),
        (llm_m.macro_density, 5),
        (llm_m.generic_complexity, 5),
    ]
    .iter()
    .map(|(score, weight)| f64::from(*score) * f64::from(*weight) / 100.0)
    .sum();

    let combo_bonus = if static_m.unsafe_density > 0 && static_m.raw_pointer_usage > 0 {
        10
    } else {
        0
    };
    let total = (weighted_sum as u32 + combo_bonus).clamp(0, 100);

    (total, combo_bonus)
}

/// Load LLM scores from the JSON file written by the scouting session.
pub fn load_llm_scores(path: &Path) -> Result<HashMap<String, LlmMetrics>> {
    let data =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

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

    let entries: Vec<LlmEntry> =
        serde_json::from_str(&data).with_context(|| format!("parsing {}", path.display()))?;

    let mut map = HashMap::new();
    for e in entries {
        map.insert(
            e.file,
            LlmMetrics {
                error_handling_risk: e.error_handling_risk,
                macro_density: e.macro_density,
                generic_complexity: e.generic_complexity,
            },
        );
    }
    Ok(map)
}

/// Merge static metrics with LLM metrics and compute weighted scores.
pub fn merge_scores(
    static_scores: &[(String, StaticMetrics)],
    llm_scores: &HashMap<String, LlmMetrics>,
) -> Vec<FileScore> {
    static_scores
        .par_iter()
        .map(|(file, static_m)| {
            let llm_m = llm_scores.get(file).cloned().unwrap_or(LlmMetrics {
                error_handling_risk: 50,
                macro_density: 50,
                generic_complexity: 50,
            });
            let (weighted_score, combination_bonus) = compute_weighted_score(static_m, &llm_m);

            let mut concerns = Vec::new();
            if static_m.unsafe_density >= 50 {
                concerns.push("high unsafe density".to_string());
            }
            if static_m.raw_pointer_usage >= 50 {
                concerns.push("raw pointer usage".to_string());
            }
            if llm_m.error_handling_risk >= 70 {
                concerns.push("poor error handling".to_string());
            }

            FileScore {
                file: file.clone(),
                static_metrics: static_m.clone(),
                llm_metrics: llm_m,
                combination_bonus,
                weighted_score,
                top_concerns: concerns,
            }
        })
        .collect()
}

/// Returns indices of files that must have a dedicated reviewer, sorted by score descending.
///
/// A file is mandatory if any of:
/// - Its name is `main.rs` or `lib.rs` (entry point at any depth)
/// - Its `weighted_score >= 70` (critical threshold)
/// - It has both `unsafe_density > 0` AND `ffi_declarations > 0`
pub fn classify_mandatory_files(scores: &[FileScore]) -> Vec<usize> {
    let mut indices: Vec<usize> = scores
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            let fname = std::path::Path::new(&s.file)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            let is_entry = fname == "main.rs" || fname == "lib.rs";
            let is_critical = s.weighted_score >= 70;
            let is_unsafe_ffi =
                s.static_metrics.unsafe_density > 0 && s.static_metrics.ffi_declarations > 0;
            is_entry || is_critical || is_unsafe_ffi
        })
        .map(|(i, _)| i)
        .collect();
    indices.sort_by(|a, b| scores[*b].weighted_score.cmp(&scores[*a].weighted_score));
    indices
}

/// Compute how many reserve slots to allocate: `ceil(reviewer_count / 3)` clamped to [1, 4].
pub fn compute_reserve_count(reviewer_count: u32) -> u32 {
    let raw = reviewer_count.div_ceil(3);
    raw.clamp(1, 4)
}

/// Weighted random selection excluding indices in `exclude`. Falls back to uniform if all
/// non-excluded weights are zero.
fn weighted_select(
    scores: &[FileScore],
    exclude: &std::collections::HashSet<usize>,
    rng: &mut SmallRng,
) -> usize {
    let weights: Vec<f64> = scores
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if exclude.contains(&i) {
                0.0
            } else {
                f64::from(s.weighted_score.max(1))
            }
        })
        .collect();
    let total: f64 = weights.iter().sum();

    if total <= 0.0 {
        // All excluded or zero — uniform over non-excluded
        let available: Vec<usize> = (0..scores.len()).filter(|i| !exclude.contains(i)).collect();
        if available.is_empty() {
            // Truly exhausted — pick uniformly from all
            return rng.random_range(0..scores.len());
        }
        return available[rng.random_range(0..available.len())];
    }

    let mut roll: f64 = rng.random::<f64>() * total;
    for (i, w) in weights.iter().enumerate() {
        roll -= w;
        if roll <= 0.0 {
            return i;
        }
    }
    scores.len() - 1
}

/// Generate reviewer task assignments using a 3-stage scheduler.
///
/// Returns `(assignments, reserve_count)`.
///
/// **Stage 1 — Coverage floor**: mandatory files get dedicated reviewers first.
/// **Stage 2 — Remaining slots**: filled via weighted sampling WITHOUT replacement.
/// **Stage 3 — Reserve slots**: extra assignments for adaptive allocation by the lead.
///
/// A monotonic lens counter cycles `MemorySafety → Parsing → Filesystem → Concurrency → Crypto`
/// across all stages, deterministic for a given seed.
pub fn generate_assignments(
    scores: &[FileScore],
    reviewer_count: u32,
    seed: u64,
) -> (Vec<TaskAssignment>, u32) {
    if scores.is_empty() || reviewer_count == 0 {
        return (Vec::new(), 0);
    }

    let mut rng = SmallRng::seed_from_u64(seed);
    let mut assignments = Vec::new();
    let mut lens_counter: usize = 0;
    let mut next_id: u32 = 1;

    let next_lens = |counter: &mut usize| -> ReviewerLens {
        let lens = ReviewerLens::ALL[*counter % ReviewerLens::ALL.len()].clone();
        *counter += 1;
        lens
    };

    // Stage 1: Coverage floor — mandatory files
    let mandatory_indices = classify_mandatory_files(scores);
    let mandatory_slots = (reviewer_count as usize).min(mandatory_indices.len());
    let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for &idx in mandatory_indices.iter().take(mandatory_slots) {
        assignments.push(TaskAssignment {
            reviewer_id: next_id,
            starting_file: scores[idx].file.clone(),
            scout_score: scores[idx].weighted_score,
            lens: Some(next_lens(&mut lens_counter)),
            mandatory: true,
            reserve: false,
        });
        used.insert(idx);
        next_id += 1;
    }

    // Stage 2: Fill remaining primary slots via weighted sampling WITHOUT replacement
    let remaining_primary = reviewer_count.saturating_sub(mandatory_slots as u32);
    for _ in 0..remaining_primary {
        if used.len() >= scores.len() {
            // All files assigned — reset exclusion to allow repeats
            used.clear();
        }
        let idx = weighted_select(scores, &used, &mut rng);
        assignments.push(TaskAssignment {
            reviewer_id: next_id,
            starting_file: scores[idx].file.clone(),
            scout_score: scores[idx].weighted_score,
            lens: Some(next_lens(&mut lens_counter)),
            mandatory: false,
            reserve: false,
        });
        used.insert(idx);
        next_id += 1;
    }

    // Stage 3: Reserve slots
    let reserve_count = compute_reserve_count(reviewer_count);
    // Reset exclusion for reserve — target highest remaining
    let mut reserve_used: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for _ in 0..reserve_count {
        if reserve_used.len() >= scores.len() {
            reserve_used.clear();
        }
        let idx = weighted_select(scores, &reserve_used, &mut rng);
        assignments.push(TaskAssignment {
            reviewer_id: next_id,
            starting_file: scores[idx].file.clone(),
            scout_score: scores[idx].weighted_score,
            lens: Some(next_lens(&mut lens_counter)),
            mandatory: false,
            reserve: true,
        });
        reserve_used.insert(idx);
        next_id += 1;
    }

    (assignments, reserve_count)
}

/// Compute the dynamic reviewer count: ceil(sqrt(n)) clamped to [3, 12].
pub fn default_reviewer_count(file_count: usize) -> u32 {
    ((file_count as f64).sqrt().ceil() as u32).clamp(3, 12)
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    #[expect(clippy::wildcard_imports)]
    use super::*;
    use std::fs;

    fn setup_tree(dir: &Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn hello() {}").unwrap();
        fs::write(dir.join("src/foo_test.rs"), "// test").unwrap();
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/integration.rs"), "// int test").unwrap();
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
        assert!(!names.contains(&"src/foo_test.rs"));
        assert!(!names.iter().any(|n| n.contains("tests/")));
    }

    #[test]
    fn static_metrics_basic() {
        // Source must have >= 50 non-blank/non-comment lines for scale_loc > 0.
        let mut source = String::from(
            r#"
fn main() {
    let x = foo().unwrap();
    let y = bar().expect("oops");
}

unsafe fn danger() {
    let p: *mut u8 = std::ptr::null_mut();
}

// TODO: fix this
"#,
        );
        // Pad to exceed the 50-line LOC threshold.
        for i in 0..50 {
            source.push_str(&format!("fn stub_{i}() {{}}\n"));
        }
        let m = compute_static_metrics(&source);
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
        assert!(score > 0);
    }

    /// Helper to build a `FileScore` with just a name and score.
    fn file_score(name: &str, score: u32) -> FileScore {
        FileScore {
            file: name.into(),
            weighted_score: score,
            combination_bonus: 0,
            static_metrics: StaticMetrics::default(),
            llm_metrics: LlmMetrics::default(),
            top_concerns: vec![],
        }
    }

    /// Helper to build a `FileScore` with custom static metrics.
    fn file_score_with_metrics(name: &str, score: u32, metrics: StaticMetrics) -> FileScore {
        FileScore {
            file: name.into(),
            weighted_score: score,
            combination_bonus: 0,
            static_metrics: metrics,
            llm_metrics: LlmMetrics::default(),
            top_concerns: vec![],
        }
    }

    #[test]
    fn generate_assignments_deterministic() {
        let scores = vec![file_score("src/a.rs", 90), file_score("src/b.rs", 10)];

        let (a1, r1) = generate_assignments(&scores, 5, 42);
        let (a2, r2) = generate_assignments(&scores, 5, 42);

        assert_eq!(r1, r2);
        // 5 primary + reserve_count reserve slots
        assert_eq!(a1.len(), a2.len());
        for (x, y) in a1.iter().zip(a2.iter()) {
            assert_eq!(x.starting_file, y.starting_file);
            assert_eq!(x.reviewer_id, y.reviewer_id);
            assert_eq!(x.lens, y.lens);
            assert_eq!(x.mandatory, y.mandatory);
            assert_eq!(x.reserve, y.reserve);
        }
    }

    #[test]
    fn coverage_floor_entry_points() {
        let scores = vec![
            file_score("src/main.rs", 40),
            file_score("src/lib.rs", 30),
            file_score("src/foo.rs", 20),
        ];
        let mandatory = classify_mandatory_files(&scores);
        assert!(mandatory.contains(&0), "main.rs should be mandatory");
        assert!(mandatory.contains(&1), "lib.rs should be mandatory");
        assert!(!mandatory.contains(&2), "foo.rs should not be mandatory");
    }

    #[test]
    fn coverage_floor_critical_threshold() {
        let scores = vec![
            file_score("src/danger.rs", 75),
            file_score("src/safe.rs", 69),
        ];
        let mandatory = classify_mandatory_files(&scores);
        assert!(mandatory.contains(&0), "score >= 70 should be mandatory");
        assert!(
            !mandatory.contains(&1),
            "score < 70 should not be mandatory"
        );
    }

    #[test]
    fn coverage_floor_unsafe_ffi() {
        let metrics = StaticMetrics {
            unsafe_density: 10,
            ffi_declarations: 50,
            ..Default::default()
        };
        let scores = vec![
            file_score_with_metrics("src/ffi_bridge.rs", 40, metrics),
            file_score("src/pure.rs", 40),
        ];
        let mandatory = classify_mandatory_files(&scores);
        assert!(
            mandatory.contains(&0),
            "unsafe+FFI file should be mandatory"
        );
        assert!(!mandatory.contains(&1), "pure file should not be mandatory");
    }

    #[test]
    fn coverage_floor_truncates_when_exceeds_reviewers() {
        let scores = vec![
            file_score("src/main.rs", 90),
            file_score("src/lib.rs", 80),
            file_score("crate2/src/main.rs", 75),
            file_score("crate2/src/lib.rs", 70),
            file_score("crate3/src/main.rs", 60),
        ];
        // 5 mandatory files (all are entry points or >= 70), but only 3 reviewers
        let (assignments, _) = generate_assignments(&scores, 3, 42);
        let mandatory_count = assignments.iter().filter(|a| a.mandatory).count();
        assert_eq!(mandatory_count, 3, "should truncate to reviewer_count");
        // They should be the top 3 by score
        let mandatory_files: Vec<&str> = assignments
            .iter()
            .filter(|a| a.mandatory)
            .map(|a| a.starting_file.as_str())
            .collect();
        assert!(mandatory_files.contains(&"src/main.rs"));
        assert!(mandatory_files.contains(&"src/lib.rs"));
        assert!(mandatory_files.contains(&"crate2/src/main.rs"));
    }

    #[test]
    fn lens_distribution_round_robin() {
        use crate::state::ReviewerLens;

        let scores = vec![
            file_score("src/a.rs", 90),
            file_score("src/b.rs", 80),
            file_score("src/c.rs", 70),
            file_score("src/d.rs", 60),
            file_score("src/e.rs", 50),
            file_score("src/f.rs", 40),
        ];
        let (assignments, _) = generate_assignments(&scores, 5, 42);
        let lenses: Vec<&ReviewerLens> = assignments
            .iter()
            .map(|a| a.lens.as_ref().unwrap())
            .collect();

        // First 5 lenses should cycle through ALL
        let expected = ReviewerLens::ALL;
        for (i, lens) in lenses.iter().take(5).enumerate() {
            assert_eq!(*lens, &expected[i % expected.len()], "lens at position {i}");
        }
    }

    #[test]
    fn reserve_count_values() {
        assert_eq!(compute_reserve_count(1), 1);
        assert_eq!(compute_reserve_count(2), 1);
        assert_eq!(compute_reserve_count(3), 1);
        assert_eq!(compute_reserve_count(4), 2);
        assert_eq!(compute_reserve_count(6), 2);
        assert_eq!(compute_reserve_count(9), 3);
        assert_eq!(compute_reserve_count(12), 4);
        assert_eq!(compute_reserve_count(15), 4); // clamped to 4
    }

    #[test]
    fn reserve_slots_are_marked() {
        let scores = vec![
            file_score("src/a.rs", 90),
            file_score("src/b.rs", 50),
            file_score("src/c.rs", 30),
        ];
        let (assignments, reserve_count) = generate_assignments(&scores, 3, 42);
        let actual_reserves = assignments.iter().filter(|a| a.reserve).count();
        assert_eq!(
            actual_reserves, reserve_count as usize,
            "reserve count should match"
        );
        let primaries = assignments.iter().filter(|a| !a.reserve).count();
        assert_eq!(primaries, 3, "primary count should match reviewer_count");
    }

    #[test]
    fn default_reviewer_count_values() {
        assert_eq!(default_reviewer_count(1), 3);
        assert_eq!(default_reviewer_count(9), 3);
        assert_eq!(default_reviewer_count(25), 5);
        assert_eq!(default_reviewer_count(100), 10);
        assert_eq!(default_reviewer_count(200), 12);
    }

    #[test]
    fn load_llm_scores_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm-scores.json");
        fs::write(
            &path,
            r#"[
                {"file": "src/main.rs", "error_handling_risk": 30, "macro_density": 10, "generic_complexity": 5},
                {"file": "src/lib.rs", "error_handling_risk": 70}
            ]"#,
        )
        .unwrap();

        let scores = load_llm_scores(&path).unwrap();
        assert_eq!(scores.len(), 2);

        let main_score = scores.get("src/main.rs").unwrap();
        assert_eq!(main_score.error_handling_risk, 30);
        assert_eq!(main_score.macro_density, 10);
        assert_eq!(main_score.generic_complexity, 5);

        // Missing fields default to 0
        let lib_score = scores.get("src/lib.rs").unwrap();
        assert_eq!(lib_score.error_handling_risk, 70);
        assert_eq!(lib_score.macro_density, 0);
        assert_eq!(lib_score.generic_complexity, 0);
    }

    #[test]
    fn load_llm_scores_empty_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm-scores.json");
        fs::write(&path, "[]").unwrap();

        let scores = load_llm_scores(&path).unwrap();
        assert!(scores.is_empty());
    }

    #[test]
    fn load_llm_scores_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert!(load_llm_scores(&path).is_err());
    }

    #[test]
    fn merge_scores_with_llm_data() {
        let static_scores = vec![
            (
                "src/a.rs".to_string(),
                StaticMetrics {
                    unsafe_density: 60,
                    raw_pointer_usage: 55,
                    ..Default::default()
                },
            ),
            ("src/b.rs".to_string(), StaticMetrics::default()),
        ];
        let mut llm = HashMap::new();
        llm.insert(
            "src/a.rs".to_string(),
            LlmMetrics {
                error_handling_risk: 80,
                macro_density: 20,
                generic_complexity: 10,
            },
        );

        let scores = merge_scores(&static_scores, &llm);
        assert_eq!(scores.len(), 2);

        // File with LLM data uses actual values
        assert_eq!(scores[0].file, "src/a.rs");
        assert_eq!(scores[0].llm_metrics.error_handling_risk, 80);
        assert!(scores[0]
            .top_concerns
            .contains(&"high unsafe density".to_string()));
        assert!(scores[0]
            .top_concerns
            .contains(&"raw pointer usage".to_string()));
        assert!(scores[0]
            .top_concerns
            .contains(&"poor error handling".to_string()));

        // File without LLM data gets defaults of 50
        assert_eq!(scores[1].file, "src/b.rs");
        assert_eq!(scores[1].llm_metrics.error_handling_risk, 50);
        assert_eq!(scores[1].llm_metrics.macro_density, 50);
        assert_eq!(scores[1].llm_metrics.generic_complexity, 50);
        assert!(scores[1].top_concerns.is_empty());
    }

    #[test]
    fn merge_scores_combo_bonus() {
        let static_scores = vec![(
            "src/ffi.rs".to_string(),
            StaticMetrics {
                unsafe_density: 50,
                raw_pointer_usage: 50,
                ..Default::default()
            },
        )];
        let scores = merge_scores(&static_scores, &HashMap::new());
        assert_eq!(scores[0].combination_bonus, 10);
        assert!(scores[0].weighted_score > 0);
    }
}

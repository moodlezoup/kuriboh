use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::rngs::SmallRng;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::state::TaskAssignment;

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
        let score = metrics
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        weighted_sum += score as f64 * *weight as f64 / 100.0;
    }

    let combo_bonus =
        if static_m.unsafe_density > 0 && static_m.raw_pointer_usage > 0 {
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
        .iter()
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

    let weights: Vec<f64> = scores.iter().map(|s| s.weighted_score.max(1) as f64).collect();
    let total_weight: f64 = weights.iter().sum();

    let mut assignments = Vec::new();
    for id in 1..=reviewer_count {
        let mut roll: f64 = rng.random::<f64>() * total_weight;
        let mut selected = scores.len() - 1;
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

#[cfg(test)]
mod tests {
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
        assert!(score > 0);
    }

    #[test]
    fn generate_assignments_deterministic() {
        let scores = vec![
            FileScore {
                file: "src/a.rs".into(),
                weighted_score: 90,
                combination_bonus: 0,
                static_metrics: StaticMetrics::default(),
                llm_metrics: LlmMetrics::default(),
                top_concerns: vec![],
            },
            FileScore {
                file: "src/b.rs".into(),
                weighted_score: 10,
                combination_bonus: 0,
                static_metrics: StaticMetrics::default(),
                llm_metrics: LlmMetrics::default(),
                top_concerns: vec![],
            },
        ];

        let a1 = generate_assignments(&scores, 5, 42);
        let a2 = generate_assignments(&scores, 5, 42);

        assert_eq!(a1.len(), 5);
        for (x, y) in a1.iter().zip(a2.iter()) {
            assert_eq!(x.starting_file, y.starting_file);
            assert_eq!(x.reviewer_id, y.reviewer_id);
        }
    }

    #[test]
    fn default_reviewer_count_values() {
        assert_eq!(default_reviewer_count(1), 3);
        assert_eq!(default_reviewer_count(9), 3);
        assert_eq!(default_reviewer_count(25), 5);
        assert_eq!(default_reviewer_count(100), 10);
        assert_eq!(default_reviewer_count(200), 12);
    }
}

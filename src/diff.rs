use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::state::{DiffFile, FileStatus};

/// Directories to skip, matching scanner.rs SKIP_DIRS.
const SKIP_DIRS: &[&str] = &["target", "vendor", ".git", ".kuriboh", ".claude"];

/// Parsed diff context for a commit range.
pub struct DiffContext {
    pub base: String,
    pub head: String,
    pub files: Vec<DiffFile>,
    pub hunks: HashMap<String, String>,
}

/// Parse a git range string like "main..feature" into (base, head).
fn parse_range(range: &str) -> Result<(String, String)> {
    if range.contains("...") {
        bail!("Three-dot syntax is not supported. Use two-dot syntax: base..head");
    }
    let parts: Vec<&str> = range.splitn(2, "..").collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        bail!("Invalid range format. Expected base..head (e.g. main..feature)");
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Check if a path should be skipped based on directory exclusions.
fn should_skip(path: &str) -> bool {
    path.split('/')
        .any(|component| SKIP_DIRS.contains(&component))
}

/// Parse `git diff --name-status` output into DiffFiles.
fn parse_name_status(output: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let (status_str, path_part) = (parts[0], parts[1]);

        // Handle rename: R100\told_path\tnew_path
        if status_str.starts_with('R') {
            let paths: Vec<&str> = path_part.splitn(2, '\t').collect();
            if paths.len() == 2 {
                let new_path = paths[1].to_string();
                if !new_path.ends_with(".rs") || should_skip(&new_path) {
                    continue;
                }
                files.push(DiffFile {
                    path: new_path,
                    status: FileStatus::Renamed {
                        from: paths[0].to_string(),
                    },
                });
            }
            continue;
        }

        let path = path_part.to_string();
        if !path.ends_with(".rs") || should_skip(&path) {
            continue;
        }

        let status = match status_str {
            "A" => FileStatus::Added,
            "M" => FileStatus::Modified,
            "D" => FileStatus::Deleted,
            _ => continue,
        };
        files.push(DiffFile { path, status });
    }
    files
}

/// Resolve a git diff range into a DiffContext.
///
/// Runs git commands against the target repository to extract changed files
/// and unified diff hunks for `.rs` files.
pub fn resolve_diff(target: &Path, range: &str) -> Result<DiffContext> {
    let (base, head) = parse_range(range)?;

    // Get changed files with status.
    let output = std::process::Command::new("git")
        .args(["diff", "--name-status", &format!("{base}..{head}")])
        .current_dir(target)
        .output()
        .context("failed to run git diff --name-status")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff --name-status failed: {stderr}");
    }
    let name_status = String::from_utf8_lossy(&output.stdout);
    let mut files = parse_name_status(&name_status);

    // Remove deleted files (nothing to review).
    files.retain(|f| !matches!(f.status, FileStatus::Deleted));

    if files.is_empty() {
        bail!("No .rs files changed between {base}..{head}");
    }

    // Get unified diff hunks for the remaining .rs files.
    let rs_paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut cmd = std::process::Command::new("git");
    cmd.args(["diff", &format!("{base}..{head}"), "--"]);
    for p in &rs_paths {
        cmd.arg(p);
    }
    let diff_output = cmd
        .current_dir(target)
        .output()
        .context("failed to run git diff for hunks")?;
    let full_diff = String::from_utf8_lossy(&diff_output.stdout).to_string();

    // Split the full diff into per-file hunks.
    let hunks = split_diff_by_file(&full_diff);

    Ok(DiffContext {
        base,
        head,
        files,
        hunks,
    })
}

/// Parse a `--pr` value into a PR number.
///
/// Accepts either a bare number (`123`) or a GitHub PR URL
/// (`https://github.com/owner/repo/pull/123`).
fn parse_pr_input(input: &str) -> Result<u32> {
    // Bare number?
    if let Ok(n) = input.parse::<u32>() {
        return Ok(n);
    }
    // GitHub URL — extract the number after /pull/
    if let Some(rest) = input.rsplit_once("/pull/") {
        let number_str = rest.1.trim_end_matches('/');
        if let Ok(n) = number_str.parse::<u32>() {
            return Ok(n);
        }
    }
    bail!("Invalid --pr value: expected a PR number or GitHub URL (e.g. 123 or https://github.com/owner/repo/pull/123)");
}

/// Resolve a GitHub PR into a `DiffContext` using the `gh` CLI.
///
/// Fetches the PR's base branch and head SHA via `gh pr view`, then delegates
/// to [`resolve_diff`] with `baseRefName..headRefSha`.
pub fn resolve_pr(target: &Path, pr_input: &str) -> Result<DiffContext> {
    let pr_number = parse_pr_input(pr_input)?;

    // Use gh to get base branch and head SHA.
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "baseRefName,headRefOid",
            "--jq",
            ".baseRefName + \"..\" + .headRefOid",
        ])
        .current_dir(target)
        .output()
        .context("failed to run `gh pr view` — is the GitHub CLI (`gh`) installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh pr view failed for PR #{pr_number}: {stderr}");
    }

    let range = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if range.is_empty() || !range.contains("..") {
        bail!("Unexpected output from `gh pr view` for PR #{pr_number}: {range:?}");
    }

    resolve_diff(target, &range)
}

/// Split a unified diff into per-file sections.
fn split_diff_by_file(diff: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let mut current_file: Option<String> = None;
    let mut current_hunk = String::new();

    for line in diff.lines() {
        if line.starts_with("diff --git") {
            // Save previous file's hunk.
            if let Some(file) = current_file.take() {
                if !current_hunk.is_empty() {
                    result.insert(file, current_hunk.clone());
                }
            }
            current_hunk.clear();

            // Extract file path from "diff --git a/path b/path".
            // Use rsplit_once to handle paths that might contain " b/".
            if let Some((_, b_path)) = line.rsplit_once(" b/") {
                current_file = Some(b_path.to_string());
            }
        }
        current_hunk.push_str(line);
        current_hunk.push('\n');
    }
    // Save last file.
    if let Some(file) = current_file {
        if !current_hunk.is_empty() {
            result.insert(file, current_hunk);
        }
    }
    result
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_valid() {
        let (base, head) = parse_range("main..feature").unwrap();
        assert_eq!(base, "main");
        assert_eq!(head, "feature");
    }

    #[test]
    fn parse_range_sha() {
        let (base, head) = parse_range("abc123..def456").unwrap();
        assert_eq!(base, "abc123");
        assert_eq!(head, "def456");
    }

    #[test]
    fn parse_range_three_dot_rejected() {
        let err = parse_range("main...feature").unwrap_err();
        assert!(err.to_string().contains("Three-dot syntax"));
    }

    #[test]
    fn parse_range_no_dots() {
        assert!(parse_range("main").is_err());
    }

    #[test]
    fn parse_range_empty_parts() {
        assert!(parse_range("..feature").is_err());
        assert!(parse_range("main..").is_err());
    }

    #[test]
    fn parse_name_status_basic() {
        let output = "A\tsrc/new.rs\nM\tsrc/lib.rs\nD\tsrc/old.rs\n";
        let files = parse_name_status(output);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[1].path, "src/lib.rs");
        assert_eq!(files[1].status, FileStatus::Modified);
        assert_eq!(files[2].path, "src/old.rs");
        assert_eq!(files[2].status, FileStatus::Deleted);
    }

    #[test]
    fn parse_name_status_filters_non_rs() {
        let output = "A\tsrc/new.rs\nM\tREADME.md\nA\tsrc/foo.py\n";
        let files = parse_name_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new.rs");
    }

    #[test]
    fn parse_name_status_skips_excluded_dirs() {
        let output = "A\ttarget/debug/build.rs\nM\tsrc/lib.rs\nA\tvendor/crate/lib.rs\n";
        let files = parse_name_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
    }

    #[test]
    fn parse_name_status_rename() {
        let output = "R100\tsrc/old.rs\tsrc/new.rs\n";
        let files = parse_name_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(
            files[0].status,
            FileStatus::Renamed {
                from: "src/old.rs".to_string()
            }
        );
    }

    #[test]
    fn split_diff_by_file_basic() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn main() {
+    println!(\"hello\");
 }
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1 +1 @@
-fn old() {}
+fn new() {}
";
        let hunks = split_diff_by_file(diff);
        assert_eq!(hunks.len(), 2);
        assert!(hunks.contains_key("src/a.rs"));
        assert!(hunks.contains_key("src/b.rs"));
        assert!(hunks["src/a.rs"].contains("println"));
        assert!(hunks["src/b.rs"].contains("fn new"));
    }

    #[test]
    fn should_skip_dirs() {
        assert!(should_skip("target/debug/build.rs"));
        assert!(should_skip("vendor/crate/lib.rs"));
        assert!(should_skip(".git/hooks.rs"));
        assert!(should_skip(".kuriboh/state.rs"));
        assert!(!should_skip("src/main.rs"));
        assert!(!should_skip("crates/core/src/lib.rs"));
    }

    #[test]
    fn parse_pr_input_number() {
        assert_eq!(parse_pr_input("123").unwrap(), 123);
        assert_eq!(parse_pr_input("1").unwrap(), 1);
    }

    #[test]
    fn parse_pr_input_github_url() {
        assert_eq!(
            parse_pr_input("https://github.com/owner/repo/pull/456").unwrap(),
            456
        );
    }

    #[test]
    fn parse_pr_input_github_url_trailing_slash() {
        assert_eq!(
            parse_pr_input("https://github.com/owner/repo/pull/789/").unwrap(),
            789
        );
    }

    #[test]
    fn parse_pr_input_invalid() {
        assert!(parse_pr_input("not-a-number").is_err());
        assert!(parse_pr_input("https://github.com/owner/repo/issues/123").is_err());
        assert!(parse_pr_input("").is_err());
    }
}

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::State;

/// Severity of a single security finding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// A single security finding extracted from agent output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub file: Option<String>,
    pub description: String,
    pub recommendation: String,
    /// Which subagent reported this finding.
    #[serde(default)]
    pub source_agent: Option<String>,
    /// Scout-phase weighted score for this file (0-100), if available.
    #[serde(default)]
    pub scout_score: Option<u32>,
    /// The call chain from entry point to vulnerability site.
    #[serde(default)]
    pub call_chain: Vec<String>,
    /// How attacker-controlled input reaches the vulnerable sink.
    #[serde(default)]
    pub reachability: Option<String>,
    /// Exact file:line + short code snippet as evidence (obtained via rg -n or Read).
    #[serde(default)]
    pub evidence: Option<String>,
    /// Minimal exploit conditions — not just "might be exploitable".
    #[serde(default)]
    pub exploit_sketch: Option<String>,
    /// Reproduction status: not_tried | partial | working | not_reproducible
    #[serde(default)]
    pub repro_status: Option<String>,
    /// Whether a proof-of-concept was provided by the reviewer.
    #[serde(default)]
    pub poc_available: bool,
    /// Whether the appraiser validated the PoC successfully.
    #[serde(default)]
    pub poc_validated: Option<bool>,
    /// Path to the PoC file, if any.
    #[serde(default)]
    pub poc_path: Option<String>,
    /// Original severity before any appraiser adjustment.
    #[serde(default)]
    pub original_severity: Option<Severity>,
    /// Appraisal verdict: "confirmed", "adjusted", "rejected", or "needs-review".
    #[serde(default)]
    pub verdict: Option<String>,
    /// Notes from the appraiser about severity adjustments or validation.
    #[serde(default)]
    pub appraiser_notes: Option<String>,
    /// Number of independent reviewers who found this same issue.
    #[serde(default)]
    pub independent_reviewers: Option<u32>,
}

/// The full structured security report.
#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub executive_summary: String,
    /// Summary of the scouting phase (file counts per tier, top-risk patterns).
    #[serde(default)]
    pub scouting_summary: Option<String>,
    /// Summary of review coverage (reviewers, files reviewed, tier coverage).
    #[serde(default)]
    pub review_coverage: Option<String>,
    /// Diff mode summary: "Changes: base..head (N files)"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<String>,
    pub findings: Vec<Finding>,
    /// Findings that the appraiser flagged as needing human review.
    #[serde(default)]
    pub needs_review: Vec<Finding>,
    /// Token cost across all agents and teammates in the run.
    pub total_cost_usd: f64,
    /// The raw synthesized text from the lead agent's final result event.
    pub raw_result: String,
}

/// Write the report to `path`.
///
/// Format is inferred from the file extension: `.json` → JSON, anything else
/// → Markdown. The `--json` flag in [`crate::cli::Args`] overrides the output
/// path extension.
pub fn write(report: &Report, path: &Path, force_json: bool) -> Result<()> {
    let is_json = force_json || path.extension().and_then(|e| e.to_str()) == Some("json");

    let content = if is_json {
        serde_json::to_string_pretty(report).context("serializing report to JSON")?
    } else {
        render_markdown(report)
    };

    std::fs::write(path, content)
        .with_context(|| format!("writing report to {}", path.display()))?;

    Ok(())
}

fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("# Kuriboh Security Review Report\n\n");

    if let Some(diff) = &report.diff_summary {
        out.push_str(&format!("**{diff}**\n\n"));
    }

    out.push_str("## Executive Summary\n\n");
    out.push_str(&report.executive_summary);
    out.push('\n');

    if let Some(scouting) = &report.scouting_summary {
        out.push_str("\n## Scouting Overview\n\n");
        out.push_str(scouting);
        out.push('\n');
    }

    if let Some(coverage) = &report.review_coverage {
        out.push_str("\n## Review Coverage\n\n");
        out.push_str(coverage);
        out.push('\n');
    }

    if report.findings.is_empty() && report.needs_review.is_empty() {
        // Structured finding extraction is not yet implemented, so raw_result
        // contains the full compiled report from the lead agent. Append only
        // the sections NOT already extracted above (skip everything up to and
        // including the "Review Coverage" section to avoid duplication).
        let remainder = skip_extracted_sections(&report.raw_result);
        if !remainder.is_empty() {
            out.push('\n');
            out.push_str(&remainder);
        }
    } else {
        if !report.findings.is_empty() {
            out.push_str("\n## Findings\n\n");
            for f in &report.findings {
                render_finding(&mut out, f);
            }
        }

        if !report.needs_review.is_empty() {
            out.push_str("\n## Needs Review\n\n");
            out.push_str("*These findings require human judgment to confirm or dismiss.*\n\n");
            for f in &report.needs_review {
                render_finding(&mut out, f);
            }
        }
    }

    out.push_str(&format!(
        "\n---\n*Total cost: ${:.4}*\n",
        report.total_cost_usd
    ));
    out
}

fn render_finding(out: &mut String, f: &Finding) {
    out.push_str(&format!("### [{:?}] {}\n", f.severity, f.title));
    if let Some(file) = &f.file {
        out.push_str(&format!("- **File**: `{file}`\n"));
    }
    if let Some(score) = f.scout_score {
        out.push_str(&format!("- **Scout Score**: {score}\n"));
    }
    if !f.call_chain.is_empty() {
        out.push_str(&format!(
            "- **Call Chain**: {}\n",
            f.call_chain.join(" -> ")
        ));
    }
    out.push_str(&format!("- **Description**: {}\n", f.description));
    if let Some(r) = &f.reachability {
        out.push_str(&format!("- **Reachability**: {r}\n"));
    }
    if let Some(e) = &f.evidence {
        out.push_str(&format!("- **Evidence**: {e}\n"));
    }
    if let Some(s) = &f.exploit_sketch {
        out.push_str(&format!("- **Exploit Sketch**: {s}\n"));
    }
    if let Some(r) = &f.repro_status {
        out.push_str(&format!("- **Repro Status**: {r}\n"));
    }
    out.push_str(&format!("- **Recommendation**: {}\n", f.recommendation));
    if f.poc_available {
        let status = match f.poc_validated {
            Some(true) => "validated",
            Some(false) => "available (not validated)",
            None => "available",
        };
        out.push_str(&format!("- **PoC**: {status}"));
        if let Some(path) = &f.poc_path {
            out.push_str(&format!(" (`{path}`)"));
        }
        out.push('\n');
    }
    if f.verdict.as_deref() == Some("adjusted") {
        if let Some(orig) = &f.original_severity {
            out.push_str(&format!("- **Original Severity**: {orig:?}\n"));
        }
    }
    if let Some(n) = f.independent_reviewers {
        if n > 1 {
            out.push_str(&format!("- **Independent Reviewers**: {n}\n"));
        }
    }
    if let Some(notes) = &f.appraiser_notes {
        out.push_str(&format!("- **Appraiser Notes**: {notes}\n"));
    }
    out.push('\n');
}

/// Return reviewer IDs that have non-empty findings files.
pub fn reviewers_with_findings(target: &Path, reviewer_ids: &[u32]) -> Vec<u32> {
    reviewer_ids
        .iter()
        .copied()
        .filter(|id| {
            let path = target.join(format!(".kuriboh/findings/reviewer-{id}.json"));
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|data| serde_json::from_str::<Vec<Finding>>(&data).ok())
                .is_some_and(|findings| !findings.is_empty())
        })
        .collect()
}

/// Collect all findings from reviewer files into a single flat list for
/// semantic dedup. Returns the JSON string to send to the LLM and the
/// parsed findings with their source reviewer IDs.
pub fn collect_all_findings(target: &Path) -> Result<(String, Vec<(u32, Finding)>)> {
    let findings_dir = target.join(".kuriboh/findings");
    if !findings_dir.exists() {
        return Ok((String::from("[]"), Vec::new()));
    }

    let mut all: Vec<(u32, Finding)> = Vec::new();

    for entry in std::fs::read_dir(&findings_dir)
        .with_context(|| format!("reading {}", findings_dir.display()))?
        .flatten()
    {
        let path = entry.path();
        let fname = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let id = match fname
            .strip_prefix("reviewer-")
            .and_then(|s| s.strip_suffix(".json"))
        {
            Some(s) => match s.parse::<u32>() {
                Ok(id) => id,
                Err(_) => continue,
            },
            None => continue,
        };

        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let findings: Vec<Finding> = match serde_json::from_str(&data) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for f in findings {
            all.push((id, f));
        }
    }

    // Build a compact JSON for the LLM: just index, file, title, severity.
    let summaries: Vec<serde_json::Value> = all
        .iter()
        .enumerate()
        .map(|(i, (_rid, f))| {
            serde_json::json!({
                "index": i,
                "file": f.file,
                "title": f.title,
                "severity": f.severity,
            })
        })
        .collect();
    let json = serde_json::to_string_pretty(&summaries)?;

    Ok((json, all))
}

/// Apply LLM-identified duplicate groups to a flat findings list.
///
/// `dedup_response` is the raw LLM output: a JSON array of arrays of indices,
/// e.g. `[[0, 3], [1, 5]]`. For each group, keeps the best finding (highest
/// severity, longest description) and records independent reviewer count.
/// Returns the deduplicated list.
pub fn apply_dedup_groups(
    mut all: Vec<(u32, Finding)>,
    dedup_response: &str,
) -> Vec<(u32, Finding)> {
    // Parse duplicate groups from LLM response.
    let groups: Vec<Vec<usize>> = extract_json_array(dedup_response).unwrap_or_default();

    // Mark indices to remove (all but the winner in each group).
    let mut remove_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for group in &groups {
        if group.len() < 2 {
            continue;
        }
        // Find the best finding in this group.
        let mut best_idx = group[0];
        for &idx in &group[1..] {
            if idx >= all.len() || best_idx >= all.len() {
                continue;
            }
            let best = &all[best_idx].1;
            let candidate = &all[idx].1;
            if candidate.severity < best.severity
                || (candidate.severity == best.severity
                    && candidate.description.len() > best.description.len())
            {
                best_idx = idx;
            }
        }

        // Count unique reviewers in the group.
        let mut reviewer_ids: Vec<u32> = group
            .iter()
            .filter_map(|&idx| {
                if idx < all.len() {
                    Some(all[idx].0)
                } else {
                    None
                }
            })
            .collect();
        reviewer_ids.sort_unstable();
        reviewer_ids.dedup();
        let reviewer_count = reviewer_ids.len() as u32;

        if best_idx < all.len() {
            let current = all[best_idx].1.independent_reviewers.unwrap_or(1);
            if reviewer_count > current {
                all[best_idx].1.independent_reviewers = Some(reviewer_count);
            }
        }

        // Mark all others for removal.
        for &idx in group {
            if idx != best_idx && idx < all.len() {
                remove_indices.insert(idx);
            }
        }
    }

    // Remove duplicates (iterate in reverse to preserve indices).
    let mut sorted_removes: Vec<usize> = remove_indices.into_iter().collect();
    sorted_removes.sort_unstable_by(|a, b| b.cmp(a));
    for idx in sorted_removes {
        if idx < all.len() {
            all.remove(idx);
        }
    }

    all
}

/// Extract a JSON array from an LLM response that may contain markdown fences
/// or surrounding text.
fn extract_json_array(response: &str) -> Option<Vec<Vec<usize>>> {
    let trimmed = response.trim();
    // Try direct parse first.
    if let Ok(v) = serde_json::from_str(trimmed) {
        return Some(v);
    }
    // Try stripping markdown code fences.
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();
    serde_json::from_str(stripped).ok()
}

/// Write deduplicated findings back to per-reviewer files. Used after semantic
/// dedup to update the files before appraisal.
pub fn write_deduped_findings(target: &Path, all: &[(u32, Finding)]) -> Result<()> {
    let findings_dir = target.join(".kuriboh/findings");

    // Group by reviewer.
    let mut per_reviewer: std::collections::HashMap<u32, Vec<&Finding>> =
        std::collections::HashMap::new();
    for (rid, finding) in all {
        per_reviewer.entry(*rid).or_default().push(finding);
    }

    // Write each reviewer's file.
    for (id, findings) in &per_reviewer {
        let path = findings_dir.join(format!("reviewer-{id}.json"));
        let json = serde_json::to_string_pretty(findings)
            .with_context(|| format!("serializing deduped findings for reviewer {id}"))?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    }

    // Write empty arrays for reviewers whose findings were all deduped away.
    for entry in std::fs::read_dir(&findings_dir)?.flatten() {
        let fname = entry.file_name().to_str().unwrap_or_default().to_string();
        if let Some(id_str) = fname
            .strip_prefix("reviewer-")
            .and_then(|s| s.strip_suffix(".json"))
        {
            if let Ok(id) = id_str.parse::<u32>() {
                if !per_reviewer.contains_key(&id) {
                    std::fs::write(entry.path(), "[]")?;
                }
            }
        }
    }

    Ok(())
}

/// Compile appraised findings into `compiled-findings.json`.
///
/// Reads all `appraised-*.json` files, filters by verdict, sorts by severity
/// then scout_score, and writes the result. This replaces the LLM-based Phase 5.
/// Compile appraised findings into `compiled-findings.json`.
///
/// Reads all `appraised-*.json` files, filters by verdict, sorts by severity
/// then scout_score, and writes the result.
///
/// In diff mode, `diff_files` contains the set of changed file paths. Findings
/// in files outside the diff are kept only if severity is HIGH or CRITICAL;
/// INFO, LOW, and MEDIUM findings outside the diff are filtered out.
pub fn compile_findings(target: &Path, diff_files: Option<&[String]>) -> Result<usize> {
    let findings_dir = target.join(".kuriboh/findings");
    let mut all_findings: Vec<Finding> = Vec::new();

    if findings_dir.exists() {
        for entry in std::fs::read_dir(&findings_dir)?.flatten() {
            let fname = entry.file_name().to_str().unwrap_or_default().to_string();
            if !fname.starts_with("appraised-") || !fname.ends_with(".json") {
                continue;
            }
            let Ok(data) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(findings) = serde_json::from_str::<Vec<Finding>>(&data) else {
                continue;
            };
            for f in findings {
                // Discard rejected findings.
                if f.verdict.as_deref() == Some("rejected") {
                    continue;
                }
                // In diff mode, filter low-severity findings for files outside the diff.
                if let Some(changed) = diff_files {
                    if !is_in_diff(&f, changed)
                        && matches!(
                            f.severity,
                            Severity::Info | Severity::Low | Severity::Medium
                        )
                    {
                        continue;
                    }
                }
                all_findings.push(f);
            }
        }
    }

    // Sort: severity ascending (Critical < High < ... < Info, thanks to derived Ord),
    // then scout_score descending within same severity.
    all_findings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| b.scout_score.unwrap_or(0).cmp(&a.scout_score.unwrap_or(0)))
    });

    let count = all_findings.len();
    let json = serde_json::to_string_pretty(&all_findings)?;
    std::fs::write(target.join(".kuriboh/compiled-findings.json"), json)?;

    Ok(count)
}

/// Check whether a finding's file is in the set of diff-changed files.
/// Strips `:line` suffixes from the finding's file path before comparing.
fn is_in_diff(finding: &Finding, diff_files: &[String]) -> bool {
    let file = match finding.file.as_deref() {
        Some(f) => f,
        None => return false,
    };
    // Strip `:line` suffix.
    let path = file.split(':').next().unwrap_or(file);
    diff_files.iter().any(|d| d == path)
}

/// Build a report by reading workspace artifacts directly (no event stream).
///
/// This is used when the Rust harness drives each phase individually
/// and findings are written to `.kuriboh/compiled-findings.json`.
pub fn parse_from_workspace(target: &Path) -> Result<Report> {
    let kb = target.join(".kuriboh");

    // Read compiled findings.
    let compiled_path = kb.join("compiled-findings.json");
    let (findings, needs_review) = if compiled_path.exists() {
        let data =
            std::fs::read_to_string(&compiled_path).context("reading compiled-findings.json")?;
        let all: Vec<Finding> =
            serde_json::from_str(&data).context("parsing compiled-findings.json")?;
        let (nr, confirmed): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|f| f.verdict.as_deref() == Some("needs-review"));
        (confirmed, nr)
    } else {
        (vec![], vec![])
    };

    // Read exploration summary.
    let exploration = std::fs::read_to_string(kb.join("exploration.md")).ok();

    // Read scores for scouting summary.
    let scouting_summary = std::fs::read_to_string(kb.join("scores.json"))
        .ok()
        .map(|data| {
            let scores: Vec<serde_json::Value> = serde_json::from_str(&data).unwrap_or_default();
            let total = scores.len();
            let critical = scores
                .iter()
                .filter(|s| s["weighted_score"].as_u64().unwrap_or(0) >= 70)
                .count();
            let high = scores
                .iter()
                .filter(|s| {
                    let v = s["weighted_score"].as_u64().unwrap_or(0);
                    (50..70).contains(&v)
                })
                .count();
            format!("{total} files scored. {critical} critical-tier, {high} high-tier.")
        });

    // Load state once for cost + diff summary.
    let loaded_state = State::load(target).ok();

    let total_cost = loaded_state.as_ref().map_or(0.0, |s| {
        s.phases.values().filter_map(|p| p.cost_usd).sum::<f64>()
    });

    let diff_summary = loaded_state.as_ref().and_then(|s| {
        match &s.mode {
            crate::state::ReviewMode::Diff { base, head, changed_files } => {
                // Resolve short SHAs for auditability (symbolic refs are mutable).
                let resolve_sha = |ref_name: &str| -> String {
                    std::process::Command::new("git")
                        .args(["rev-parse", "--short", ref_name])
                        .current_dir(target)
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                        .map_or_else(
                            || ref_name.to_string(),
                            |sha| sha.trim().to_string(),
                        )
                };
                let base_sha = resolve_sha(base);
                let head_sha = resolve_sha(head);
                Some(format!(
                    "Review of changes: `{base}..{head}` ({base_sha}..{head_sha}, {} files changed)",
                    changed_files.len()
                ))
            }
            crate::state::ReviewMode::Full => None,
        }
    });

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
        diff_summary,
        findings,
        needs_review,
        total_cost_usd: total_cost,
        raw_result: exploration.unwrap_or_default(),
    })
}

/// Skip sections already extracted (Executive Summary, Scouting Overview,
/// Review Coverage) from the raw result to avoid duplication in the report.
///
/// Returns everything from the first heading that is NOT one of the
/// already-extracted sections.
fn skip_extracted_sections(raw: &str) -> String {
    static EXTRACTED: &[&str] = &["executive summary", "scouting overview", "review coverage"];

    let mut lines: Vec<&str> = Vec::new();
    let mut skipping = true;

    for line in raw.lines() {
        if line.starts_with('#') {
            let heading = line.trim_start_matches('#').trim();
            if EXTRACTED.iter().any(|h| heading.eq_ignore_ascii_case(h)) {
                skipping = true;
                continue;
            }
            // First non-extracted heading — start keeping lines.
            skipping = false;
        }
        if !skipping {
            lines.push(line);
        }
    }

    lines.join("\n").trim().to_owned()
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn skip_extracted_sections_empty_input() {
        assert_eq!(skip_extracted_sections(""), "");
    }

    #[test]
    fn skip_extracted_sections_all_extracted() {
        let raw = "## Executive Summary\nSome summary.\n## Scouting Overview\nScores here.";
        assert_eq!(skip_extracted_sections(raw), "");
    }

    #[test]
    fn skip_extracted_sections_keeps_non_extracted() {
        let raw = "\
## Executive Summary
Some summary.
## Findings
### [Critical] Buffer overflow
- File: src/foo.rs:42
## Remediation Roadmap
Fix the bugs.";
        let result = skip_extracted_sections(raw);
        assert!(result.starts_with("## Findings"));
        assert!(result.contains("Buffer overflow"));
        assert!(result.contains("## Remediation Roadmap"));
        assert!(!result.contains("Executive Summary"));
    }

    #[test]
    fn skip_extracted_sections_case_insensitive() {
        let raw = "## EXECUTIVE SUMMARY\nblah\n## Findings\nreal stuff";
        let result = skip_extracted_sections(raw);
        assert!(result.starts_with("## Findings"));
        assert!(!result.contains("EXECUTIVE SUMMARY"));
    }

    #[test]
    fn skip_extracted_sections_interleaved() {
        let raw = "\
## Findings
Finding 1.
## Review Coverage
Coverage details.
## Needs Review
Needs review stuff.";
        let result = skip_extracted_sections(raw);
        assert!(result.contains("## Findings"));
        assert!(result.contains("Finding 1."));
        assert!(!result.contains("Review Coverage"));
        assert!(result.contains("## Needs Review"));
    }

    #[test]
    fn parse_from_workspace_empty() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh");
        std::fs::create_dir_all(&kb).unwrap();

        let report = parse_from_workspace(dir.path()).unwrap();
        assert!(report.findings.is_empty());
        assert!(report.needs_review.is_empty());
        assert_eq!(report.executive_summary, "No findings survived appraisal.");
    }

    #[test]
    fn parse_from_workspace_with_findings() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh");
        std::fs::create_dir_all(&kb).unwrap();

        let findings = serde_json::json!([
            {
                "severity": "HIGH",
                "title": "Buffer overflow",
                "description": "Writes past buffer end",
                "recommendation": "Add bounds check",
                "verdict": "confirmed"
            },
            {
                "severity": "MEDIUM",
                "title": "Unchecked input",
                "description": "No validation",
                "recommendation": "Validate",
                "verdict": "needs-review"
            }
        ]);
        std::fs::write(
            kb.join("compiled-findings.json"),
            serde_json::to_string(&findings).unwrap(),
        )
        .unwrap();

        let report = parse_from_workspace(dir.path()).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.needs_review.len(), 1);
        assert_eq!(report.findings[0].title, "Buffer overflow");
        assert_eq!(report.needs_review[0].title, "Unchecked input");
    }

    #[test]
    fn render_markdown_structure() {
        let report = Report {
            executive_summary: "Found 1 issue.".into(),
            scouting_summary: Some("10 files scored.".into()),
            review_coverage: Some("3 reviewers.".into()),
            diff_summary: None,
            findings: vec![Finding {
                severity: Severity::High,
                title: "Test finding".into(),
                file: Some("src/foo.rs:42".into()),
                description: "A test vulnerability".into(),
                recommendation: "Fix it".into(),
                source_agent: None,
                scout_score: Some(75),
                call_chain: vec!["a.rs:fn_a".into(), "b.rs:fn_b".into()],
                reachability: Some("direct".into()),
                evidence: Some("src/foo.rs:42: bad code".into()),
                exploit_sketch: None,
                repro_status: None,
                poc_available: false,
                poc_validated: None,
                poc_path: None,
                original_severity: None,
                verdict: None,
                appraiser_notes: None,
                independent_reviewers: None,
            }],
            needs_review: vec![],
            total_cost_usd: 1.5,
            raw_result: String::new(),
        };

        let md = render_markdown(&report);
        assert!(md.contains("# Kuriboh Security Review Report"));
        assert!(md.contains("## Executive Summary"));
        assert!(md.contains("Found 1 issue."));
        assert!(md.contains("## Scouting Overview"));
        assert!(md.contains("## Review Coverage"));
        assert!(md.contains("## Findings"));
        assert!(md.contains("[High] Test finding"));
        assert!(md.contains("**Scout Score**: 75"));
        assert!(md.contains("**Call Chain**: a.rs:fn_a -> b.rs:fn_b"));
        assert!(md.contains("$1.5"));
    }

    #[test]
    fn apply_dedup_groups_merges_duplicates() {
        let findings = vec![
            (
                1,
                Finding {
                    severity: Severity::High,
                    title: "Buffer overflow".into(),
                    file: Some("src/foo.rs:42".into()),
                    description: "Detailed description of the overflow".into(),
                    recommendation: "Fix".into(),
                    source_agent: None,
                    scout_score: None,
                    call_chain: vec![],
                    reachability: None,
                    evidence: None,
                    exploit_sketch: None,
                    repro_status: None,
                    poc_available: false,
                    poc_validated: None,
                    poc_path: None,
                    original_severity: None,
                    verdict: None,
                    appraiser_notes: None,
                    independent_reviewers: None,
                },
            ),
            (
                2,
                Finding {
                    severity: Severity::Medium,
                    title: "Buffer overflow in foo".into(),
                    file: Some("src/foo.rs:50".into()),
                    description: "Short".into(),
                    recommendation: "Fix".into(),
                    source_agent: None,
                    scout_score: None,
                    call_chain: vec![],
                    reachability: None,
                    evidence: None,
                    exploit_sketch: None,
                    repro_status: None,
                    poc_available: false,
                    poc_validated: None,
                    poc_path: None,
                    original_severity: None,
                    verdict: None,
                    appraiser_notes: None,
                    independent_reviewers: None,
                },
            ),
            (
                2,
                Finding {
                    severity: Severity::Low,
                    title: "Missing error handling".into(),
                    file: Some("src/bar.rs:10".into()),
                    description: "Unwrap on network input".into(),
                    recommendation: "Use ?".into(),
                    source_agent: None,
                    scout_score: None,
                    call_chain: vec![],
                    reachability: None,
                    evidence: None,
                    exploit_sketch: None,
                    repro_status: None,
                    poc_available: false,
                    poc_validated: None,
                    poc_path: None,
                    original_severity: None,
                    verdict: None,
                    appraiser_notes: None,
                    independent_reviewers: None,
                },
            ),
        ];

        // LLM says findings 0 and 1 are duplicates.
        let result = apply_dedup_groups(findings, "[[0, 1]]");
        assert_eq!(result.len(), 2);
        // Winner should be the HIGH severity one (index 0).
        assert_eq!(result[0].1.severity, Severity::High);
        assert_eq!(result[0].1.independent_reviewers, Some(2));
        assert_eq!(result[1].1.title, "Missing error handling");
    }

    #[test]
    fn apply_dedup_groups_empty_response() {
        let findings = vec![(
            1,
            Finding {
                severity: Severity::High,
                title: "Test".into(),
                file: None,
                description: "d".into(),
                recommendation: "r".into(),
                source_agent: None,
                scout_score: None,
                call_chain: vec![],
                reachability: None,
                evidence: None,
                exploit_sketch: None,
                repro_status: None,
                poc_available: false,
                poc_validated: None,
                poc_path: None,
                original_severity: None,
                verdict: None,
                appraiser_notes: None,
                independent_reviewers: None,
            },
        )];
        let result = apply_dedup_groups(findings, "[]");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn extract_json_array_with_fences() {
        let response = "```json\n[[0, 1], [2, 3]]\n```";
        let result = extract_json_array(response).unwrap();
        assert_eq!(result, vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn extract_json_array_plain() {
        let result = extract_json_array("[[0, 2]]").unwrap();
        assert_eq!(result, vec![vec![0, 2]]);
    }

    #[test]
    fn compile_findings_sorts_by_severity() {
        let dir = tempfile::tempdir().unwrap();
        let findings_dir = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&findings_dir).unwrap();

        let appraised = serde_json::json!([
            {"severity": "LOW", "title": "Low finding", "description": "d",
             "recommendation": "r", "verdict": "confirmed"},
            {"severity": "HIGH", "title": "High finding", "description": "d",
             "recommendation": "r", "verdict": "confirmed"},
            {"severity": "MEDIUM", "title": "Rejected", "description": "d",
             "recommendation": "r", "verdict": "rejected"}
        ]);
        std::fs::write(
            findings_dir.join("appraised-1.json"),
            serde_json::to_string(&appraised).unwrap(),
        )
        .unwrap();

        let count = compile_findings(dir.path(), None).unwrap();
        assert_eq!(count, 2); // rejected one filtered

        let data =
            std::fs::read_to_string(dir.path().join(".kuriboh/compiled-findings.json")).unwrap();
        let compiled: Vec<Finding> = serde_json::from_str(&data).unwrap();
        assert_eq!(compiled[0].severity, Severity::High);
        assert_eq!(compiled[1].severity, Severity::Low);
    }

    #[test]
    fn compile_findings_filters_low_severity_outside_diff() {
        let dir = tempfile::tempdir().unwrap();
        let findings_dir = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&findings_dir).unwrap();

        let appraised = serde_json::json!([
            {"severity": "HIGH", "title": "High in diff", "file": "src/changed.rs:10",
             "description": "d", "recommendation": "r", "verdict": "confirmed"},
            {"severity": "LOW", "title": "Low in diff", "file": "src/changed.rs:20",
             "description": "d", "recommendation": "r", "verdict": "confirmed"},
            {"severity": "HIGH", "title": "High outside diff", "file": "src/other.rs:5",
             "description": "d", "recommendation": "r", "verdict": "confirmed"},
            {"severity": "MEDIUM", "title": "Medium outside diff", "file": "src/other.rs:15",
             "description": "d", "recommendation": "r", "verdict": "confirmed"},
            {"severity": "LOW", "title": "Low outside diff", "file": "src/other.rs:25",
             "description": "d", "recommendation": "r", "verdict": "confirmed"}
        ]);
        std::fs::write(
            findings_dir.join("appraised-1.json"),
            serde_json::to_string(&appraised).unwrap(),
        )
        .unwrap();

        let diff_files = vec!["src/changed.rs".to_string()];
        let count = compile_findings(dir.path(), Some(&diff_files)).unwrap();
        // HIGH in diff, LOW in diff, HIGH outside diff = 3 kept
        // MEDIUM outside diff and LOW outside diff = 2 filtered
        assert_eq!(count, 3);

        let data =
            std::fs::read_to_string(dir.path().join(".kuriboh/compiled-findings.json")).unwrap();
        let compiled: Vec<Finding> = serde_json::from_str(&data).unwrap();
        assert_eq!(compiled[0].title, "High in diff");
        assert_eq!(compiled[1].title, "High outside diff");
        assert_eq!(compiled[2].title, "Low in diff");
    }

    #[test]
    fn reviewers_with_findings_filters_empty() {
        let dir = tempfile::tempdir().unwrap();
        let findings_dir = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&findings_dir).unwrap();

        std::fs::write(findings_dir.join("reviewer-1.json"), "[]").unwrap();
        std::fs::write(
            findings_dir.join("reviewer-2.json"),
            r#"[{"severity":"HIGH","title":"t","description":"d","recommendation":"r"}]"#,
        )
        .unwrap();
        std::fs::write(findings_dir.join("reviewer-3.json"), "[]").unwrap();

        let result = reviewers_with_findings(dir.path(), &[1, 2, 3]);
        assert_eq!(result, vec![2]);
    }

    #[test]
    fn write_json_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let report = Report {
            executive_summary: "No issues.".into(),
            scouting_summary: None,
            review_coverage: None,
            diff_summary: None,
            findings: vec![],
            needs_review: vec![],
            total_cost_usd: 0.0,
            raw_result: String::new(),
        };

        write(&report, &path, true).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["executive_summary"], "No issues.");
    }
}

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

/// Pre-deduplicate findings across all reviewers before appraisal.
///
/// Reads every `findings/reviewer-*.json`, groups findings by a normalized key
/// `(file_stem, lowercase_title)`, keeps the best version per group (highest
/// severity, then longest description), and writes the deduplicated arrays back
/// to each reviewer's file. Returns `(total_before, total_after)`.
pub fn pre_deduplicate_findings(target: &Path) -> Result<(usize, usize)> {
    let findings_dir = target.join(".kuriboh/findings");
    if !findings_dir.exists() {
        return Ok((0, 0));
    }

    // Collect all (reviewer_id, finding) pairs.
    let mut all: Vec<(u32, Finding)> = Vec::new();
    let mut reviewer_files: Vec<(u32, std::path::PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(&findings_dir)
        .with_context(|| format!("reading {}", findings_dir.display()))?
        .flatten()
    {
        let path = entry.path();
        let fname = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // Match reviewer-N.json but NOT appraised-N.json
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
            Err(_) => continue, // malformed — skip, appraisal will handle it
        };
        for f in findings {
            all.push((id, f));
        }
        reviewer_files.push((id, path));
    }

    let total_before = all.len();
    if total_before == 0 {
        return Ok((0, 0));
    }

    // Group by normalized (file_stem, title) key.
    let mut groups: std::collections::HashMap<(String, String), Vec<(u32, Finding)>> =
        std::collections::HashMap::new();
    for (rid, finding) in all {
        let key = dedup_key(&finding);
        groups.entry(key).or_default().push((rid, finding));
    }

    // For each group, pick the winner: highest severity, then longest description.
    let mut kept: std::collections::HashMap<u32, Vec<Finding>> = std::collections::HashMap::new();
    for (_key, mut group) in groups {
        // Sort: lowest Severity enum value = highest severity (Critical < High < ... < Info).
        group.sort_by(|a, b| {
            a.1.severity
                .cmp(&b.1.severity)
                .then_with(|| b.1.description.len().cmp(&a.1.description.len()))
        });
        let (winner_id, mut winner) = group.remove(0);
        if !group.is_empty() {
            // Record how many independent reviewers found this.
            let reviewer_count = {
                let mut ids: Vec<u32> = vec![winner_id];
                ids.extend(group.iter().map(|(rid, _)| *rid));
                ids.sort_unstable();
                ids.dedup();
                ids.len() as u32
            };
            if winner.independent_reviewers.unwrap_or(0) < reviewer_count {
                winner.independent_reviewers = Some(reviewer_count);
            }
        }
        kept.entry(winner_id).or_default().push(winner);
    }

    // Write back deduplicated findings per reviewer.
    for (id, path) in &reviewer_files {
        let findings = kept.remove(id).unwrap_or_default();
        let json = serde_json::to_string_pretty(&findings)
            .with_context(|| format!("serializing deduped findings for reviewer {id}"))?;
        std::fs::write(path, json)
            .with_context(|| format!("writing deduped {}", path.display()))?;
    }

    // Recount from what we wrote back.
    let mut total_after = 0usize;
    for (_id, path) in &reviewer_files {
        let data = std::fs::read_to_string(path).unwrap_or_default();
        let findings: Vec<serde_json::Value> = serde_json::from_str(&data).unwrap_or_default();
        total_after += findings.len();
    }

    Ok((total_before, total_after))
}

/// Compute the dedup key for a finding: (file_stem, normalized_title).
///
/// `file_stem` strips `:line` suffixes so `foo.rs:42` and `foo.rs:50` with the
/// same title are considered duplicates.
fn dedup_key(f: &Finding) -> (String, String) {
    let file_stem = f
        .file
        .as_deref()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_string();
    let title = f.title.trim().to_lowercase();
    (file_stem, title)
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

    // Sum costs from state.json if available.
    let total_cost = State::load(target).ok().map_or(0.0, |s| {
        s.phases.values().filter_map(|p| p.cost_usd).sum::<f64>()
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
    fn pre_dedup_removes_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let findings_dir = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&findings_dir).unwrap();

        // Reviewer 1: HIGH buffer overflow in foo.rs
        let r1 = serde_json::json!([{
            "severity": "HIGH",
            "title": "Buffer overflow",
            "file": "src/foo.rs:42",
            "description": "Writes past buffer end via unchecked index",
            "recommendation": "Add bounds check"
        }]);
        std::fs::write(
            findings_dir.join("reviewer-1.json"),
            serde_json::to_string(&r1).unwrap(),
        )
        .unwrap();

        // Reviewer 2: same finding (different line) + a unique finding
        let r2 = serde_json::json!([
            {
                "severity": "MEDIUM",
                "title": "Buffer overflow",
                "file": "src/foo.rs:50",
                "description": "Short desc",
                "recommendation": "Fix it"
            },
            {
                "severity": "LOW",
                "title": "Missing error handling",
                "file": "src/bar.rs:10",
                "description": "Unwrap on network input",
                "recommendation": "Use ?"
            }
        ]);
        std::fs::write(
            findings_dir.join("reviewer-2.json"),
            serde_json::to_string(&r2).unwrap(),
        )
        .unwrap();

        let (before, after) = pre_deduplicate_findings(dir.path()).unwrap();
        assert_eq!(before, 3);
        assert_eq!(after, 2); // one duplicate removed

        // The kept buffer overflow should be the HIGH one (higher severity).
        let r1_data = std::fs::read_to_string(findings_dir.join("reviewer-1.json")).unwrap();
        let r1_findings: Vec<Finding> = serde_json::from_str(&r1_data).unwrap();
        assert_eq!(r1_findings.len(), 1);
        assert_eq!(r1_findings[0].severity, Severity::High);
        assert_eq!(r1_findings[0].independent_reviewers, Some(2));

        // Reviewer 2 keeps only the unique finding.
        let r2_data = std::fs::read_to_string(findings_dir.join("reviewer-2.json")).unwrap();
        let r2_findings: Vec<Finding> = serde_json::from_str(&r2_data).unwrap();
        assert_eq!(r2_findings.len(), 1);
        assert_eq!(r2_findings[0].title, "Missing error handling");
    }

    #[test]
    fn pre_dedup_no_findings_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (before, after) = pre_deduplicate_findings(dir.path()).unwrap();
        assert_eq!(before, 0);
        assert_eq!(after, 0);
    }

    #[test]
    fn pre_dedup_empty_findings() {
        let dir = tempfile::tempdir().unwrap();
        let findings_dir = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&findings_dir).unwrap();
        std::fs::write(findings_dir.join("reviewer-1.json"), "[]").unwrap();

        let (before, after) = pre_deduplicate_findings(dir.path()).unwrap();
        assert_eq!(before, 0);
        assert_eq!(after, 0);
    }

    #[test]
    fn pre_dedup_skips_appraised_files() {
        let dir = tempfile::tempdir().unwrap();
        let findings_dir = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&findings_dir).unwrap();

        let finding = serde_json::json!([{
            "severity": "HIGH",
            "title": "Test",
            "description": "desc",
            "recommendation": "fix"
        }]);
        std::fs::write(
            findings_dir.join("reviewer-1.json"),
            serde_json::to_string(&finding).unwrap(),
        )
        .unwrap();
        std::fs::write(
            findings_dir.join("appraised-1.json"),
            serde_json::to_string(&finding).unwrap(),
        )
        .unwrap();

        let (before, after) = pre_deduplicate_findings(dir.path()).unwrap();
        assert_eq!(before, 1);
        assert_eq!(after, 1);
    }

    #[test]
    fn dedup_key_strips_line_numbers() {
        let f = Finding {
            severity: Severity::High,
            title: " Buffer Overflow ".into(),
            file: Some("src/foo.rs:42".into()),
            description: String::new(),
            recommendation: String::new(),
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
        };
        let key = super::dedup_key(&f);
        assert_eq!(
            key,
            ("src/foo.rs".to_string(), "buffer overflow".to_string())
        );
    }

    #[test]
    fn write_json_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let report = Report {
            executive_summary: "No issues.".into(),
            scouting_summary: None,
            review_coverage: None,
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

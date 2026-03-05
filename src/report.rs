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
    /// Whether a proof-of-concept was provided by the reviewer.
    #[serde(default)]
    pub poc_available: bool,
    /// Whether the appraiser validated the PoC successfully.
    #[serde(default)]
    pub poc_validated: Option<bool>,
    /// Path to the PoC file, if any.
    #[serde(default)]
    pub poc_path: Option<String>,
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

/// Extract a named Markdown section (case-insensitive heading match).
///
/// Returns the body text between the matched `## <heading>` and the next
/// heading of the same or higher level, or `None` if the section is absent.
fn extract_section(raw: &str, heading: &str) -> Option<String> {
    let mut in_section = false;
    let mut lines: Vec<&str> = Vec::new();

    for line in raw.lines() {
        if line
            .trim_start_matches('#')
            .trim()
            .eq_ignore_ascii_case(heading)
        {
            in_section = true;
            continue;
        }
        if in_section {
            if line.starts_with('#') {
                break;
            }
            lines.push(line);
        }
    }

    let body = lines.join("\n").trim().to_owned();
    if body.is_empty() {
        None
    } else {
        Some(body)
    }
}

/// Best-effort extraction of the Executive Summary section from Markdown output.
fn extract_executive_summary(raw: &str) -> String {
    // Find the "Executive Summary" heading and collect lines until the next heading.
    let mut in_summary = false;
    let mut lines: Vec<&str> = Vec::new();

    for line in raw.lines() {
        if line
            .trim_start_matches('#')
            .trim()
            .eq_ignore_ascii_case("executive summary")
        {
            in_summary = true;
            continue;
        }
        if in_summary {
            if line.starts_with('#') {
                break;
            }
            lines.push(line);
        }
    }

    let summary = lines.join("\n").trim().to_owned();
    if summary.is_empty() {
        // Fallback: first non-empty paragraph before any heading.
        raw.lines()
            .skip_while(|l| l.trim().is_empty() || l.starts_with('#'))
            .take(5)
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        summary
    }
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
    let scouting_summary =
        std::fs::read_to_string(kb.join("scores.json"))
            .ok()
            .map(|data| {
                let scores: Vec<serde_json::Value> =
                    serde_json::from_str(&data).unwrap_or_default();
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
    let total_cost = State::load(target)
        .ok()
        .map(|s| s.phases.values().filter_map(|p| p.cost_usd).sum::<f64>())
        .unwrap_or(0.0);

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

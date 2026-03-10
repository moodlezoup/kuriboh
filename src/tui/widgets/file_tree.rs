use std::collections::BTreeMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::state::{FileState, FindingCounts, TuiState};

pub fn render(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Deep Review ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 3 {
        return;
    }

    // Header line
    let active_count = state.active_reviewer_count();
    let reviewed = state.files_reviewed_count();
    let total_findings = state.total_finding_counts();
    let header = Line::from(vec![
        Span::styled(
            format!("Reviewers: {active_count} active"),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("Files: {reviewed}/{} reviewed", state.total_assigned_files),
            Style::default().fg(Color::White),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!(
                "Findings: {} ({}H {}M {}L)",
                total_findings.total(),
                total_findings.high,
                total_findings.medium,
                total_findings.low,
            ),
            severity_color(&total_findings),
        ),
    ]);

    let tree_lines = build_tree_lines(state);

    let mut lines = vec![header, Line::raw("")];
    lines.extend(tree_lines);

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn severity_color(counts: &FindingCounts) -> Style {
    if counts.high > 0 {
        Style::default().fg(Color::Red)
    } else if counts.medium > 0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    }
}

/// Represents a directory in the tree with its children.
struct DirEntry {
    /// Files that are active or have findings (expanded).
    visible_files: Vec<(String, FileState)>,
    /// Count of files that were reviewed but have no findings (collapsed).
    reviewed_no_findings: u32,
}

fn build_tree_lines(state: &TuiState) -> Vec<Line<'static>> {
    // Group files by directory.
    let mut dirs: BTreeMap<String, DirEntry> = BTreeMap::new();

    for (path, file_state) in &state.file_activity {
        let (dir, filename) = match path.rfind('/') {
            Some(idx) => (path[..idx].to_string(), path[idx + 1..].to_string()),
            None => (".".to_string(), path.clone()),
        };

        let entry = dirs.entry(dir).or_insert_with(|| DirEntry {
            visible_files: Vec::new(),
            reviewed_no_findings: 0,
        });

        let has_activity = !file_state.active_reviewers.is_empty();
        let has_findings = file_state.findings.total() > 0;

        if has_activity || has_findings {
            entry.visible_files.push((filename, file_state.clone()));
        } else if file_state.reviewed {
            entry.reviewed_no_findings += 1;
        }
    }

    let mut lines = Vec::new();

    for (dir, entry) in &dirs {
        if entry.visible_files.is_empty() && entry.reviewed_no_findings == 0 {
            continue;
        }

        // Sort: active first, then by severity, then alphabetical.
        let mut files = entry.visible_files.clone();
        files.sort_by(|a, b| {
            let a_active = !a.1.active_reviewers.is_empty();
            let b_active = !b.1.active_reviewers.is_empty();
            b_active
                .cmp(&a_active)
                .then_with(|| b.1.findings.high.cmp(&a.1.findings.high))
                .then_with(|| b.1.findings.medium.cmp(&a.1.findings.medium))
                .then_with(|| a.0.cmp(&b.0))
        });

        if !files.is_empty() {
            // Directory header
            lines.push(Line::styled(
                format!("{dir}/"),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ));

            for (filename, file_state) in &files {
                lines.push(build_file_line(filename, file_state));
            }

            // Inline collapsed summary for this directory.
            if entry.reviewed_no_findings > 0 {
                lines.push(Line::styled(
                    format!(
                        "    ({} more reviewed, no findings)",
                        entry.reviewed_no_findings
                    ),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        } else {
            // Entire directory collapsed.
            lines.push(Line::from(vec![
                Span::styled(format!("{dir}/"), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("  {} reviewed", entry.reviewed_no_findings),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    lines
}

fn build_file_line(filename: &str, file_state: &FileState) -> Line<'static> {
    let is_active = !file_state.active_reviewers.is_empty();
    let has_findings = file_state.findings.total() > 0;

    let prefix = if is_active { "  \u{25b6} " } else { "  \u{25cf} " };
    let prefix_color = if is_active { Color::Green } else { Color::Red };

    let mut spans: Vec<Span> = vec![
        Span::styled(prefix.to_string(), Style::default().fg(prefix_color)),
        Span::styled(
            format!("{:<24}", filename),
            Style::default().fg(Color::White),
        ),
    ];

    // Column 1: active count
    if is_active {
        let count = file_state.active_reviewers.len();
        spans.push(Span::styled(
            format!("{count} active    "),
            Style::default().fg(Color::Green),
        ));
    } else {
        spans.push(Span::raw("              "));
    }

    // Column 2: severity counts
    if has_findings {
        let mut parts: Vec<Span> = Vec::new();
        if file_state.findings.high > 0 {
            parts.push(Span::styled(
                format!("{}H", file_state.findings.high),
                Style::default().fg(Color::Red),
            ));
        }
        if file_state.findings.medium > 0 {
            if !parts.is_empty() {
                parts.push(Span::raw(" "));
            }
            parts.push(Span::styled(
                format!("{}M", file_state.findings.medium),
                Style::default().fg(Color::Yellow),
            ));
        }
        if file_state.findings.low > 0 {
            if !parts.is_empty() {
                parts.push(Span::raw(" "));
            }
            parts.push(Span::styled(
                format!("{}L", file_state.findings.low),
                Style::default().fg(Color::Cyan),
            ));
        }
        spans.extend(parts);
    }

    Line::from(spans)
}

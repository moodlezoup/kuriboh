use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::tui::state::TuiState;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    content: &str,
    scroll: u16,
    state: &TuiState,
    colon_pressed: bool,
) {
    let chunks = Layout::vertical([
        Constraint::Min(3),    // Report content
        Constraint::Length(1), // Status bar
    ])
    .split(area);

    let total_lines = content.lines().count();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Report ")
        .title_alignment(Alignment::Left);
    let inner = block.inner(chunks[0]);

    // Clamp scroll to content bounds.
    let max_scroll = total_lines.saturating_sub(inner.height as usize) as u16;
    let clamped_scroll = scroll.min(max_scroll);

    // Style the content with basic markdown highlighting.
    let lines: Vec<Line> = content.lines().map(style_markdown_line).collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((clamped_scroll, 0));
    frame.render_widget(paragraph, chunks[0]);

    // Scrollbar
    if total_lines > inner.height as usize {
        let mut scrollbar_state =
            ScrollbarState::new(total_lines).position(clamped_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            chunks[0],
            &mut scrollbar_state,
        );
    }

    // Status bar
    let prompt = if colon_pressed { ":" } else { "" };
    let status = Line::from(vec![
        Span::styled(
            format!(" ${:.2}", state.cumulative_cost),
            Style::default().fg(Color::Green),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("{} findings", total_lines_with_prefix(content, "### [")),
            Style::default().fg(Color::White),
        ),
        Span::raw("  |  "),
        Span::styled("j/k scroll  :q quit", Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(prompt, Style::default().fg(Color::White)),
    ]);
    frame.render_widget(Paragraph::new(status), chunks[1]);
}

/// Count lines starting with a given prefix (for counting findings).
fn total_lines_with_prefix(content: &str, prefix: &str) -> usize {
    content.lines().filter(|l| l.starts_with(prefix)).count()
}

/// Apply basic styling to a markdown line.
fn style_markdown_line(line: &str) -> Line<'static> {
    let owned = line.to_string();
    if owned.starts_with("# ") {
        Line::styled(
            owned,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else if owned.starts_with("## ") {
        Line::styled(
            owned,
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
    } else if owned.starts_with("### [Critical]")
        || owned.starts_with("### [CRITICAL]")
        || owned.starts_with("### [High]")
        || owned.starts_with("### [HIGH]")
    {
        Line::styled(
            owned,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else if owned.starts_with("### [Medium]") || owned.starts_with("### [MEDIUM]") {
        Line::styled(owned, Style::default().fg(Color::Yellow))
    } else if owned.starts_with("### [Low]") || owned.starts_with("### [LOW]") {
        Line::styled(owned, Style::default().fg(Color::Cyan))
    } else if owned.starts_with("### [Info]") || owned.starts_with("### [INFO]") {
        Line::styled(owned, Style::default().fg(Color::DarkGray))
    } else if owned.starts_with("### ") {
        Line::styled(owned, Style::default().add_modifier(Modifier::BOLD))
    } else if owned.starts_with("- **") {
        Line::styled(owned, Style::default().fg(Color::White))
    } else if owned.starts_with("---") {
        Line::styled(owned, Style::default().fg(Color::DarkGray))
    } else {
        Line::raw(owned)
    }
}

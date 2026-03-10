use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::state::TuiState;

const DISPLAY_LINES: usize = 5;

pub fn render(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default().borders(Borders::ALL).title(" Activity ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let entries: Vec<_> = state
        .log_entries
        .iter()
        .rev()
        .take(DISPLAY_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let lines: Vec<Line> = entries
        .iter()
        .map(|entry| {
            Line::from(vec![
                Span::styled(
                    format!("[{:>8}] ", entry.agent),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(entry.message.clone()),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

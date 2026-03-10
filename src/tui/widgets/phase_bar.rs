use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::tui::state::TuiState;

pub fn render(frame: &mut Frame, area: Rect, state: &TuiState, quit_requested: bool) {
    let block = Block::default().borders(Borders::BOTTOM).title(" kuriboh ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::horizontal([Constraint::Min(20), Constraint::Length(25)]).split(inner);

    let progress = state.phase_progress();
    let elapsed = state.elapsed();
    let elapsed_str = format!("{}m{}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60);

    if quit_requested {
        let warning = Paragraph::new("Press q again to abort review")
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(warning, chunks[0]);
    } else if progress < 0.0 {
        // Indeterminate: show phase name with animated dots
        let dots = ".".repeat((elapsed.as_secs() % 4) as usize + 1);
        let text = format!(" {} {}", state.current_phase.label(), dots);
        frame.render_widget(Paragraph::new(text), chunks[0]);
    } else {
        let pct = (progress * 100.0) as u16;
        let label = format!("{} ({}%)", state.current_phase.label(), pct);
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Green))
            .ratio(progress.clamp(0.0, 1.0))
            .label(label);
        frame.render_widget(gauge, chunks[0]);
    }

    let right = format!(" ${:.2}  {}", state.cumulative_cost, elapsed_str);
    let right_widget = Paragraph::new(right).alignment(Alignment::Right);
    frame.render_widget(right_widget, chunks[1]);
}

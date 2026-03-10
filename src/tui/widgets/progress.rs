use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::state::TuiState;

pub fn render(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", state.current_phase.label()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let progress = state.phase_progress();
    let description = match state.current_phase_name() {
        "exploration" => "Exploring codebase structure...".to_string(),
        "scouting" => {
            if state.total_files_to_score > 0 {
                format!(
                    "Scoring {} files ({} complete)",
                    state.total_files_to_score, state.files_scored
                )
            } else {
                "Computing file metrics...".to_string()
            }
        }
        "appraisal_compilation" => "Appraising and compiling findings...".to_string(),
        _ => String::new(),
    };

    let pct_str = if progress < 0.0 {
        ".".repeat((state.elapsed().as_secs() % 4) as usize + 1)
    } else {
        format!("{}%", (progress * 100.0) as u32)
    };

    let text = format!("\n\n  {description}\n\n  {pct_str}");
    let paragraph = Paragraph::new(text).style(Style::default().fg(Color::White));
    frame.render_widget(paragraph, inner);
}

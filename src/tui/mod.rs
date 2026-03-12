mod state;
mod widgets;

use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{self as ct_event, Event, KeyCode, KeyEventKind};
use ratatui::prelude::*;
use tokio::sync::mpsc;

use crate::events::ClaudeEvent;
use crate::scanner::FileScore;

pub use self::state::TuiState;

/// Events sent from the phase loop / runner to the TUI.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    /// A raw event from a Claude Code session.
    Claude(ClaudeEvent),
    /// A phase has started.
    PhaseStart { name: String },
    /// A phase completed successfully.
    PhaseComplete { name: String, cost_usd: f64 },
    /// Scores loaded after scouting (for file tree context).
    ScoresLoaded(Vec<FileScore>),
    /// A reviewer was assigned to a starting file.
    ReviewerAssigned { id: u32, file: String },
    /// The final report is ready; show it in the TUI.
    ReportReady { content: String },
    /// The pipeline is done; TUI should show the report and wait for :q.
    Shutdown,
}

/// The main TUI application. Owns the terminal and render loop.
pub struct TuiApp {
    state: TuiState,
    event_rx: mpsc::UnboundedReceiver<TuiEvent>,
    workspace_path: PathBuf,
    quit_requested: bool,
    /// When set, we're showing the final report and waiting for :q.
    report_content: Option<String>,
    report_scroll: u16,
    /// Tracks `:q` input sequence.
    colon_pressed: bool,
}

impl TuiApp {
    pub fn new(event_rx: mpsc::UnboundedReceiver<TuiEvent>, workspace_path: PathBuf) -> Self {
        Self {
            state: TuiState::new(),
            event_rx,
            workspace_path,
            quit_requested: false,
            report_content: None,
            report_scroll: 0,
            colon_pressed: false,
        }
    }

    /// Run the TUI render loop. Returns when Shutdown is received or user quits.
    pub async fn run(mut self) -> Result<()> {
        let mut terminal = ratatui::init();
        // Set panic hook to restore terminal on crash.
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            ratatui::restore();
            original_hook(info);
        }));

        let tick_rate = tokio::time::Duration::from_millis(100);
        let mut tick_interval = tokio::time::interval(tick_rate);
        let mut poll_counter: u32 = 0;

        loop {
            terminal.draw(|frame| self.render(frame))?;

            tokio::select! {
                event = self.event_rx.recv() => {
                    match event {
                        Some(TuiEvent::ReportReady { content }) => {
                            self.report_content = Some(content);
                        }
                        Some(TuiEvent::Shutdown) | None => {
                            if self.report_content.is_some() {
                                // Stay in report view — don't break.
                                // Drain remaining events without blocking.
                                self.event_rx.close();
                            } else {
                                break;
                            }
                        }
                        Some(ev) => self.state.handle_event(ev),
                    }
                }
                _ = tick_interval.tick() => {
                    if ct_event::poll(std::time::Duration::ZERO)? {
                        if let Event::Key(key) = ct_event::read()? {
                            if key.kind == KeyEventKind::Press {
                                if self.report_content.is_some() {
                                    // Report view: handle :q and scrolling.
                                    if self.handle_report_key(key.code) {
                                        break;
                                    }
                                } else if key.code == KeyCode::Char('q') {
                                    if self.quit_requested {
                                        break;
                                    }
                                    self.quit_requested = true;
                                }
                            }
                        }
                    }
                    if self.report_content.is_none() {
                        // Poll workspace files every 5 ticks (500ms)
                        poll_counter += 1;
                        if poll_counter.is_multiple_of(5) {
                            self.state.poll_workspace(&self.workspace_path);
                        }
                        self.state.decay_active_reviewers();
                    }
                }
            }
        }

        ratatui::restore();
        Ok(())
    }

    /// Handle a keypress in report view. Returns true if we should quit.
    fn handle_report_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char(':') => {
                self.colon_pressed = true;
                false
            }
            KeyCode::Char('q') if self.colon_pressed => true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.report_scroll = self.report_scroll.saturating_add(1);
                self.colon_pressed = false;
                false
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.report_scroll = self.report_scroll.saturating_sub(1);
                self.colon_pressed = false;
                false
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.report_scroll = self.report_scroll.saturating_add(20);
                self.colon_pressed = false;
                false
            }
            KeyCode::PageUp => {
                self.report_scroll = self.report_scroll.saturating_sub(20);
                self.colon_pressed = false;
                false
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.report_scroll = 0;
                self.colon_pressed = false;
                false
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.report_scroll = u16::MAX;
                self.colon_pressed = false;
                false
            }
            _ => {
                self.colon_pressed = false;
                false
            }
        }
    }

    fn render(&self, frame: &mut Frame) {
        if self.report_content.is_some() {
            self.render_report(frame);
            return;
        }

        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(7),
        ])
        .split(frame.area());

        widgets::phase_bar::render(frame, chunks[0], &self.state, self.quit_requested);

        match self.state.current_phase_name() {
            "deep_review" | "appraisal_compilation" => {
                widgets::file_tree::render(frame, chunks[1], &self.state);
            }
            _ => widgets::progress::render(frame, chunks[1], &self.state),
        }

        widgets::activity_log::render(frame, chunks[2], &self.state);
    }

    fn render_report(&self, frame: &mut Frame) {
        let content = self.report_content.as_deref().unwrap_or_default();
        widgets::report::render(
            frame,
            frame.area(),
            content,
            self.report_scroll,
            &self.state,
            self.colon_pressed,
        );
    }
}

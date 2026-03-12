use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Instant;

use crate::events::{ClaudeEvent, ContentBlock};
use crate::report::{Finding, Severity};

use super::TuiEvent;

/// Phases in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Exploration,
    Scouting,
    DeepReview,
    Appraisal,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exploration => "Exploration",
            Self::Scouting => "Scouting",
            Self::DeepReview => "Deep Review",
            Self::Appraisal => "Appraisal & Compilation",
        }
    }
}

/// Severity counts for a single file.
#[derive(Debug, Clone, Default)]
pub struct FindingCounts {
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

impl FindingCounts {
    pub fn total(&self) -> u32 {
        self.high + self.medium + self.low
    }
}

/// Per-file state during deep review.
#[derive(Debug, Clone)]
pub struct FileState {
    pub active_reviewers: HashSet<u32>,
    pub findings: FindingCounts,
    pub last_activity: Instant,
    pub reviewed: bool,
}

/// A single log entry for the activity feed.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub agent: String,
    pub message: String,
}

pub struct TuiState {
    pub current_phase: Phase,
    pub phase_start_time: Instant,
    pub cumulative_cost: f64,

    // Scouting progress
    pub total_files_to_score: usize,
    pub files_scored: usize,

    // Deep review state
    pub file_activity: HashMap<String, FileState>,
    pub total_assigned_files: usize,
    pub reviewer_files: HashMap<u32, String>,

    // Appraisal progress
    pub total_findings_to_appraise: usize,
    pub findings_appraised: usize,

    // Activity log
    pub log_entries: VecDeque<LogEntry>,
}

const MAX_LOG_ENTRIES: usize = 100;
const DECAY_TIMEOUT_SECS: u64 = 30;

impl TuiState {
    pub fn new() -> Self {
        Self {
            current_phase: Phase::Exploration,
            phase_start_time: Instant::now(),
            cumulative_cost: 0.0,
            total_files_to_score: 0,
            files_scored: 0,
            file_activity: HashMap::new(),
            total_assigned_files: 0,
            reviewer_files: HashMap::new(),
            total_findings_to_appraise: 0,
            findings_appraised: 0,
            log_entries: VecDeque::new(),
        }
    }

    pub fn current_phase_name(&self) -> &str {
        match self.current_phase {
            Phase::Exploration => "exploration",
            Phase::Scouting => "scouting",
            Phase::DeepReview => "deep_review",
            Phase::Appraisal => "appraisal_compilation",
        }
    }

    pub fn phase_progress(&self) -> f64 {
        match self.current_phase {
            Phase::Exploration => -1.0,
            Phase::Scouting => {
                if self.total_files_to_score == 0 {
                    -1.0
                } else {
                    self.files_scored as f64 / self.total_files_to_score as f64
                }
            }
            Phase::DeepReview => {
                if self.total_assigned_files == 0 {
                    -1.0
                } else {
                    let reviewed = self.file_activity.values().filter(|f| f.reviewed).count();
                    reviewed as f64 / self.total_assigned_files as f64
                }
            }
            Phase::Appraisal => {
                if self.total_findings_to_appraise == 0 {
                    -1.0
                } else {
                    self.findings_appraised as f64 / self.total_findings_to_appraise as f64
                }
            }
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.phase_start_time.elapsed()
    }

    pub fn handle_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::PhaseStart { name } => {
                self.current_phase = match name.as_str() {
                    "exploration" => Phase::Exploration,
                    "scouting" => Phase::Scouting,
                    "deep_review" => Phase::DeepReview,
                    "appraisal_compilation" => Phase::Appraisal,
                    _ => return,
                };
                self.phase_start_time = Instant::now();
                // Clear file activity when entering deep review so it starts
                // clean. Preserve it when entering appraisal so the file tree
                // carries over and severity counts update in place.
                if self.current_phase != Phase::Appraisal {
                    self.file_activity.clear();
                    self.reviewer_files.clear();
                }
                self.push_log(
                    "system",
                    &format!("Phase started: {}", self.current_phase.label()),
                );
            }
            TuiEvent::PhaseComplete { name, cost_usd } => {
                self.cumulative_cost += cost_usd;
                self.push_log(
                    "system",
                    &format!("Phase complete: {name} (${cost_usd:.2})"),
                );
            }
            TuiEvent::ScoresLoaded(scores) => {
                self.total_files_to_score = scores.len();
                self.files_scored = scores.len();
                self.total_assigned_files = scores.len();
            }
            TuiEvent::ReviewerAssigned { id, file } => {
                self.push_log(&format!("r-{id}"), &format!("Assigned to {file}"));
            }
            TuiEvent::Claude(ref claude_event) => {
                self.handle_claude_event(claude_event);
            }
            TuiEvent::ReportReady { .. } | TuiEvent::Shutdown => {}
        }
    }

    fn handle_claude_event(&mut self, event: &ClaudeEvent) {
        match event {
            ClaudeEvent::Assistant { message, .. } => {
                for block in &message.content {
                    if let ContentBlock::ToolUse { name, input, .. } = block {
                        self.handle_tool_call(name, input);
                    }
                }
            }
            ClaudeEvent::Result {
                total_cost_usd: Some(cost),
                ..
            } => {
                self.cumulative_cost += cost;
            }
            _ => {}
        }
    }

    fn handle_tool_call(&mut self, tool_name: &str, input: &serde_json::Value) {
        if self.current_phase != Phase::DeepReview {
            return;
        }

        let file_path = match tool_name {
            "Read" | "read" => input.get("file_path").and_then(|v| v.as_str()),
            "Grep" | "grep" => input.get("path").and_then(|v| v.as_str()),
            "Glob" | "glob" => input.get("path").and_then(|v| v.as_str()),
            _ => None,
        };

        let file_path = match file_path {
            Some(p) => p,
            None => return,
        };

        let relative = normalize_file_path(file_path);
        if relative.is_empty() || !relative.ends_with(".rs") {
            return;
        }

        let reviewer_id = infer_reviewer_id(file_path);

        // Update previous file for this reviewer.
        if let Some(prev_file) = self.reviewer_files.insert(reviewer_id, relative.clone()) {
            if prev_file != relative {
                if let Some(state) = self.file_activity.get_mut(&prev_file) {
                    state.active_reviewers.remove(&reviewer_id);
                    if state.active_reviewers.is_empty() {
                        state.reviewed = true;
                    }
                }
            }
        }

        let state = self
            .file_activity
            .entry(relative.clone())
            .or_insert_with(|| FileState {
                active_reviewers: HashSet::new(),
                findings: FindingCounts::default(),
                last_activity: Instant::now(),
                reviewed: false,
            });
        state.active_reviewers.insert(reviewer_id);
        state.last_activity = Instant::now();

        self.push_log(
            &format!("r-{reviewer_id}"),
            &format!("{tool_name} {relative}"),
        );
    }

    /// Remove reviewers from active sets if they've been idle too long.
    pub fn decay_active_reviewers(&mut self) {
        let now = Instant::now();
        for state in self.file_activity.values_mut() {
            if !state.active_reviewers.is_empty()
                && now.duration_since(state.last_activity).as_secs() > DECAY_TIMEOUT_SECS
            {
                state.active_reviewers.clear();
                state.reviewed = true;
            }
        }
    }

    /// Poll workspace files for findings. Resets finding counts before each poll
    /// to avoid unbounded accumulation.
    ///
    /// During deep review: reads `reviewer-*.json`.
    /// During appraisal: reads `appraised-*.json` (which reflect severity adjustments
    /// and rejected findings filtered out).
    pub fn poll_workspace(&mut self, workspace: &Path) {
        let prefix = match self.current_phase {
            Phase::DeepReview => "reviewer-",
            Phase::Appraisal => "appraised-",
            _ => return,
        };

        let findings_dir = workspace.join("findings");
        let Ok(entries) = std::fs::read_dir(&findings_dir) else {
            return;
        };

        // Reset all finding counts before re-scanning.
        for state in self.file_activity.values_mut() {
            state.findings = FindingCounts::default();
        }

        for entry in entries.flatten() {
            let path = entry.path();
            let fname = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();

            if !fname.starts_with(prefix) || !fname.ends_with(".json") {
                continue;
            }

            let Ok(data) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(findings) = serde_json::from_str::<Vec<Finding>>(&data) else {
                continue;
            };

            for finding in &findings {
                // During appraisal, skip rejected findings.
                if finding.verdict.as_deref() == Some("rejected") {
                    continue;
                }
                if let Some(ref file) = finding.file {
                    let normalized = normalize_file_path(file);
                    if normalized.is_empty() {
                        continue;
                    }
                    let counts =
                        self.file_activity
                            .entry(normalized)
                            .or_insert_with(|| FileState {
                                active_reviewers: HashSet::new(),
                                findings: FindingCounts::default(),
                                last_activity: Instant::now(),
                                reviewed: true,
                            });
                    match finding.severity {
                        Severity::Critical | Severity::High => counts.findings.high += 1,
                        Severity::Medium => counts.findings.medium += 1,
                        Severity::Low | Severity::Info => counts.findings.low += 1,
                    }
                }
            }
        }
    }

    pub fn active_reviewer_count(&self) -> usize {
        self.file_activity
            .values()
            .flat_map(|f| f.active_reviewers.iter())
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn files_reviewed_count(&self) -> usize {
        self.file_activity.values().filter(|f| f.reviewed).count()
    }

    pub fn total_finding_counts(&self) -> FindingCounts {
        let mut total = FindingCounts::default();
        for f in self.file_activity.values() {
            total.high += f.findings.high;
            total.medium += f.findings.medium;
            total.low += f.findings.low;
        }
        total
    }

    fn push_log(&mut self, agent: &str, message: &str) {
        self.log_entries.push_back(LogEntry {
            agent: agent.to_string(),
            message: message.to_string(),
        });
        if self.log_entries.len() > MAX_LOG_ENTRIES {
            self.log_entries.pop_front();
        }
    }
}

/// Infer reviewer ID from file path by checking worktree path patterns.
fn infer_reviewer_id(path: &str) -> u32 {
    if let Some(idx) = path.find(".kuriboh/worktrees/reviewer-") {
        let after = &path[idx + ".kuriboh/worktrees/reviewer-".len()..];
        if let Some(end) = after.find('/') {
            if let Ok(id) = after[..end].parse::<u32>() {
                return id;
            }
        }
    }
    0
}

/// Normalize a file path to a relative path (strip worktree prefixes, absolute paths, line numbers).
fn normalize_file_path(path: &str) -> String {
    let mut p = path;
    // Strip line number suffix (e.g., "src/foo.rs:42")
    if let Some(idx) = p.rfind(':') {
        if p[idx + 1..].chars().all(|c| c.is_ascii_digit()) && !p[idx + 1..].is_empty() {
            p = &p[..idx];
        }
    }
    // Strip worktree prefix
    if let Some(idx) = p.find(".kuriboh/worktrees/reviewer-") {
        let after_reviewer = &p[idx + ".kuriboh/worktrees/reviewer-".len()..];
        if let Some(slash) = after_reviewer.find('/') {
            return after_reviewer[slash + 1..].to_string();
        }
    }
    // Strip leading path components to get project-relative path.
    if let Some(idx) = p.find("src/") {
        return p[idx..].to_string();
    }
    p.to_string()
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_line_numbers() {
        assert_eq!(normalize_file_path("src/foo.rs:42"), "src/foo.rs");
    }

    #[test]
    fn normalize_strips_worktree_prefix() {
        assert_eq!(
            normalize_file_path("/code/project/.kuriboh/worktrees/reviewer-1/src/lib.rs"),
            "src/lib.rs"
        );
    }

    #[test]
    fn normalize_strips_absolute_path() {
        assert_eq!(
            normalize_file_path("/home/user/project/src/main.rs"),
            "src/main.rs"
        );
    }

    #[test]
    fn normalize_preserves_relative() {
        assert_eq!(normalize_file_path("src/auth/mod.rs"), "src/auth/mod.rs");
    }

    #[test]
    fn infer_reviewer_from_worktree_path() {
        assert_eq!(
            infer_reviewer_id("/project/.kuriboh/worktrees/reviewer-3/src/lib.rs"),
            3
        );
    }

    #[test]
    fn infer_reviewer_default() {
        assert_eq!(infer_reviewer_id("src/lib.rs"), 0);
    }

    #[test]
    fn phase_progress_indeterminate() {
        let state = TuiState::new();
        assert!(state.phase_progress() < 0.0);
    }

    #[test]
    fn push_log_caps_entries() {
        let mut state = TuiState::new();
        for i in 0..150 {
            state.push_log("test", &format!("entry {i}"));
        }
        assert_eq!(state.log_entries.len(), MAX_LOG_ENTRIES);
    }
}

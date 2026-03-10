use std::path::PathBuf;

use clap::Parser;

/// A Claude Code harness for automated security reviews of Rust codebases.
///
/// kuriboh installs specialized subagent definitions into the target project,
/// spawns a `claude` agent team to perform the review, and emits a structured
/// security report.
#[derive(Debug, Parser)]
#[command(name = "kuriboh", version, about, long_about = None)]
pub struct Args {
    /// Path to the Rust codebase to review
    #[arg(short, long, value_name = "PATH")]
    pub target: PathBuf,

    /// Where to write the final security report (default: ./kuriboh-report.md)
    #[arg(short, long, value_name = "PATH", default_value = "kuriboh-report.md")]
    pub output: PathBuf,

    /// Path to a TOML config file for customizing agent definitions.
    ///
    /// Override built-in agent fields (description, tools, model, prompt) or
    /// define new custom subagents that reviewers can spawn. See README for
    /// the config format.
    #[arg(long, value_name = "PATH")]
    pub agents_config: Option<PathBuf>,

    /// Custom guidance for reviewer agents.
    ///
    /// Injected into the orchestration prompt to focus the review on specific
    /// areas or bug classes. Examples:
    ///   --prompt "Focus on the networking layer in src/net/"
    ///   --prompt "Look for TOCTOU race conditions and symlink attacks"
    #[arg(short, long, value_name = "TEXT")]
    pub prompt: Option<String>,

    /// Claude model for non-lead sessions (exploration, scouting, appraisal).
    ///
    /// The deep review lead always uses `claude-opus-4-6` regardless of this
    /// flag. This controls the model used for all other phases.
    #[arg(long, default_value = "claude-sonnet-4-6", value_name = "MODEL")]
    pub model: String,

    /// Number of reviewer agents to spawn in Phase 3 (Deep Review).
    ///
    /// Each reviewer starts from a high-scoring file and performs a
    /// frontier-based prioritized search with backtracking. More reviewers means
    /// better coverage but higher cost. Default: dynamically calculated as
    /// ceil(sqrt(total_scored_files)), clamped to [3, 12].
    #[arg(long, value_name = "N")]
    pub reviewers: Option<u32>,

    /// Maximum dollar amount to spend on API calls.
    ///
    /// Passed through to Claude Code as `--max-budget-usd`. The session
    /// will stop once this budget is exhausted.
    #[arg(long, value_name = "AMOUNT")]
    pub max_budget_usd: Option<f64>,

    /// Maximum number of turns the agent team is allowed to run.
    ///
    /// The 5-phase pipeline (explore → scout → deep review → appraisal →
    /// compilation) needs significant headroom. 400 is a safe default for
    /// medium codebases; large codebases may need 600+.
    #[arg(long, default_value_t = 400, value_name = "N")]
    pub max_turns: u32,

    /// Emit JSON output instead of Markdown
    #[arg(long)]
    pub json: bool,

    /// Pass `--dangerously-skip-permissions` to Claude Code.
    ///
    /// Disables all per-tool confirmation prompts in the inner Claude Code
    /// session. Only safe when Claude Code's native sandbox
    /// (bubblewrap/Seatbelt) is active, or when running inside a Docker AI
    /// Sandbox / container with its own isolation boundary.
    ///
    /// Without this flag, Claude Code will prompt for permission on each
    /// tool use, which requires an interactive terminal.
    #[arg(long)]
    pub dangerously_skip_permissions: bool,

    /// Print a cost estimate and exit without running the review.
    ///
    /// Scans the target codebase, counts files, computes the reviewer count,
    /// and prints an estimated cost breakdown by phase.
    #[arg(long)]
    pub estimate: bool,

    /// Show Claude Code's output in real time as the review progresses.
    ///
    /// Streams the agent's text to stderr so you can follow along with
    /// each phase. Also enables debug-level tracing logs.
    #[arg(short, long)]
    pub verbose: bool,

    /// Resume a previous run from `.kuriboh/state.json`.
    ///
    /// Skips phases that already completed successfully. Re-runs phases
    /// that were running or failed. Validates that --target matches the
    /// stored target path.
    #[arg(long)]
    pub resume: bool,

    /// Focus review on changes between two commits.
    ///
    /// Accepts git range syntax: base..head (e.g. main..feature,
    /// abc123..def456). Only .rs files changed in the range are scored
    /// and assigned to reviewers. Reviewers receive diff hunks to
    /// focus their analysis.
    #[arg(long, value_name = "RANGE")]
    pub diff: Option<String>,

    /// Seed for reproducible task assignments.
    ///
    /// Controls the weighted-random reviewer-to-file mapping in Phase 3.
    /// If omitted, a random seed is generated. Stored in state.json so
    /// `--resume` reuses the same seed.
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,

    /// Keep the `.kuriboh/` workspace directory after the run.
    ///
    /// Useful for debugging: inspect `exploration.md`, `scores.json`, and other
    /// intermediate artifacts produced during the phased review.
    #[arg(long)]
    pub keep_workspace: bool,
}

pub fn parse() -> Args {
    Args::parse()
}

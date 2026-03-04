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

    /// Override the set of agents to deploy (comma-separated agent names).
    /// Defaults to: unsafe-auditor,dep-checker,crypto-reviewer
    #[arg(long, value_delimiter = ',', value_name = "AGENTS")]
    pub agents: Vec<String>,

    /// Path to an optional agents config file (TOML) for customizing agent prompts
    #[arg(long, value_name = "PATH")]
    pub agents_config: Option<PathBuf>,

    /// Claude model to use for the orchestrating agent team lead
    #[arg(long, default_value = "claude-sonnet-4-6", value_name = "MODEL")]
    pub model: String,

    /// Number of reviewer agents to spawn in Phase 3 (Deep Review).
    ///
    /// Each reviewer starts from a randomly-selected file (weighted by scout
    /// score) and performs a depth-first security audit. More reviewers means
    /// better coverage but higher cost. Default: dynamically calculated as
    /// ceil(sqrt(total_scored_files)), clamped to [3, 12].
    #[arg(long, value_name = "N")]
    pub reviewers: Option<u32>,

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

    /// Disable the Docker AI Sandbox and run `claude` directly on the host.
    ///
    /// Only use this for local development. Production runs should always use
    /// the sandbox so that `--dangerously-skip-permissions` is safe.
    #[arg(long)]
    pub no_sandbox: bool,

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

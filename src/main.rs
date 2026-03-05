mod agents;
mod cli;
mod events;
mod report;
mod runner;
mod scanner;
mod state;
mod state;

use std::path::Path;

use anyhow::{bail, Result};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = cli::parse();

    let default_level = if args.verbose {
        "kuriboh=debug"
    } else {
        "kuriboh=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(default_level.parse()?),
        )
        .init();

    // Validate --target early (before expensive agent work).
    args.target = std::fs::canonicalize(&args.target)
        .map_err(|e| anyhow::anyhow!("--target {}: {e}", args.target.display()))?;
    if !args.target.is_dir() {
        bail!("--target {} is not a directory", args.target.display());
    }

    // Validate --output parent directory exists before committing to a long run.
    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            bail!(
                "--output parent directory does not exist: {}",
                parent.display()
            );
        }
    }

    if args.agents_config.is_some() {
        tracing::warn!("--agents-config is not yet implemented; ignoring");
    }

    if args.estimate {
        print_estimate(&args);
        return Ok(());
    }

    info!(target = %args.target.display(), "Starting kuriboh security review");

    // 1. Write subagent definitions into the target's .claude/agents/ directory.
    agents::install(&args.target, &args.agents_config)?;

    // 2. Spawn Claude Code and stream NDJSON events.
    let event_stream = runner::run(&args).await?;

    // 3. Parse events into a structured report and write it out.
    let report = report::parse(&event_stream)?;
    report::write(&report, &args.output, args.json)?;

    // 4. Clean up intermediate artifacts unless --keep-workspace.
    if !args.keep_workspace {
        agents::cleanup(&args.target)?;
    }

    info!(
        output = %args.output.display(),
        cost_usd = report.total_cost_usd,
        "Review complete"
    );
    Ok(())
}

/// Count `.rs` files under `dir`, excluding common non-production paths.
fn count_rs_files(dir: &Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                matches!(n, "target" | "vendor" | ".git" | ".kuriboh" | ".claude")
            });
            if !skip {
                count += count_rs_files(&path);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            count += 1;
        }
    }
    count
}

/// Compute the dynamic reviewer count: ceil(sqrt(n)) clamped to [3, 12].
fn default_reviewer_count(file_count: usize) -> u32 {
    ((file_count as f64).sqrt().ceil() as u32).clamp(3, 12)
}

fn print_estimate(args: &cli::Args) {
    let file_count = count_rs_files(&args.target);
    let reviewers = args
        .reviewers
        .unwrap_or_else(|| default_reviewer_count(file_count));

    // Empirical cost-per-agent estimates (based on kuriboh self-reviews).
    // These are rough — actual cost depends on file complexity and model.
    let cost_exploration = 0.15; // 1 Explore subagent
    let cost_scouting = file_count as f64 * 0.02; // Haiku per file
    let cost_per_reviewer = 1.80; // Sonnet DFS review
    let cost_per_appraiser = 0.60; // Sonnet validation
    let cost_compilation = 0.30; // Lead synthesis
    let cost_lead_overhead = 0.50; // Lead orchestration across phases

    let cost_review = reviewers as f64 * cost_per_reviewer;
    let cost_appraisal = reviewers as f64 * cost_per_appraiser;
    let total = cost_exploration
        + cost_scouting
        + cost_review
        + cost_appraisal
        + cost_compilation
        + cost_lead_overhead;

    println!("Kuriboh Cost Estimate");
    println!("=====================");
    println!();
    println!("Target:       {}", args.target.display());
    println!("Rust files:   {file_count}");
    println!("Model:        {}", args.model);
    println!("Reviewers:    {reviewers}");
    println!("Max turns:    {}", args.max_turns);
    if let Some(budget) = args.max_budget_usd {
        println!("Max budget:   ${budget:.2}");
    }
    println!();
    println!("Phase                  Est. Cost");
    println!("-----                  ---------");
    println!("1. Exploration         ${cost_exploration:.2}");
    println!("2. Scouting ({file_count} files) ${cost_scouting:.2}");
    println!("3. Deep Review ({reviewers}x)    ${cost_review:.2}");
    println!("4. Appraisal ({reviewers}x)      ${cost_appraisal:.2}");
    println!("5. Compilation         ${cost_compilation:.2}");
    println!("   Lead overhead       ${cost_lead_overhead:.2}");
    println!("                       ---------");
    println!("   Total               ${total:.2}");
    println!();
    println!("Note: estimates are approximate, based on empirical data from");
    println!("small-to-medium codebases. Actual cost depends on file complexity,");
    println!("model pricing, and how many turns each agent uses.");
}

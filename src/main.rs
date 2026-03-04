mod agents;
mod cli;
mod events;
mod report;
mod runner;
mod sandbox;

use anyhow::{bail, Result};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kuriboh=info".parse()?),
        )
        .init();

    let mut args = cli::parse();

    // Validate --target early (before expensive agent work).
    args.target = std::fs::canonicalize(&args.target).map_err(|e| {
        anyhow::anyhow!("--target {}: {e}", args.target.display())
    })?;
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

    info!(target = %args.target.display(), "Starting kuriboh security review");

    let sandbox = sandbox::SandboxConfig {
        enabled: !args.no_sandbox,
    };

    // 1. Write subagent definitions into the target's .claude/agents/ directory.
    agents::install(&args.target, &args.agents_config)?;

    // 2. Spawn Claude Code and stream NDJSON events.
    let event_stream = runner::run(&args, &sandbox).await?;

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

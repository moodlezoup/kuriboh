mod agents;
mod cli;
mod events;
mod report;
mod runner;
mod sandbox;

use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kuriboh=info".parse()?),
        )
        .init();

    let args = cli::parse();
    info!(target = %args.target.display(), "Starting kuriboh security review");

    let sandbox = sandbox::SandboxConfig {
        enabled: !args.no_sandbox,
    };

    // 1. Write subagent definitions into the target's .claude/agents/ directory.
    agents::install(&args.target, &args.agents_config)?;

    // 2. Spawn Claude Code (inside the Docker sandbox) and stream NDJSON events.
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

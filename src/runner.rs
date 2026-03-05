use std::io::Write;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::cli::Args;
use crate::events::{self, ClaudeEvent, ContentBlock};

/// Options for a single Claude Code session.
pub struct SessionOpts {
    pub prompt: String,
    /// Whether to enable agent teams for this session.
    pub agent_teams: bool,
}

/// Spawn a single Claude Code session, stream its NDJSON output, and return
/// the full sequence of parsed [`ClaudeEvent`]s.
pub async fn run_session(args: &Args, opts: &SessionOpts) -> Result<Vec<ClaudeEvent>> {
    let mut claude_args = Vec::new();
    if args.dangerously_skip_permissions {
        claude_args.push("--dangerously-skip-permissions".to_string());
    }
    if let Some(budget) = args.max_budget_usd {
        claude_args.extend(["--max-budget-usd".to_string(), budget.to_string()]);
    }
    claude_args.extend([
        "--model".to_string(),
        args.model.clone(),
        "--max-turns".to_string(),
        args.max_turns.to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ]);
    if opts.agent_teams {
        claude_args.extend(["--teammate-mode".to_string(), "in-process".to_string()]);
    }
    claude_args.extend(["-p".to_string(), opts.prompt.clone()]);

    let program = "claude";

    tracing::info!(
        %program,
        model = %args.model,
        max_turns = args.max_turns,
        agent_teams = opts.agent_teams,
        "Spawning Claude Code session"
    );
    tracing::debug!(
        cmd = %format!("{program} {}", claude_args.iter().map(|a| {
            if a.contains(' ') || a.contains('"') { format!("'{a}'") } else { a.clone() }
        }).collect::<Vec<_>>().join(" ")),
        "Full command"
    );

    let mut cmd = Command::new(program);
    cmd.args(&claude_args)
        .env_remove("CLAUDECODE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if opts.agent_teams {
        cmd.env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `{program}` — is it installed and on PATH?"))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();
    let mut collected: Vec<ClaudeEvent> = Vec::new();
    let mut stderr_buf = String::new();

    loop {
        tokio::select! {
            line = stdout_lines.next_line() => {
                match line.context("reading claude stdout")? {
                    None => break,
                    Some(l) => {
                        if let Some(ev) = events::parse_line(&l) {
                            if args.verbose {
                                print_event_text(&ev);
                            }
                            tracing::debug!(?ev, "event");
                            collected.push(ev);
                        }
                    }
                }
            }
            line = stderr_lines.next_line() => {
                if let Ok(Some(l)) = line {
                    tracing::debug!(stderr = %l);
                    stderr_buf.push_str(&l);
                    stderr_buf.push('\n');
                }
            }
        }
    }

    while let Ok(Some(l)) = stderr_lines.next_line().await {
        stderr_buf.push_str(&l);
        stderr_buf.push('\n');
    }

    let status = child.wait().await.context("waiting for claude to exit")?;
    if !status.success() {
        tracing::warn!(exit_code = status.code().unwrap_or(-1), "claude exited non-zero");
    }

    if collected.is_empty() {
        bail!("claude produced no events. Stderr:\n{stderr_buf}");
    }

    Ok(collected)
}

/// Print assistant text content to stderr for `--verbose` mode.
fn print_event_text(ev: &ClaudeEvent) {
    let blocks = match ev {
        ClaudeEvent::Assistant { message, .. } => &message.content,
        _ => return,
    };
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    for block in blocks {
        if let ContentBlock::Text { text } = block {
            let _ = lock.write_all(text.as_bytes());
            let _ = lock.flush();
        }
    }
}

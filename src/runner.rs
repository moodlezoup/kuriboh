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
    /// Override the model for this session. Falls back to `Args::model` if `None`.
    pub model: Option<String>,
}

/// Spawn a single Claude Code session, stream its NDJSON output, and return
/// the full sequence of parsed [`ClaudeEvent`]s.
pub async fn run_session(
    args: &Args,
    opts: &SessionOpts,
    tui_tx: Option<&tokio::sync::mpsc::UnboundedSender<crate::tui::TuiEvent>>,
) -> Result<Vec<ClaudeEvent>> {
    let mut claude_args = Vec::new();
    if args.dangerously_skip_permissions {
        claude_args.push("--dangerously-skip-permissions".to_string());
    }
    if let Some(budget) = args.max_budget_usd {
        claude_args.extend(["--max-budget-usd".to_string(), budget.to_string()]);
    }
    let model = opts.model.as_deref().unwrap_or(&args.model);
    claude_args.extend([
        "--model".to_string(),
        model.to_string(),
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
        %model,
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

    let stdout = child.stdout.take().context("stdout not piped")?;
    let stderr = child.stderr.take().context("stderr not piped")?;

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
                            if let Some(tx) = &tui_tx {
                                let _ = tx.send(crate::tui::TuiEvent::Claude(ev.clone()));
                            }
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

    // Extract error message from Result events (Claude Code reports app-level
    // errors here, e.g. invalid API key, model not found).
    let session_error = collected.iter().find_map(|ev| match ev {
        ClaudeEvent::Result {
            is_error: true,
            result,
            ..
        } => Some(result.clone()),
        _ => None,
    });

    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        if let Some(err_msg) = &session_error {
            bail!("claude exited with code {code}: {err_msg}");
        }
        let stderr_trimmed = stderr_buf.trim();
        if stderr_trimmed.is_empty() {
            bail!("claude exited with code {code} (no output on stderr or in events)");
        }
        bail!("claude exited with code {code}. Stderr:\n{stderr_trimmed}");
    }

    // Catch application errors that don't set a non-zero exit code.
    if let Some(err_msg) = session_error {
        bail!("claude session error: {err_msg}");
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

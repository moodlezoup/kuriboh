mod agents;
mod cli;
mod diff;
mod events;
mod prompts;
mod report;
mod runner;
mod scanner;
mod state;
mod tui;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use tracing::info;

use state::{PhaseStatus, State};

type TuiTx = Option<tokio::sync::mpsc::UnboundedSender<tui::TuiEvent>>;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = cli::parse();

    let default_level = if args.tui {
        "kuriboh=warn"
    } else if args.verbose {
        "kuriboh=debug"
    } else {
        "kuriboh=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(default_level.parse()?),
        )
        .init();

    args.target = std::fs::canonicalize(&args.target)
        .map_err(|e| anyhow::anyhow!("--target {}: {e}", args.target.display()))?;
    if !args.target.is_dir() {
        bail!("--target {} is not a directory", args.target.display());
    }

    // Default output goes under .kuriboh/ so cleanup handles it.
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| args.target.join(".kuriboh/kuriboh-report.md"));

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            bail!(
                "--output parent directory does not exist: {}",
                parent.display()
            );
        }
    }

    if args.estimate {
        print_estimate(&args);
        return Ok(());
    }

    if !args.dangerously_skip_permissions {
        bail!(
            "--dangerously-skip-permissions is required. Without it, inner Claude Code \
             sessions will prompt for tool permissions that nobody can approve.\n\
             This flag is safe when running inside a sandbox (Docker, bubblewrap, Seatbelt)."
        );
    }

    // Resolve diff context if --diff or --pr was provided.
    let diff_ctx = if let Some(ref range) = args.diff {
        let ctx = diff::resolve_diff(&args.target, range)?;
        info!(
            base = %ctx.base,
            head = %ctx.head,
            changed_files = ctx.files.len(),
            "Diff mode: reviewing changes"
        );
        Some(ctx)
    } else if let Some(ref pr_input) = args.pr {
        let ctx = diff::resolve_pr(&args.target, pr_input)?;
        info!(
            pr = pr_input,
            base = %ctx.base,
            head = %ctx.head,
            changed_files = ctx.files.len(),
            "PR mode: reviewing pull request changes"
        );
        Some(ctx)
    } else {
        None
    };

    info!(target = %args.target.display(), "Starting kuriboh security review");

    // Clean stale workspace from a prior run before starting fresh.
    // Must happen before agents::install(), which creates .kuriboh/.
    if !args.resume {
        let kb = args.target.join(".kuriboh");
        if kb.exists() {
            info!("Cleaning stale .kuriboh/ workspace from prior run");
            agents::cleanup(&args.target)?;
        }
    }

    // Install subagent definitions.
    agents::install(&args.target, &args.agents_config)?;

    // Load or create pipeline state.
    let mut state = if args.resume {
        let s = State::load(&args.target)?;
        if s.target != args.target {
            bail!(
                "--resume target mismatch: state has {}, got {}",
                s.target.display(),
                args.target.display()
            );
        }
        info!("Resuming from existing state");
        s
    } else {
        let seed = args.seed.unwrap_or_else(rand::random);
        let mut s = State::new(args.target.clone(), seed);
        if let Some(ref ctx) = diff_ctx {
            s.mode = state::ReviewMode::Diff {
                base: ctx.base.clone(),
                head: ctx.head.clone(),
                changed_files: ctx.files.clone(),
            };
        }
        s
    };

    // Validate --diff and --resume mode consistency.
    if args.resume {
        if let (state::ReviewMode::Full, Some(_)) = (&state.mode, &args.diff) {
            bail!("Cannot use --diff with --resume on a full-mode review. Start a new review with --diff instead.");
        }
    }

    // If resuming a diff review, re-derive hunks from git (not stored in state).
    let diff_ctx = if diff_ctx.is_none() {
        match &state.mode {
            state::ReviewMode::Diff { base, head, .. } => {
                let range = format!("{base}..{head}");
                Some(diff::resolve_diff(&args.target, &range)?)
            }
            state::ReviewMode::Full => None,
        }
    } else {
        diff_ctx
    };

    // Save initial state so --resume can find it even if we crash in phase 1.
    state.save(&args.target)?;

    // Spawn TUI if requested.
    let (tui_tx, tui_handle): (TuiTx, Option<tokio::task::JoinHandle<()>>) = if args.tui {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let workspace = args.target.join(".kuriboh");
        let tui_app = tui::TuiApp::new(rx, workspace);
        let handle = tokio::spawn(async move {
            if let Err(e) = tui_app.run().await {
                tracing::warn!(err = %e, "TUI exited with error");
            }
        });
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    // === Phase 1: Exploration ===
    run_phase(&mut state, &args, "exploration", &diff_ctx, &tui_tx).await?;

    // === Phase 2: Scouting ===
    run_phase(&mut state, &args, "scouting", &diff_ctx, &tui_tx).await?;

    // === Phase 3: Deep Review ===
    run_phase(&mut state, &args, "deep_review", &diff_ctx, &tui_tx).await?;

    // Semantic dedup: use a cheap LLM to identify duplicate findings across reviewers.
    match semantic_dedup(&args, &tui_tx).await {
        Ok((before, after)) if before > 0 => {
            let removed = before - after;
            info!(before, after, removed, "Semantically deduplicated findings");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(err = %e, "Semantic dedup failed, continuing with raw findings");
        }
    }

    // === Phase 4: Appraisal (only reviewers with findings) ===
    run_phase(
        &mut state,
        &args,
        "appraisal_compilation",
        &diff_ctx,
        &tui_tx,
    )
    .await?;

    // === Phase 5: Compilation (Rust, no Claude) ===
    let compiled_count = report::compile_findings(&args.target)?;
    info!(compiled_count, "Compiled findings from appraised files");

    // === Report Generation (Rust, no Claude) ===
    let report = report::parse_from_workspace(&args.target)?;
    report::write(&report, &output, args.json)?;

    // Send report to TUI and wait for user to dismiss with :q.
    if let Some(tx) = &tui_tx {
        let report_content = std::fs::read_to_string(&output).unwrap_or_default();
        let _ = tx.send(tui::TuiEvent::ReportReady {
            content: report_content,
        });
        let _ = tx.send(tui::TuiEvent::Shutdown);
    }
    // Wait for TUI task to finish (user presses :q).
    if let Some(handle) = tui_handle {
        let _ = handle.await;
    }

    if args.keep_workspace {
        info!(
            output = %output.display(),
            cost_usd = report.total_cost_usd,
            "Review complete"
        );
    } else {
        // Move the report out of .kuriboh/ before cleanup deletes it.
        let final_output = if output.starts_with(args.target.join(".kuriboh")) {
            let dest = args.target.join(
                output
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("kuriboh-report.md")),
            );
            std::fs::copy(&output, &dest)
                .with_context(|| format!("copying report to {}", dest.display()))?;
            dest
        } else {
            output.clone()
        };
        agents::cleanup(&args.target)?;
        info!(
            output = %final_output.display(),
            cost_usd = report.total_cost_usd,
            "Review complete"
        );
    }
    Ok(())
}

/// Run a single phase with sentinel checking and state management.
async fn run_phase(
    state: &mut State,
    args: &cli::Args,
    phase_name: &str,
    diff_ctx: &Option<diff::DiffContext>,
    tui_tx: &TuiTx,
) -> Result<()> {
    // Check if already done and sentinel still valid.
    if *state.phase_status(phase_name) == PhaseStatus::Done {
        if state::check_sentinel(&args.target, phase_name, state)? {
            info!(phase = phase_name, "Phase already complete, skipping");
            return Ok(());
        }
        tracing::warn!(
            phase = phase_name,
            "Phase marked done but sentinel failed, re-running"
        );
    }

    if let Some(tx) = tui_tx {
        let _ = tx.send(tui::TuiEvent::PhaseStart {
            name: phase_name.to_string(),
        });
    }

    info!(phase = phase_name, "Starting phase");
    state.phase_mut(phase_name).status = PhaseStatus::Running;
    state.save(&args.target)?;

    let result = match phase_name {
        "exploration" => run_exploration(state, args, diff_ctx, tui_tx).await,
        "scouting" => run_scouting(state, args, tui_tx).await,
        "deep_review" => run_deep_review(state, args, diff_ctx, tui_tx).await,
        "appraisal_compilation" => run_appraisal_compilation(state, args, tui_tx).await,
        _ => bail!("Unknown phase: {phase_name}"),
    };

    match result {
        Ok(()) => {
            if state::check_sentinel(&args.target, phase_name, state)? {
                state.phase_mut(phase_name).status = PhaseStatus::Done;
                let cost = state.phase_mut(phase_name).cost_usd.unwrap_or(0.0);
                state.save(&args.target)?;
                if let Some(tx) = tui_tx {
                    let _ = tx.send(tui::TuiEvent::PhaseComplete {
                        name: phase_name.to_string(),
                        cost_usd: cost,
                    });
                }
                info!(phase = phase_name, "Phase complete");
                Ok(())
            } else {
                state.phase_mut(phase_name).status = PhaseStatus::Failed;
                state.phase_mut(phase_name).reason = Some("sentinel check failed".to_string());
                state.save(&args.target)?;
                bail!("Phase {phase_name} completed but sentinel check failed");
            }
        }
        Err(e) => {
            state.phase_mut(phase_name).status = PhaseStatus::Failed;
            state.phase_mut(phase_name).reason = Some(format!("{e:#}"));
            state.save(&args.target)?;
            Err(e).with_context(|| format!("Phase {phase_name} failed"))
        }
    }
}

/// Run semantic deduplication of findings using a cheap LLM model.
/// Returns (total_before, total_after).
async fn semantic_dedup(args: &cli::Args, tui_tx: &TuiTx) -> Result<(usize, usize)> {
    let (findings_json, all_findings) = report::collect_all_findings(&args.target)?;
    let total_before = all_findings.len();

    if total_before < 2 {
        return Ok((total_before, total_before));
    }

    let prompt = prompts::semantic_dedup(&findings_json);
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: false,
            model: Some("claude-haiku-4-5-20251001".to_string()),
        },
        tui_tx.as_ref(),
    )
    .await?;

    // Extract the LLM's text response.
    let response = events
        .iter()
        .find_map(|ev| match ev {
            events::ClaudeEvent::Result { result, .. } => Some(result.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let deduped = report::apply_dedup_groups(all_findings, &response);
    let total_after = deduped.len();

    // Write back deduplicated findings per reviewer.
    report::write_deduped_findings(&args.target, &deduped)?;

    Ok((total_before, total_after))
}

// --- Phase implementations ---

async fn run_exploration(
    state: &mut State,
    args: &cli::Args,
    diff_ctx: &Option<diff::DiffContext>,
    tui_tx: &TuiTx,
) -> Result<()> {
    let exploration_diff = diff_ctx
        .as_ref()
        .map(|ctx| prompts::ExplorationDiffContext {
            base: ctx.base.clone(),
            head: ctx.head.clone(),
            changed_files: ctx.files.clone(),
            commit_log: ctx.commit_log.clone(),
            pr_context: ctx.pr_context.clone(),
        });
    let prompt = prompts::exploration(
        &args.target.display().to_string(),
        args.prompt.as_deref(),
        exploration_diff.as_ref(),
    );
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: false,
            model: None,
        },
        tui_tx.as_ref(),
    )
    .await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("exploration").cost_usd = Some(cost);
    Ok(())
}

async fn run_scouting(state: &mut State, args: &cli::Args, tui_tx: &TuiTx) -> Result<()> {
    // 2a: Enumerate files and compute static metrics.
    let mut file_list = scanner::enumerate_files(&args.target, false)?;
    if file_list.len() > 300 {
        info!(
            total = file_list.len(),
            "Over 300 .rs files, filtering test files for scouting"
        );
        file_list.retain(|p| {
            !p.components()
                .any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "benches")))
                && !p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.ends_with("_test"))
        });
    }

    let file_strings: Vec<String> = file_list.iter().map(|p| p.display().to_string()).collect();
    // Store full file list (needed for exploration context even in diff mode).
    state.files.clone_from(&file_strings);

    // In diff mode, score only changed files; in full mode, score all.
    let files_to_score: Vec<String> = match &state.mode {
        state::ReviewMode::Diff { changed_files, .. } => {
            changed_files.iter().map(|f| f.path.clone()).collect()
        }
        state::ReviewMode::Full => file_strings.clone(),
    };

    let static_scores: Vec<(String, scanner::StaticMetrics)> = files_to_score
        .par_iter()
        .map(|file_path| {
            let full_path = args.target.join(file_path);
            let source = std::fs::read_to_string(&full_path).unwrap_or_else(|e| {
                tracing::warn!(file = %full_path.display(), err = %e, "Failed to read file, using empty source");
                String::new()
            });
            let metrics = scanner::compute_static_metrics(&source);
            (file_path.clone(), metrics)
        })
        .collect();

    // 2b: LLM metrics via scout subagents.
    let prompt = prompts::llm_scouting(&args.target.display().to_string(), &files_to_score);
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: false,
            model: None,
        },
        tui_tx.as_ref(),
    )
    .await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("scouting").cost_usd = Some(cost);

    // 2c: Merge scores.
    let llm_scores_path = args.target.join(".kuriboh/llm-scores.json");
    let llm_scores = scanner::load_llm_scores(&llm_scores_path).unwrap_or_default();

    let file_scores = scanner::merge_scores(&static_scores, &llm_scores);

    // Write scores.json
    let scores_json = serde_json::to_string_pretty(&file_scores)?;
    std::fs::write(args.target.join(".kuriboh/scores.json"), &scores_json)?;

    // Generate task assignments.
    let reviewer_count = args.reviewers.unwrap_or_else(|| match &state.mode {
        state::ReviewMode::Diff { .. } => scanner::default_reviewer_count_diff(file_scores.len()),
        state::ReviewMode::Full => scanner::default_reviewer_count(file_scores.len()),
    });
    state.reviewer_count = reviewer_count;
    let (assignments, reserve_count) =
        scanner::generate_assignments(&file_scores, reviewer_count, state.seed);
    state.task_assignments = assignments;
    state.reserve_count = reserve_count;

    let mandatory_count = state
        .task_assignments
        .iter()
        .filter(|a| a.mandatory)
        .count();
    info!(
        reviewer_count,
        mandatory_count, reserve_count, "Task assignments generated"
    );

    if let Some(tx) = tui_tx {
        let _ = tx.send(tui::TuiEvent::ScoresLoaded(file_scores));
        for a in &state.task_assignments {
            let _ = tx.send(tui::TuiEvent::ReviewerAssigned {
                id: a.reviewer_id,
                file: a.starting_file.clone(),
            });
        }
    }

    state.save(&args.target)?;

    Ok(())
}

async fn run_deep_review(
    state: &mut State,
    args: &cli::Args,
    diff_ctx: &Option<diff::DiffContext>,
    tui_tx: &TuiTx,
) -> Result<()> {
    // Prune stale worktree metadata from prior runs (e.g. --keep-workspace
    // leaves .git/worktrees/ references to deleted directories, which blocks
    // branch reuse even with -B).
    let _ = std::process::Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(&args.target)
        .output();

    // In diff mode, check out the head ref in worktrees (detached).
    let diff_head_ref = match &state.mode {
        state::ReviewMode::Diff { head, .. } => Some(head.clone()),
        state::ReviewMode::Full => None,
    };

    // Create git worktrees and PoC dirs in parallel.
    let wt_results: Vec<Result<()>> = state
        .task_assignments
        .par_iter()
        .map(|a| {
            let wt_path = args
                .target
                .join(format!(".kuriboh/worktrees/reviewer-{}", a.reviewer_id));
            // Remove existing worktree (from --keep-workspace or partial prior run).
            if wt_path.exists() {
                let _ = std::process::Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(&wt_path)
                    .current_dir(&args.target)
                    .output();
            }
            let output = if let Some(ref head_ref) = diff_head_ref {
                std::process::Command::new("git")
                    .args(["worktree", "add", "--detach"])
                    .arg(&wt_path)
                    .arg(head_ref)
                    .current_dir(&args.target)
                    .output()?
            } else {
                std::process::Command::new("git")
                    .args(["worktree", "add"])
                    .arg(&wt_path)
                    .arg("-B")
                    .arg(format!("kuriboh-review-{}", a.reviewer_id))
                    .current_dir(&args.target)
                    .output()?
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!(
                    "git worktree add failed for reviewer {}: {stderr}",
                    a.reviewer_id
                );
            }
            let poc_dir = args
                .target
                .join(format!(".kuriboh/pocs/reviewer-{}", a.reviewer_id));
            std::fs::create_dir_all(&poc_dir)?;
            Ok(())
        })
        .collect();
    for result in wt_results {
        result?;
    }

    let diff_info = diff_ctx.as_ref().map(|ctx| prompts::DiffPromptInfo {
        base: ctx.base.clone(),
        head: ctx.head.clone(),
        changed_files: ctx.files.clone(),
        hunks: ctx.hunks.clone(),
    });

    let prompt = prompts::deep_review(
        &state.task_assignments,
        &args.target.display().to_string(),
        args.max_turns,
        args.prompt.as_deref(),
        diff_info.as_ref(),
    );
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: true,
            model: Some("claude-opus-4-6".to_string()),
        },
        tui_tx.as_ref(),
    )
    .await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("deep_review").cost_usd = Some(cost);

    // Safety net: write `[]` to any missing reserve findings files.
    for a in state.task_assignments.iter().filter(|a| a.reserve) {
        let path = args
            .target
            .join(format!(".kuriboh/findings/reviewer-{}.json", a.reviewer_id));
        if !path.exists() {
            tracing::warn!(
                reviewer_id = a.reviewer_id,
                "Reserve reviewer findings missing, writing empty []"
            );
            std::fs::write(&path, "[]")?;
        }
    }

    Ok(())
}

async fn run_appraisal_compilation(
    state: &mut State,
    args: &cli::Args,
    tui_tx: &TuiTx,
) -> Result<()> {
    let all_ids: Vec<u32> = state
        .task_assignments
        .iter()
        .map(|a| a.reviewer_id)
        .collect();
    let non_empty_ids = report::reviewers_with_findings(&args.target, &all_ids);

    if non_empty_ids.is_empty() {
        info!("No reviewers produced findings, skipping appraisal");
        // Write empty compiled-findings.json so sentinel passes.
        std::fs::write(args.target.join(".kuriboh/compiled-findings.json"), "[]")?;
        state.phase_mut("appraisal_compilation").cost_usd = Some(0.0);
        return Ok(());
    }

    info!(
        total = all_ids.len(),
        with_findings = non_empty_ids.len(),
        "Appraising reviewers with findings"
    );

    let prompt = prompts::appraisal(
        &non_empty_ids,
        &args.target.display().to_string(),
        args.max_turns,
    );
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: false,
            model: None,
        },
        tui_tx.as_ref(),
    )
    .await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("appraisal_compilation").cost_usd = Some(cost);
    Ok(())
}

#[expect(clippy::print_stdout)]
fn print_estimate(args: &cli::Args) {
    let diff_result = args
        .diff
        .as_ref()
        .map(|range| {
            (
                diff::resolve_diff(&args.target, range),
                format!("diff ({range})"),
            )
        })
        .or_else(|| {
            args.pr
                .as_ref()
                .map(|pr| (diff::resolve_pr(&args.target, pr), format!("pr ({pr})")))
        });

    let (file_count, reviewers, mode_label) = if let Some((result, label)) = diff_result {
        match result {
            Ok(ctx) => {
                let count = ctx.files.len();
                let r = args
                    .reviewers
                    .unwrap_or_else(|| scanner::default_reviewer_count_diff(count));
                (count, r, label)
            }
            Err(e) => {
                println!("Error: {e}");
                return;
            }
        }
    } else {
        let file_list = scanner::enumerate_files(&args.target, false).unwrap_or_default();
        let count = file_list.len();
        let r = args
            .reviewers
            .unwrap_or_else(|| scanner::default_reviewer_count(count));
        (count, r, "full codebase".to_string())
    };

    let reserves = scanner::compute_reserve_count(reviewers);
    let total_reviewers = reviewers + reserves;

    let cost_exploration = 0.15;
    let cost_scouting = file_count as f64 * 0.01; // cheaper: only 3 LLM metrics
    let cost_per_reviewer = 1.80;
    let cost_per_appraiser = 0.60;
    let cost_compilation = 0.30;
    let cost_lead_overhead = 0.50;

    let cost_review = f64::from(total_reviewers) * cost_per_reviewer;
    let cost_appraisal = f64::from(total_reviewers) * cost_per_appraiser;
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
    println!("Mode:         {mode_label}");
    println!("Rust files:   {file_count}");
    println!("Model:        {} (lead: claude-opus-4-6)", args.model);
    println!("Reviewers:    {reviewers} (+{reserves} reserve)");
    println!("Max turns:    {}", args.max_turns);
    if let Some(budget) = args.max_budget_usd {
        println!("Max budget:   ${budget:.2}");
    }
    println!();
    println!("Phase                  Est. Cost");
    println!("-----                  ---------");
    println!("1. Exploration         ${cost_exploration:.2}");
    println!("2. Scouting ({file_count} files) ${cost_scouting:.2}");
    println!("3. Deep Review ({reviewers}+{reserves}x)  ${cost_review:.2}");
    println!("4. Appraisal ({reviewers}+{reserves}x)    ${cost_appraisal:.2}");
    println!("5. Compilation         ${cost_compilation:.2}");
    println!("   Lead overhead       ${cost_lead_overhead:.2}");
    println!("                       ---------");
    println!("   Total               ${total:.2}");
    println!();
    println!("Note: estimates are approximate. Reserve reviewers may not all");
    println!("be used — the lead spawns them adaptively based on findings.");
}

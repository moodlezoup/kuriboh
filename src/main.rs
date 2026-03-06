mod agents;
mod cli;
mod events;
mod prompts;
mod report;
mod runner;
mod scanner;
mod state;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use tracing::info;

use state::{PhaseStatus, State};

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

    args.target = std::fs::canonicalize(&args.target)
        .map_err(|e| anyhow::anyhow!("--target {}: {e}", args.target.display()))?;
    if !args.target.is_dir() {
        bail!("--target {} is not a directory", args.target.display());
    }

    if let Some(parent) = args.output.parent() {
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

    info!(target = %args.target.display(), "Starting kuriboh security review");

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
        State::new(args.target.clone(), seed)
    };

    // Save initial state so --resume can find it even if we crash in phase 1.
    state.save(&args.target)?;

    // === Phase 1: Exploration ===
    run_phase(&mut state, &args, "exploration").await?;

    // === Phase 2: Scouting ===
    run_phase(&mut state, &args, "scouting").await?;

    // === Phase 3: Deep Review ===
    run_phase(&mut state, &args, "deep_review").await?;

    // Pre-deduplicate findings across reviewers (Rust-side, before appraisal).
    match report::pre_deduplicate_findings(&args.target) {
        Ok((before, after)) if before > 0 => {
            let removed = before - after;
            info!(
                before,
                after, removed, "Pre-deduplicated findings across reviewers"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(err = %e, "Pre-dedup failed, continuing with raw findings");
        }
    }

    // === Phase 4+5: Appraisal & Compilation ===
    run_phase(&mut state, &args, "appraisal_compilation").await?;

    // === Report Generation (Rust, no Claude) ===
    let report = report::parse_from_workspace(&args.target)?;
    report::write(&report, &args.output, args.json)?;

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

/// Run a single phase with sentinel checking and state management.
async fn run_phase(state: &mut State, args: &cli::Args, phase_name: &str) -> Result<()> {
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

    info!(phase = phase_name, "Starting phase");
    state.phase_mut(phase_name).status = PhaseStatus::Running;
    state.save(&args.target)?;

    let result = match phase_name {
        "exploration" => run_exploration(state, args).await,
        "scouting" => run_scouting(state, args).await,
        "deep_review" => run_deep_review(state, args).await,
        "appraisal_compilation" => run_appraisal_compilation(state, args).await,
        _ => bail!("Unknown phase: {phase_name}"),
    };

    match result {
        Ok(()) => {
            if state::check_sentinel(&args.target, phase_name, state)? {
                state.phase_mut(phase_name).status = PhaseStatus::Done;
                state.save(&args.target)?;
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

// --- Phase implementations ---

async fn run_exploration(state: &mut State, args: &cli::Args) -> Result<()> {
    let prompt = prompts::exploration(&args.target.display().to_string(), args.prompt.as_deref());
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: false,
            model: None,
        },
    )
    .await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("exploration").cost_usd = Some(cost);
    Ok(())
}

async fn run_scouting(state: &mut State, args: &cli::Args) -> Result<()> {
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
    state.files.clone_from(&file_strings);

    let static_scores: Vec<(String, scanner::StaticMetrics)> = file_list
        .par_iter()
        .map(|file_path| {
            let full_path = args.target.join(file_path);
            let source = std::fs::read_to_string(&full_path).unwrap_or_else(|e| {
                tracing::warn!(file = %full_path.display(), err = %e, "Failed to read file, using empty source");
                String::new()
            });
            let metrics = scanner::compute_static_metrics(&source);
            (file_path.display().to_string(), metrics)
        })
        .collect();

    // 2b: LLM metrics via scout subagents.
    let prompt = prompts::llm_scouting(&args.target.display().to_string(), &file_strings);
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: false,
            model: None,
        },
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
    let reviewer_count = args
        .reviewers
        .unwrap_or_else(|| scanner::default_reviewer_count(file_scores.len()));
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

    state.save(&args.target)?;

    Ok(())
}

async fn run_deep_review(state: &mut State, args: &cli::Args) -> Result<()> {
    // Prune stale worktree metadata from prior runs (e.g. --keep-workspace
    // leaves .git/worktrees/ references to deleted directories, which blocks
    // branch reuse even with -B).
    let _ = std::process::Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(&args.target)
        .output();

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
            let output = std::process::Command::new("git")
                .args(["worktree", "add"])
                .arg(&wt_path)
                .arg("-B")
                .arg(format!("kuriboh-review-{}", a.reviewer_id))
                .current_dir(&args.target)
                .output()?;
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

    let prompt = prompts::deep_review(
        &state.task_assignments,
        &args.target.display().to_string(),
        args.max_turns,
        args.prompt.as_deref(),
    );
    let events = runner::run_session(
        args,
        &runner::SessionOpts {
            prompt,
            agent_teams: true,
            model: Some("claude-opus-4-6".to_string()),
        },
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

async fn run_appraisal_compilation(state: &mut State, args: &cli::Args) -> Result<()> {
    let reviewer_ids: Vec<u32> = state
        .task_assignments
        .iter()
        .map(|a| a.reviewer_id)
        .collect();
    let prompt = prompts::appraisal_and_compilation(
        &reviewer_ids,
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
    )
    .await?;
    let cost = events::total_cost_usd(&events);
    state.phase_mut("appraisal_compilation").cost_usd = Some(cost);
    Ok(())
}

#[expect(clippy::print_stdout)]
fn print_estimate(args: &cli::Args) {
    let file_list = scanner::enumerate_files(&args.target, false).unwrap_or_default();
    let file_count = file_list.len();
    let reviewers = args
        .reviewers
        .unwrap_or_else(|| scanner::default_reviewer_count(file_count));
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

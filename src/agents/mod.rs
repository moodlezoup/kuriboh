/// Subagent template files embedded at compile time.
mod templates;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// An agent definition that will be written to `.claude/agents/<name>.md`.
pub struct AgentDef {
    pub name: &'static str,
    pub content: &'static str,
}

/// All built-in agents, keyed by name.
pub const BUILTIN_AGENTS: &[AgentDef] = &[
    AgentDef {
        name: "unsafe-auditor",
        content: templates::UNSAFE_AUDITOR,
    },
    AgentDef {
        name: "dep-checker",
        content: templates::DEP_CHECKER,
    },
    AgentDef {
        name: "crypto-reviewer",
        content: templates::CRYPTO_REVIEWER,
    },
    AgentDef {
        name: "scout",
        content: templates::SCOUT,
    },
    AgentDef {
        name: "appraiser",
        content: templates::APPRAISER,
    },
];

/// Installs subagent definition files into `<target>/.claude/agents/`.
///
/// This is called before spawning `claude` so that Claude Code can discover
/// and delegate to the specialized reviewers during the agent team run.
///
/// If `config` is provided, agent prompts may be overridden from the config
/// file. Otherwise the embedded templates are used verbatim.
pub fn install(target: &Path, config: &Option<PathBuf>) -> Result<()> {
    let agents_dir = target.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("creating {}", agents_dir.display()))?;

    // Create the .kuriboh/ workspace for intermediate review artifacts
    // (exploration.md, scores.json, etc.). Phases write here; other agents read.
    let kuriboh_dir = target.join(".kuriboh");
    std::fs::create_dir_all(&kuriboh_dir)
        .with_context(|| format!("creating {}", kuriboh_dir.display()))?;
    for subdir in ["findings", "worktrees", "pocs"] {
        let sub = kuriboh_dir.join(subdir);
        std::fs::create_dir_all(&sub).with_context(|| format!("creating {}", sub.display()))?;
    }
    tracing::debug!(path = %kuriboh_dir.display(), "Created .kuriboh workspace");

    for agent in BUILTIN_AGENTS {
        let dest = agents_dir.join(format!("{}.md", agent.name));
        // TODO: merge overrides from config if present
        let _ = config; // suppress unused warning until implemented
        std::fs::write(&dest, agent.content)
            .with_context(|| format!("writing agent def {}", dest.display()))?;
        tracing::debug!(agent = agent.name, path = %dest.display(), "Installed agent");
    }

    Ok(())
}

/// Remove the `.kuriboh/` workspace directory after a completed run.
///
/// Skipped when `--keep-workspace` is set, so users can inspect intermediate
/// artifacts like `exploration.md`, `scores.json`, worktrees, and PoCs.
///
/// Git worktrees must be properly removed before deleting the directory,
/// otherwise `.git/worktrees/` will retain dangling metadata.
pub fn cleanup(target: &Path) -> Result<()> {
    let kuriboh_dir = target.join(".kuriboh");
    if !kuriboh_dir.exists() {
        return Ok(());
    }

    // Remove any git worktrees created during the review.
    let worktrees_dir = kuriboh_dir.join("worktrees");
    if worktrees_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&worktrees_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    match std::process::Command::new("git")
                        .args(["worktree", "remove", "--force"])
                        .arg(&path)
                        .current_dir(target)
                        .output()
                    {
                        Ok(o) if o.status.success() => {
                            tracing::debug!(path = %path.display(), "Removed git worktree");
                        }
                        Ok(o) => {
                            tracing::warn!(
                                path = %path.display(),
                                stderr = %String::from_utf8_lossy(&o.stderr),
                                "Failed to remove git worktree cleanly"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                err = %e,
                                "git not found, skipping worktree removal"
                            );
                        }
                    }
                }
            }
        }
        // Prune any orphaned worktree references.
        let _ = std::process::Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(target)
            .output();
    }

    std::fs::remove_dir_all(&kuriboh_dir)
        .with_context(|| format!("cleaning up {}", kuriboh_dir.display()))?;
    tracing::debug!(path = %kuriboh_dir.display(), "Cleaned up .kuriboh workspace");

    Ok(())
}

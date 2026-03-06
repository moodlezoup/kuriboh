/// Subagent template files embedded at compile time.
mod templates;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// A structured agent definition rendered to `.kuriboh/agents/kuriboh_<name>.md`
/// and symlinked from `.claude/agents/kuriboh_<name>.md`.
#[derive(Clone, Debug)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub tools: String,
    /// Tools to explicitly deny (defense-in-depth, removed from inherited set).
    pub disallowed_tools: Option<String>,
    pub model: String,
    pub background: bool,
    /// Maximum agentic turns before the subagent stops.
    pub max_turns: Option<u32>,
    /// Permission mode: "default", "acceptEdits", "dontAsk", "bypassPermissions", "plan".
    pub permission_mode: Option<String>,
    pub prompt: String,
}

impl AgentDef {
    /// Render the agent definition as a `.md` file with YAML frontmatter.
    fn render(&self) -> String {
        let mut out = String::from("---\n");
        out.push_str(&format!("name: {}\n", self.name));
        out.push_str(&format!("description: >\n  {}\n", self.description));
        out.push_str(&format!("tools: {}\n", self.tools));
        if let Some(dt) = &self.disallowed_tools {
            out.push_str(&format!("disallowedTools: {dt}\n"));
        }
        out.push_str(&format!("model: {}\n", self.model));
        if self.background {
            out.push_str("background: true\n");
        }
        if let Some(mt) = self.max_turns {
            out.push_str(&format!("maxTurns: {mt}\n"));
        }
        if let Some(pm) = &self.permission_mode {
            out.push_str(&format!("permissionMode: {pm}\n"));
        }
        out.push_str("---\n\n");
        out.push_str(&self.prompt);
        if !self.prompt.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

/// Per-agent overrides in the TOML config file.
#[derive(Debug, Deserialize)]
struct AgentOverride {
    description: Option<String>,
    tools: Option<String>,
    disallowed_tools: Option<String>,
    model: Option<String>,
    background: Option<bool>,
    max_turns: Option<u32>,
    permission_mode: Option<String>,
    prompt: Option<String>,
}

/// Top-level TOML config structure for `--agents-config`.
#[derive(Debug, Deserialize)]
struct AgentsConfig {
    #[serde(default)]
    agents: HashMap<String, AgentOverride>,
}

/// Loads the agents config TOML file and returns the parsed config.
fn load_config(path: &Path) -> Result<AgentsConfig> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("reading agents config {}", path.display()))?;
    toml::from_str(&data).with_context(|| format!("parsing agents config {}", path.display()))
}

/// Applies overrides from the config to the built-in agents, and adds any new
/// custom agents defined in the config.
fn apply_config(agents: &mut Vec<AgentDef>, config: AgentsConfig) -> Result<()> {
    let builtin_names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();

    for (name, overrides) in config.agents {
        if let Some(agent) = agents.iter_mut().find(|a| a.name == name) {
            // Override fields on a built-in agent.
            if let Some(desc) = overrides.description {
                agent.description = desc;
            }
            if let Some(tools) = overrides.tools {
                agent.tools = tools;
            }
            if overrides.disallowed_tools.is_some() {
                agent.disallowed_tools = overrides.disallowed_tools;
            }
            if let Some(model) = overrides.model {
                agent.model = model;
            }
            if let Some(bg) = overrides.background {
                agent.background = bg;
            }
            if overrides.max_turns.is_some() {
                agent.max_turns = overrides.max_turns;
            }
            if overrides.permission_mode.is_some() {
                agent.permission_mode = overrides.permission_mode;
            }
            if let Some(prompt) = overrides.prompt {
                agent.prompt = prompt;
            }
        } else {
            // New custom agent — validate required fields.
            let desc = overrides
                .description
                .with_context(|| format!("custom agent '{name}' requires a 'description' field"))?;
            let prompt = overrides
                .prompt
                .with_context(|| format!("custom agent '{name}' requires a 'prompt' field"))?;

            if builtin_names.contains(&name) {
                bail!("agent name '{name}' conflicts with a built-in agent");
            }

            agents.push(AgentDef {
                name,
                description: desc,
                tools: overrides.tools.unwrap_or_else(|| "Read, Glob, Grep".into()),
                disallowed_tools: overrides.disallowed_tools,
                model: overrides.model.unwrap_or_else(|| "sonnet".into()),
                background: overrides.background.unwrap_or(false),
                max_turns: overrides.max_turns,
                permission_mode: overrides.permission_mode,
                prompt,
            });
        }
    }

    Ok(())
}

/// Installs agent definitions for Claude Code to discover.
///
/// Real files are written to `.kuriboh/agents/`, and symlinks are created in
/// `.claude/agents/` pointing back to them. This keeps the target repo clean —
/// cleanup only needs to remove symlinks, and `.kuriboh/` deletion takes care
/// of the real files.
///
/// If `config` is provided, agent prompts may be overridden and custom agents
/// added from the TOML config file. Otherwise the embedded templates are used.
pub fn install(target: &Path, config: &Option<PathBuf>) -> Result<()> {
    // Create the .kuriboh/ workspace for intermediate review artifacts
    // (exploration.md, scores.json, etc.). Phases write here; other agents read.
    let kuriboh_dir = target.join(".kuriboh");
    std::fs::create_dir_all(&kuriboh_dir)
        .with_context(|| format!("creating {}", kuriboh_dir.display()))?;
    for subdir in ["agents", "findings", "worktrees", "pocs", "frontier"] {
        let sub = kuriboh_dir.join(subdir);
        std::fs::create_dir_all(&sub).with_context(|| format!("creating {}", sub.display()))?;
    }
    tracing::debug!(path = %kuriboh_dir.display(), "Created .kuriboh workspace");

    let mut agents = templates::builtin_agents();

    if let Some(config_path) = config {
        let cfg = load_config(config_path)?;
        let custom_count = cfg
            .agents
            .keys()
            .filter(|k| !agents.iter().any(|a| &a.name == *k))
            .count();
        apply_config(&mut agents, cfg)?;
        if custom_count > 0 {
            tracing::info!(count = custom_count, "Loaded custom agents from config");
        }
    }

    let real_agents_dir = kuriboh_dir.join("agents");
    let link_dir = target.join(".claude").join("agents");
    std::fs::create_dir_all(&link_dir)
        .with_context(|| format!("creating {}", link_dir.display()))?;

    for agent in &agents {
        let filename = format!("kuriboh_{}.md", agent.name);

        // Write the real file into .kuriboh/agents/.
        let real_path = real_agents_dir.join(&filename);
        std::fs::write(&real_path, agent.render())
            .with_context(|| format!("writing agent def {}", real_path.display()))?;

        // Symlink from .claude/agents/<name>.md → .kuriboh/agents/<name>.md.
        // Use a relative path so it works if the repo is moved.
        let link_path = link_dir.join(&filename);
        let relative_target = PathBuf::from("..")
            .join("..")
            .join(".kuriboh")
            .join("agents")
            .join(&filename);

        // Remove any stale symlink or file at the link path.
        if link_path.symlink_metadata().is_ok() {
            std::fs::remove_file(&link_path)
                .with_context(|| format!("removing stale {}", link_path.display()))?;
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(&relative_target, &link_path).with_context(|| {
            format!(
                "symlinking {} → {}",
                link_path.display(),
                relative_target.display()
            )
        })?;

        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&relative_target, &link_path).with_context(|| {
            format!(
                "symlinking {} → {}",
                link_path.display(),
                relative_target.display()
            )
        })?;

        tracing::debug!(agent = %agent.name, link = %link_path.display(), "Installed agent");
    }

    Ok(())
}

/// Remove the `.kuriboh/` workspace directory and symlinks from `.claude/agents/`
/// after a completed run.
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

    // Remove symlinks we created in .claude/agents/ before deleting .kuriboh/
    // (so the symlink targets still exist for identification).
    remove_agent_symlinks(target);

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

/// Remove symlinks in `.claude/agents/` that point into `.kuriboh/`.
///
/// Only removes symlinks — never deletes real files, so user-owned agents are
/// always preserved. Cleans up empty `.claude/agents/` and `.claude/` dirs.
fn remove_agent_symlinks(target: &Path) {
    let agents_dir = target.join(".claude").join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return;
    };

    let kuriboh_agents = target.join(".kuriboh").join("agents");

    for entry in entries.flatten() {
        let path = entry.path();
        // Only consider symlinks.
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        // Check that the symlink resolves into .kuriboh/agents/.
        let is_ours = std::fs::read_link(&path)
            .ok()
            .and_then(|link_target| {
                // Resolve the relative symlink against the directory containing it.
                let resolved = agents_dir.join(&link_target);
                std::fs::canonicalize(&resolved).ok()
            })
            .is_some_and(|canonical| canonical.starts_with(&kuriboh_agents));

        if is_ours && std::fs::remove_file(&path).is_ok() {
            tracing::debug!(path = %path.display(), "Removed agent symlink");
        }
    }

    // Remove .claude/agents/ and .claude/ if we left them empty.
    remove_dir_if_empty(&agents_dir);
    remove_dir_if_empty(&target.join(".claude"));
}

/// Remove a directory only if it exists and is empty.
fn remove_dir_if_empty(dir: &Path) {
    if let Ok(mut entries) = std::fs::read_dir(dir) {
        if entries.next().is_none() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    #[expect(clippy::wildcard_imports)]
    use super::*;

    #[test]
    fn builtin_agents_render_valid_frontmatter() {
        for agent in templates::builtin_agents() {
            let rendered = agent.render();
            assert!(
                rendered.starts_with("---\n"),
                "agent {} missing frontmatter",
                agent.name
            );
            assert!(
                rendered.contains(&format!("name: {}", agent.name)),
                "agent {} missing name in frontmatter",
                agent.name
            );
            assert!(
                rendered.contains("---\n\n"),
                "agent {} missing frontmatter end",
                agent.name
            );
        }
    }

    #[test]
    fn apply_config_overrides_builtin() {
        let mut agents = templates::builtin_agents();
        let config: AgentsConfig = toml::from_str(
            r#"
            [agents.unsafe-auditor]
            model = "opus"
            tools = "Read, Glob, Grep, Bash"
            "#,
        )
        .unwrap();

        apply_config(&mut agents, config).unwrap();

        let ua = agents.iter().find(|a| a.name == "unsafe-auditor").unwrap();
        assert_eq!(ua.model, "opus");
        assert_eq!(ua.tools, "Read, Glob, Grep, Bash");
        // Prompt should be unchanged.
        assert!(ua.prompt.contains("memory-safety auditor"));
    }

    #[test]
    fn apply_config_adds_custom_agent() {
        let mut agents = templates::builtin_agents();
        let original_count = agents.len();
        let config: AgentsConfig = toml::from_str(
            r#"
            [agents.api-reviewer]
            description = "Reviews REST API endpoints"
            prompt = "You are an API reviewer."
            model = "haiku"
            "#,
        )
        .unwrap();

        apply_config(&mut agents, config).unwrap();

        assert_eq!(agents.len(), original_count + 1);
        let custom = agents.iter().find(|a| a.name == "api-reviewer").unwrap();
        assert_eq!(custom.description, "Reviews REST API endpoints");
        assert_eq!(custom.model, "haiku");
        assert_eq!(custom.tools, "Read, Glob, Grep"); // default
        assert!(!custom.background); // default
    }

    #[test]
    fn apply_config_rejects_custom_without_prompt() {
        let mut agents = templates::builtin_agents();
        let config: AgentsConfig = toml::from_str(
            r#"
            [agents.bad-agent]
            description = "Missing prompt"
            "#,
        )
        .unwrap();

        let err = apply_config(&mut agents, config).unwrap_err();
        assert!(err.to_string().contains("requires a 'prompt' field"));
    }

    #[test]
    fn install_creates_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path();

        install(target, &None).unwrap();

        let agents_dir = target.join(".claude/agents");
        let kuriboh_agents = target.join(".kuriboh/agents");

        // Every built-in agent should have a symlink in .claude/agents/
        // and a real file in .kuriboh/agents/.
        for agent in templates::builtin_agents() {
            let link = agents_dir.join(format!("kuriboh_{}.md", agent.name));
            let real = kuriboh_agents.join(format!("kuriboh_{}.md", agent.name));

            assert!(real.exists(), "real file missing for {}", agent.name);
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "{} should be a symlink",
                agent.name
            );
            // The symlink should resolve to the real file.
            let resolved = std::fs::canonicalize(&link).unwrap();
            let expected = std::fs::canonicalize(&real).unwrap();
            assert_eq!(resolved, expected);
        }
    }

    #[test]
    fn cleanup_removes_symlinks_preserves_user_agents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path();

        install(target, &None).unwrap();

        // Add a user-owned (non-symlink) agent file.
        let agents_dir = target.join(".claude/agents");
        std::fs::write(
            agents_dir.join("my-custom.md"),
            "---\nname: my-custom\n---\n",
        )
        .unwrap();

        cleanup(target).unwrap();

        // .kuriboh/ should be gone.
        assert!(!target.join(".kuriboh").exists());
        // All symlinks should be gone.
        for agent in templates::builtin_agents() {
            assert!(
                !agents_dir
                    .join(format!("kuriboh_{}.md", agent.name))
                    .exists(),
                "symlink for {} should be removed",
                agent.name
            );
        }
        // User's real file should still be there.
        assert!(agents_dir.join("my-custom.md").exists());
        // .claude/agents/ should NOT be removed (still has user's file).
        assert!(agents_dir.exists());
    }

    #[test]
    fn cleanup_removes_empty_claude_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path();

        install(target, &None).unwrap();
        cleanup(target).unwrap();

        // Both .claude/agents/ and .claude/ should be removed (empty).
        assert!(!target.join(".claude/agents").exists());
        assert!(!target.join(".claude").exists());
    }

    #[test]
    fn cleanup_tolerates_no_kuriboh_dir() {
        let dir = tempfile::tempdir().unwrap();
        cleanup(dir.path()).unwrap();
    }

    #[test]
    fn install_replaces_stale_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path();

        // First install.
        install(target, &None).unwrap();
        // Second install (simulating --resume re-running install).
        install(target, &None).unwrap();

        // Should still work — no "file exists" errors.
        let link = target.join(".claude/agents/kuriboh_scout.md");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn apply_config_rejects_custom_without_description() {
        let mut agents = templates::builtin_agents();
        let config: AgentsConfig = toml::from_str(
            r#"
            [agents.bad-agent]
            prompt = "You do stuff."
            "#,
        )
        .unwrap();

        let err = apply_config(&mut agents, config).unwrap_err();
        assert!(err.to_string().contains("requires a 'description' field"));
    }
}

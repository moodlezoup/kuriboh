/// Subagent template files embedded at compile time.
mod templates;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// A structured agent definition that will be rendered to `.claude/agents/<name>.md`.
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

/// Installs subagent definition files into `<target>/.claude/agents/`.
///
/// This is called before spawning `claude` so that Claude Code can discover
/// and delegate to the specialized reviewers during the agent team run.
///
/// If `config` is provided, agent prompts may be overridden and custom agents
/// added from the TOML config file. Otherwise the embedded templates are used.
pub fn install(target: &Path, config: &Option<PathBuf>) -> Result<()> {
    let agents_dir = target.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("creating {}", agents_dir.display()))?;

    // Create the .kuriboh/ workspace for intermediate review artifacts
    // (exploration.md, scores.json, etc.). Phases write here; other agents read.
    let kuriboh_dir = target.join(".kuriboh");
    std::fs::create_dir_all(&kuriboh_dir)
        .with_context(|| format!("creating {}", kuriboh_dir.display()))?;
    for subdir in ["findings", "worktrees", "pocs", "frontier"] {
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

    for agent in &agents {
        let dest = agents_dir.join(format!("{}.md", agent.name));
        std::fs::write(&dest, agent.render())
            .with_context(|| format!("writing agent def {}", dest.display()))?;
        tracing::debug!(agent = %agent.name, path = %dest.display(), "Installed agent");
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

#[cfg(test)]
mod tests {
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

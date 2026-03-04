//! Docker AI Sandbox integration.
//!
//! Each `kuriboh` run spawns Claude Code inside an isolated Docker AI Sandbox
//! microVM (<https://docs.docker.com/ai/sandboxes/>). The sandbox provides:
//!
//! - Full filesystem and network isolation from the host
//! - A private Docker daemon (so agents can safely run containers themselves)
//! - Bidirectional workspace sync at the same absolute path inside the VM
//!
//! Because the agent is isolated inside the microVM, we pass
//! `--dangerously-skip-permissions` so Claude Code runs fully autonomously
//! without interactive permission prompts — the sandbox is the permission
//! boundary, not per-tool confirmations.

use std::path::Path;

/// Configuration for the Docker AI Sandbox wrapper.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Run inside a Docker AI Sandbox microVM (default: true).
    ///
    /// Disable with `--no-sandbox` for local development / CI environments
    /// where Docker Desktop with sandbox support is not available.
    pub enabled: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl SandboxConfig {
    /// Returns `(program, argv)` for the process to spawn.
    ///
    /// **Sandboxed** (default):
    /// ```text
    /// docker sandbox run \
    ///   -e CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 \
    ///   <workspace> claude \
    ///   -- <claude_args…>
    /// ```
    ///
    /// **Unsandboxed** (`--no-sandbox`):
    /// ```text
    /// claude <claude_args…>
    /// ```
    ///
    /// The workspace path is passed directly; Docker AI Sandbox syncs it into
    /// the microVM at the same absolute path so all file references remain valid.
    pub fn build_command(&self, workspace: &Path, claude_args: Vec<String>) -> (String, Vec<String>) {
        if !self.enabled {
            return ("claude".to_string(), claude_args);
        }

        let mut args = vec![
            "sandbox".to_string(),
            "run".to_string(),
            // Forward the agent-teams feature flag into the sandboxed environment.
            "-e".to_string(),
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1".to_string(),
            // Workspace to sync (absolute path, same inside and outside the VM).
            workspace.to_string_lossy().into_owned(),
            // Agent binary to invoke.
            "claude".to_string(),
            // Separator: everything after this is passed to claude, not docker.
            "--".to_string(),
        ];
        args.extend(claude_args);

        ("docker".to_string(), args)
    }
}

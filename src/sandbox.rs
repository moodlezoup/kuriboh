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
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// docker sandbox run claude <workspace> -- <claude_args…>
    /// ```
    ///
    /// **Unsandboxed** (`--no-sandbox`):
    /// ```text
    /// claude <claude_args…>
    /// ```
    ///
    /// The workspace path is canonicalized to an absolute path; Docker AI
    /// Sandbox syncs it into the microVM at the same path so all file
    /// references remain valid.
    ///
    /// Note: `docker sandbox run` does not support `-e` for env vars like
    /// `docker run` does. Environment variables (e.g.
    /// `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`) must be set in the user's
    /// shell profile so the sandbox daemon picks them up.
    pub fn build_command(&self, workspace: &Path, claude_args: Vec<String>) -> (String, Vec<String>) {
        if !self.enabled {
            return ("claude".to_string(), claude_args);
        }

        let abs_workspace = std::fs::canonicalize(workspace)
            .unwrap_or_else(|_| std::path::absolute(workspace).unwrap_or(workspace.to_path_buf()));

        // Unique name so we always get a fresh sandbox (not reconnect to stale one).
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let pid = std::process::id();
        let sandbox_name = format!("kuriboh-{ts}-{pid}");

        let mut args = vec![
            "sandbox".to_string(),
            "run".to_string(),
            "--name".to_string(),
            sandbox_name,
            // Agent name (must come before workspace).
            "claude".to_string(),
            // Workspace to sync (absolute path).
            abs_workspace.to_string_lossy().into_owned(),
            // Separator: everything after this is passed to claude, not docker.
            "--".to_string(),
        ];
        args.extend(claude_args);

        ("docker".to_string(), args)
    }
}

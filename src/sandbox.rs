//! Sandbox configuration for running Claude Code securely.
//!
//! kuriboh supports two isolation strategies:
//!
//! 1. **Claude Code native sandbox** (default): Uses OS-level primitives
//!    (bubblewrap on Linux, Seatbelt on macOS) for filesystem and network
//!    isolation. Claude Code runs with `--dangerously-skip-permissions` because
//!    the native sandbox restricts what commands can actually do.
//!
//! 2. **No sandbox** (`--no-sandbox`): Claude Code runs directly on the host
//!    without `--dangerously-skip-permissions`. The user retains per-tool
//!    confirmation prompts. Only for local development.

use std::path::Path;

/// Configuration for the sandbox wrapper.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Run with sandbox isolation (default: true).
    ///
    /// When enabled, `--dangerously-skip-permissions` is passed to Claude Code
    /// because the sandbox restricts filesystem/network access at the OS level.
    ///
    /// Disable with `--no-sandbox` for local development / CI environments
    /// where the native sandbox may not be available.
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
    /// claude --dangerously-skip-permissions <claude_args…>
    /// ```
    ///
    /// **Unsandboxed** (`--no-sandbox`):
    /// ```text
    /// claude <claude_args…>
    /// ```
    ///
    /// In both cases Claude Code is invoked directly. The difference is whether
    /// `--dangerously-skip-permissions` is included — it is only safe when the
    /// native sandbox is active, restricting filesystem writes and network
    /// access at the OS level.
    pub fn build_command(&self, _workspace: &Path, claude_args: Vec<String>) -> (String, Vec<String>) {
        let mut args = Vec::new();

        if self.enabled {
            args.push("--dangerously-skip-permissions".to_string());
        }

        args.extend(claude_args);
        ("claude".to_string(), args)
    }
}

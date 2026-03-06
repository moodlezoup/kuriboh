# kuriboh

A Claude Code harness for automated security reviews of Rust codebases.

kuriboh wraps the `claude` CLI to orchestrate a multi-phase, multi-agent security review pipeline. It installs specialized subagent definitions into a target project, spawns Claude Code inside a Docker AI Sandbox, and produces a structured security report.

## How it works

kuriboh runs a 5-phase review pipeline, all orchestrated by a single Claude Code session:

1. **Exploration** -- A fast read-only Explore subagent surveys the codebase: module tree, entry points, architectural patterns, unsafe hotspots.
2. **Scouting** -- A scout subagent (Haiku, parallel) scores every `.rs` file on a 0-100 complexity/bug-proneness scale using 10 weighted heuristics.
3. **Deep Review** -- Reviewer agents are assigned starting files via weighted random sampling (higher scout scores = higher probability). Each reviewer creates a git worktree and performs a depth-first security audit, following call chains and writing PoCs.
4. **Appraisal** -- An appraiser agent validates each reviewer's findings: checks claims against source, tests PoCs, adjusts severity, filters false positives.
5. **Compilation** -- The lead agent deduplicates findings across reviewers, generates coverage statistics, and produces the final report.

### Design tenets

- **File-based**: All intermediate artifacts live in `.kuriboh/` (exploration.md, scores.json, findings, worktrees, PoCs).
- **Read-only by default**: The original codebase is never modified. Reviewers work in isolated git worktrees.
- **Massively parallel**: Scouts, reviewers, and appraisers all run as background subagents.

## Installation

```bash
cargo install --path .
```

### Requirements

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (`claude` CLI on PATH)
- Rust toolchain (for building kuriboh itself)
- Claude Code's native sandbox enabled (`/sandbox` in Claude Code) for safe autonomous operation

## Usage

```bash
# Standard usage (interactive permission prompts)
kuriboh --target /path/to/rust/crate

# Skip permission prompts (requires sandbox or container isolation)
kuriboh --target ./my-crate --dangerously-skip-permissions

# Customize output
kuriboh --target ./my-crate --output report.json --json

# More reviewers, more turns
kuriboh --target ./large-crate --reviewers 10 --max-turns 600

# Keep intermediate artifacts for debugging
kuriboh --target ./my-crate --keep-workspace
```

### CLI flags

| Flag | Default | Description |
|------|---------|-------------|
| `--target PATH` | (required) | Path to the Rust codebase to review |
| `--output PATH` | `kuriboh-report.md` | Output report path (`.json` extension triggers JSON) |
| `--model MODEL` | `claude-sonnet-4-6` | Model for the orchestrating team lead |
| `--reviewers N` | dynamic | Number of reviewer agents (default: `ceil(sqrt(files))` clamped [3,12]) |
| `--max-turns N` | `400` | Max turns for the Claude Code session |
| `--json` | off | Force JSON output regardless of file extension |
| `--dangerously-skip-permissions` | off | Pass `--dangerously-skip-permissions` to Claude Code |
| `--keep-workspace` | off | Preserve `.kuriboh/` directory after the run |
| `--agents NAMES` | all | Comma-separated agent names to deploy |
| `--agents-config PATH` | none | TOML file for customizing agent prompts |

### Environment variables

- `RUST_LOG=kuriboh=debug` -- verbose logging (shows full orchestration prompt, events, etc.)
- `ANTHROPIC_API_KEY` -- required by Claude Code

## Output

kuriboh produces a Markdown or JSON report containing:

- **Executive Summary** -- high-level risk overview
- **Scouting Overview** -- file risk tier distribution
- **Review Coverage** -- reviewer count, files covered, tier coverage percentages
- **Findings** -- each with severity, file location, call chain, scout score, PoC status, appraiser verdict
- **Needs Review** -- findings requiring human judgment
- **Remediation Roadmap** -- prioritized action list

## Project structure

```
src/
  main.rs           -- Entry point: install agents -> run claude -> parse report -> cleanup
  cli.rs            -- CLI argument definitions (clap)
  runner.rs         -- Spawns Claude Code, streams NDJSON events, builds orchestration prompt
  events.rs         -- ClaudeEvent model for --output-format stream-json
  report.rs         -- Report/Finding structs, Markdown/JSON rendering
  sandbox.rs        -- Sandbox config (controls --dangerously-skip-permissions)
  agents/
    mod.rs          -- Agent installation (.claude/agents/) and .kuriboh/ lifecycle
    templates.rs    -- 6 embedded subagent definitions
```

### Subagents

| Agent | Model | Role |
|-------|-------|------|
| scout | haiku | Per-file complexity scoring (Phase 2) |
| reviewer | sonnet | Depth-first security audit with worktree (Phase 3) |
| appraiser | sonnet | Finding validation and PoC testing (Phase 4) |
| unsafe-auditor | sonnet | Specialist: memory safety (available to reviewers) |
| dep-checker | haiku | Specialist: dependency CVEs (available to reviewers) |
| crypto-reviewer | sonnet | Specialist: cryptographic misuse (available to reviewers) |

## License

MIT

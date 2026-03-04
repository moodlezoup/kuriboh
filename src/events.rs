//! Types for Claude Code's `--output-format stream-json` NDJSON event stream.
//!
//! Claude Code emits one JSON object per line on stdout. Each object carries a
//! `"type"` discriminant. We model the types we care about and log-and-skip
//! anything unknown.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single event from `claude --output-format stream-json`.
///
/// Events arrive as newline-delimited JSON (NDJSON); use [`parse_line`] to
/// convert a raw line into this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClaudeEvent {
    /// Emitted once at startup with session metadata.
    System {
        subtype: String,
        session_id: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        tools: Vec<Value>,
    },

    /// A turn produced by the assistant: text, tool-use, and/or tool-result blocks.
    Assistant {
        session_id: String,
        message: AssistantMessage,
    },

    /// Tool results fed back from the environment to the assistant.
    User {
        session_id: String,
        message: Value,
    },

    /// The final event, emitted after all turns complete (success or error).
    Result {
        subtype: String,
        session_id: String,
        is_error: bool,
        /// The synthesized final text output from the lead agent.
        result: String,
        #[serde(default)]
        duration_ms: Option<u64>,
        #[serde(default)]
        num_turns: Option<u32>,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        usage: Option<TokenUsage>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub id: Option<String>,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

/// A block within an [`AssistantMessage`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(default)]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// Parse one NDJSON line from the claude event stream.
///
/// Returns `None` for blank lines or non-JSON lines. Only lines that start
/// with `{` but fail to parse are logged (at DEBUG), since `--verbose` mode
/// produces many non-JSON lines (ANSI sequences, progress text, etc.).
pub fn parse_line(line: &str) -> Option<ClaudeEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    match serde_json::from_str(trimmed) {
        Ok(ev) => Some(ev),
        Err(e) => {
            tracing::debug!("Skipping malformed JSON event ({e}): {}", &trimmed[..trimmed.len().min(200)]);
            None
        }
    }
}

/// Extract the final synthesized result text from a completed event stream.
pub fn final_result(events: &[ClaudeEvent]) -> Option<&str> {
    events.iter().rev().find_map(|ev| match ev {
        ClaudeEvent::Result { result, is_error: false, .. } => Some(result.as_str()),
        _ => None,
    })
}

/// Sum token costs across all `Result` events (agent team leads + teammates).
pub fn total_cost_usd(events: &[ClaudeEvent]) -> f64 {
    events
        .iter()
        .filter_map(|ev| match ev {
            ClaudeEvent::Result { total_cost_usd, .. } => *total_cost_usd,
            _ => None,
        })
        .sum()
}

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
    User { session_id: String, message: Value },

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
#[expect(clippy::struct_field_names)]
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
            tracing::debug!(
                "Skipping malformed JSON event ({e}): {}",
                &trimmed[..trimmed.len().min(200)]
            );
            None
        }
    }
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

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_blank() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("\n").is_none());
    }

    #[test]
    fn parse_line_non_json() {
        assert!(parse_line("hello world").is_none());
        assert!(parse_line("--- progress 50% ---").is_none());
        assert!(parse_line("\x1b[32mgreen text\x1b[0m").is_none());
    }

    #[test]
    fn parse_line_malformed_json() {
        assert!(parse_line("{not valid json}").is_none());
        assert!(parse_line(r#"{"type": "unknown_variant"}"#).is_none());
    }

    #[test]
    fn parse_line_system_event() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc","model":"claude-sonnet-4-6","tools":[]}"#;
        let ev = parse_line(line).unwrap();
        assert!(matches!(ev, ClaudeEvent::System { session_id, .. } if session_id == "abc"));
    }

    #[test]
    fn parse_line_result_event() {
        let line = r#"{"type":"result","subtype":"success","session_id":"abc","is_error":false,"result":"done","total_cost_usd":1.23}"#;
        let ev = parse_line(line).unwrap();
        assert!(
            matches!(ev, ClaudeEvent::Result { total_cost_usd: Some(c), .. } if (c - 1.23).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn parse_line_with_leading_whitespace() {
        let line = r#"  {"type":"system","subtype":"init","session_id":"s1","tools":[]}"#;
        assert!(parse_line(line).is_some());
    }

    #[test]
    fn total_cost_sums_result_events() {
        let events = vec![
            ClaudeEvent::System {
                subtype: "init".into(),
                session_id: "s1".into(),
                model: None,
                tools: vec![],
            },
            ClaudeEvent::Result {
                subtype: "success".into(),
                session_id: "s1".into(),
                is_error: false,
                result: "done".into(),
                duration_ms: None,
                num_turns: None,
                total_cost_usd: Some(1.50),
                usage: None,
            },
            ClaudeEvent::Result {
                subtype: "success".into(),
                session_id: "s2".into(),
                is_error: false,
                result: "done".into(),
                duration_ms: None,
                num_turns: None,
                total_cost_usd: Some(0.75),
                usage: None,
            },
        ];
        let cost = total_cost_usd(&events);
        assert!((cost - 2.25).abs() < f64::EPSILON);
    }

    #[test]
    fn total_cost_empty_events() {
        assert!((total_cost_usd(&[]) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn total_cost_skips_none_costs() {
        let events = vec![ClaudeEvent::Result {
            subtype: "success".into(),
            session_id: "s1".into(),
            is_error: false,
            result: "done".into(),
            duration_ms: None,
            num_turns: None,
            total_cost_usd: None,
            usage: None,
        }];
        assert!((total_cost_usd(&events) - 0.0).abs() < f64::EPSILON);
    }
}

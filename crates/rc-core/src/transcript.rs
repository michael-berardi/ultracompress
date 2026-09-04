//! Brief transcript: chronological conversation flow with tool calls
//! collapsed to one-liners and (#N) references, in a rolling window.

use crate::model::{Block, RcMessage, truncate_chars};

#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptLine {
    pub reference: String, // "#N" or null-ish
    pub line: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Transcript {
    pub lines: Vec<TranscriptLine>,
    pub omitted_lines: usize,
    pub total_lines: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranscriptConfig {
    /// Max rendered transcript lines (rolling window keeps the tail).
    pub max_lines: usize,
    /// Max chars per rendered line.
    pub max_line_chars: usize,
    /// Chars of tool-result text echoed per referenced call.
    pub tool_result_chars: usize,
}

impl Default for TranscriptConfig {
    fn default() -> Self {
        TranscriptConfig { max_lines: 120, max_line_chars: 220, tool_result_chars: 160 }
    }
}

struct Entry {
    reference: String,
    line: String,
}

/// Build the brief transcript. Tool results collapse into `(#N)` refs on the
/// triggering tool call line; user/assistant text is kept but truncated.
pub fn build(messages: &[RcMessage], cfg: &TranscriptConfig) -> Transcript {
    let mut entries: Vec<Entry> = Vec::new();
    let mut ref_counter = 0usize;
    // Map tool_call_id -> reference for pairing results to calls.
    let mut call_refs: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for m in messages {
        match m.role {
            crate::model::Role::User => {
                let text = collect_text(&m.content);
                if text.trim().is_empty() {
                    continue;
                }
                entries.push(Entry {
                    reference: String::new(),
                    line: format!("[user] {}", truncate_chars(text.trim(), cfg.max_line_chars)),
                });
            }
            crate::model::Role::Assistant => {
                for b in &m.content {
                    match b {
                        Block::Text { text } => {
                            let t = text.trim();
                            if !t.is_empty() {
                                entries.push(Entry {
                                    reference: String::new(),
                                    line: format!("[assistant] {}", truncate_chars(t, cfg.max_line_chars)),
                                });
                            }
                        }
                        Block::ToolCall { id, name, arguments } => {
                            ref_counter += 1;
                            let reference = format!("#{ref_counter}");
                            if !id.is_empty() {
                                call_refs.insert(id.clone(), reference.clone());
                            }
                            entries.push(Entry {
                                reference: reference.clone(),
                                line: format!(
                                    "* {} {} ({})",
                                    name,
                                    tool_args_digest(name, arguments, cfg.max_line_chars),
                                    reference
                                ),
                            });
                        }
                        _ => {}
                    }
                }
            }
            crate::model::Role::ToolResult => {
                for b in &m.content {
                    if let Block::ToolResult { tool_call_id, tool_name, text, is_error } = b {
                        let reference = call_refs.get(tool_call_id).cloned().unwrap_or_default();
                        if reference.is_empty() {
                            ref_counter += 1;
                            let reference = format!("#{ref_counter}");
                            entries.push(Entry {
                                reference: reference.clone(),
                                line: format!(
                                    "* {} result ({}) {}",
                                    if tool_name.is_empty() { "tool" } else { tool_name },
                                    reference,
                                    result_digest(text, *is_error, cfg.tool_result_chars)
                                ),
                            });
                        } else {
                            // Attach a short digest line under the call.
                            entries.push(Entry {
                                reference,
                                line: result_digest(text, *is_error, cfg.tool_result_chars),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let total_lines = entries.len();
    let omitted = total_lines.saturating_sub(cfg.max_lines);
    let lines = entries
        .into_iter()
        .skip(omitted)
        .map(|e| TranscriptLine { reference: e.reference, line: e.line })
        .collect();
    Transcript { lines, omitted_lines: omitted, total_lines }
}

fn collect_text(content: &[Block]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn result_digest(text: &str, is_error: bool, max: usize) -> String {
    let t = text.trim();
    if t.is_empty() {
        return "(empty result)".to_string();
    }
    let first = t.lines().next().unwrap_or("");
    let tag = if is_error { "ERROR " } else { "" };
    format!("→ {tag}{}", truncate_chars(first, max))
}

fn tool_args_digest(name: &str, args: &serde_json::Value, max: usize) -> String {
    match name {
        "bash" => args
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| format!("{:?}", truncate_chars(c, max.min(160))))
            .unwrap_or_else(|| "\"\"".into()),
        "read" | "edit" | "write" => args
            .get("path")
            .or_else(|| args.get("file_path"))
            .and_then(|p| p.as_str())
            .map(|p| format!("{p:?}"))
            .unwrap_or_default(),
        _ => {
            let s = args.to_string();
            if s == "null" {
                String::new()
            } else {
                truncate_chars(&s, 80)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_tool_calls_with_refs() {
        let msgs = vec![
            RcMessage {
                id: "1".into(),
                role: crate::model::Role::User,
                content: vec![Block::Text { text: "run the tests".into() }],
                timestamp: None,
            },
            RcMessage {
                id: "2".into(),
                role: crate::model::Role::Assistant,
                content: vec![Block::ToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "cargo test"}),
                }],
                timestamp: None,
            },
            RcMessage {
                id: "3".into(),
                role: crate::model::Role::ToolResult,
                content: vec![Block::ToolResult {
                    tool_call_id: "call_1".into(),
                    tool_name: "bash".into(),
                    text: "test result: ok. 42 passed".into(),
                    is_error: false,
                }],
                timestamp: None,
            },
        ];
        let t = build(&msgs, &TranscriptConfig::default());
        let joined: String = t.lines.iter().map(|l| l.line.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("[user] run the tests"));
        assert!(joined.contains("* bash \"cargo test\" (#1)"));
        assert!(joined.contains("→ test result: ok. 42 passed"));
    }

    #[test]
    fn rolling_window_keeps_recent() {
        let cfg = TranscriptConfig { max_lines: 3, ..Default::default() };
        let msgs: Vec<RcMessage> = (0..10)
            .map(|i| RcMessage {
                id: format!("u{i}"),
                role: crate::model::Role::User,
                content: vec![Block::Text { text: format!("message {i}") }],
                timestamp: None,
            })
            .collect();
        let t = build(&msgs, &cfg);
        assert_eq!(t.lines.len(), 3);
        assert_eq!(t.omitted_lines, 7);
        assert!(t.lines.last().unwrap().line.contains("message 9"));
    }
}

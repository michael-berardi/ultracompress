//! Core data model: messages and content blocks, tolerant of both raw Pi
//! `AgentMessage` shapes (session JSONL) and converted LLM `Message` shapes
//! (what the extension passes over stdin).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Normalized content block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        text: String,
    },
    ToolCall {
        #[serde(default)]
        id: String,
        name: String,
        #[serde(default)]
        arguments: Value,
    },
    /// A tool result (raw sessions may nest these inside user messages).
    ToolResult {
        #[serde(default)]
        tool_call_id: String,
        #[serde(default)]
        tool_name: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        is_error: bool,
    },
    Image {
        #[serde(default)]
        mime_type: String,
        /// base64 payload when present.
        #[serde(default)]
        data: String,
        #[serde(default)]
        url: String,
    },
    Other {
        #[serde(rename = "kind")]
        kind: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    ToolResult,
    System,
    Custom,
    Other,
}

impl Role {
    pub fn parse(s: &str) -> Role {
        match s {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "toolResult" | "tool_result" | "tool" => Role::ToolResult,
            "system" => Role::System,
            "custom" => Role::Custom,
            _ => Role::Other,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::ToolResult => "toolResult",
            Role::System => "system",
            Role::Custom => "custom",
            Role::Other => "other",
        };
        f.write_str(s)
    }
}

/// Normalized message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RcMessage {
    /// Stable id when known (session entry id); synthesized otherwise.
    #[serde(default)]
    pub id: String,
    pub role: Role,
    pub content: Vec<Block>,
    #[serde(default)]
    pub timestamp: Option<u64>,
}

impl RcMessage {
    pub fn total_chars(&self) -> usize {
        self.content
            .iter()
            .map(|b| match b {
                Block::Text { text } => text.len(),
                Block::Thinking { thinking, text } => thinking.len() + text.len(),
                Block::ToolResult { text, .. } => text.len(),
                Block::ToolCall { arguments, .. } => arguments.to_string().len(),
                _ => 0,
            })
            .sum()
    }

    pub fn text_preview(&self, max: usize) -> String {
        for b in &self.content {
            if let Block::Text { text } = b {
                return truncate_chars(text, max);
            }
        }
        for b in &self.content {
            if let Block::ToolResult { text, .. } = b {
                return truncate_chars(text, max);
            }
        }
        String::new()
    }
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Parse a single content value (string or block array) into blocks.
pub fn parse_content(v: &Value) -> Vec<Block> {
    match v {
        Value::String(s) => vec![Block::Text { text: s.clone() }],
        Value::Array(items) => items.iter().filter_map(parse_block).collect(),
        _ => vec![],
    }
}

fn parse_block(v: &Value) -> Option<Block> {
    let obj = v.as_object()?;
    let ty = obj.get("type")?.as_str()?;
    match ty {
        "text" => Some(Block::Text {
            text: obj
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "thinking" => Some(Block::Thinking {
            thinking: obj
                .get("thinking")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            text: obj
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "toolCall" | "tool_call" | "toolUse" => Some(Block::ToolCall {
            id: obj
                .get("id")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            name: obj
                .get("name")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string(),
            arguments: obj.get("arguments").cloned().unwrap_or(Value::Null),
        }),
        "toolResult" | "tool_result" => Some(Block::ToolResult {
            tool_call_id: obj
                .get("toolCallId")
                .or_else(|| obj.get("tool_call_id"))
                .or_else(|| obj.get("toolUseId"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            tool_name: obj
                .get("toolName")
                .or_else(|| obj.get("tool_name"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            text: extract_tool_result_text(obj.get("content")),
            is_error: obj
                .get("isError")
                .or_else(|| obj.get("is_error"))
                .and_then(|t| t.as_bool())
                .unwrap_or(false),
        }),
        "image" => Some(Block::Image {
            mime_type: obj
                .get("mimeType")
                .or_else(|| obj.get("mime_type"))
                .and_then(|t| t.as_str())
                .unwrap_or("image/png")
                .to_string(),
            data: obj
                .get("data")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            url: obj
                .get("url")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        other => Some(Block::Other {
            kind: other.to_string(),
        }),
    }
}

/// Tool result content may be a string, a block array, or nested objects.
fn extract_tool_result_text(v: Option<&Value>) -> String {
    match v {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|it| match it {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(o)) => o
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(String::new),
        Some(_) => String::new(),
    }
}

/// Parse a message value (raw session entry or converted LLM message).
pub fn parse_message(v: &Value, fallback_id: &str) -> Option<RcMessage> {
    let obj = v.as_object()?;

    // Raw session entry: { type: "message", id, message: { role, content } }
    if obj.get("type").and_then(|t| t.as_str()) == Some("message") {
        let inner = obj.get("message")?.as_object()?;
        let role = Role::parse(inner.get("role").and_then(|r| r.as_str())?);
        let mut content = parse_content(inner.get("content").unwrap_or(&Value::Null));
        // Raw sessions put tool metadata at the message level with plain text
        // blocks; normalize into a single ToolResult block so both shapes
        // (raw entries and converted LLM messages) route identically.
        if role == Role::ToolResult {
            content = normalize_tool_result(content, inner);
        }
        return Some(RcMessage {
            id: obj
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or(fallback_id)
                .to_string(),
            role,
            content,
            timestamp: obj.get("timestamp").and_then(|t| t.as_u64()),
        });
    }

    // Converted message: { role, content }
    let role_v = obj.get("role")?.as_str()?;
    let role = Role::parse(role_v);
    let mut content = parse_content(obj.get("content").unwrap_or(&Value::Null));
    if role == Role::ToolResult {
        content = normalize_tool_result(content, obj);
    }
    Some(RcMessage {
        id: obj
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or(fallback_id)
            .to_string(),
        role,
        content,
        timestamp: obj.get("timestamp").and_then(|t| t.as_u64()),
    })
}

/// Coerce a toolResult message's content into ToolResult blocks.
fn normalize_tool_result(content: Vec<Block>, meta: &serde_json::Map<String, Value>) -> Vec<Block> {
    let already = content
        .iter()
        .any(|b| matches!(b, Block::ToolResult { .. }));
    if already {
        return content;
    }
    let text = content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(text.as_str()),
            Block::ToolResult { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    vec![Block::ToolResult {
        tool_call_id: meta
            .get("toolCallId")
            .or_else(|| meta.get("tool_call_id"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        tool_name: meta
            .get("toolName")
            .or_else(|| meta.get("tool_name"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        text,
        is_error: meta
            .get("isError")
            .or_else(|| meta.get("is_error"))
            .and_then(|t| t.as_bool())
            .unwrap_or(false),
    }]
}

/// Parse an array of messages (the extension stdin contract).
pub fn parse_messages(v: &Value) -> Vec<RcMessage> {
    match v {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| parse_message(item, &format!("m{i}")))
            .collect(),
        _ => vec![],
    }
}

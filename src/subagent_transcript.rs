//! Best-effort renderer for agent sub-agent transcripts.
//!
//! Sub-agent transcripts are JSONL files written by the agent CLI; their exact
//! line shapes are not a herdr contract. Parsing skips anything unrecognized so
//! a CLI format change degrades to fewer lines, never an error.

const MAX_LINE_LEN: usize = 200;
const MAX_LINES: usize = 500;

/// Renders JSONL transcript content into display lines.
///
/// Malformed lines are skipped. Each output line is at most [`MAX_LINE_LEN`]
/// chars and at most [`MAX_LINES`] lines are produced.
pub fn render_transcript(jsonl: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in jsonl.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        if let Some(record) = render_record(object) {
            lines.extend(record);
            if lines.len() >= MAX_LINES {
                lines.truncate(MAX_LINES);
                return lines;
            }
        }
    }
    lines
}

/// Renders one transcript record into zero or more display lines.
fn render_record(object: &serde_json::Map<String, serde_json::Value>) -> Option<Vec<String>> {
    let message = object.get("message").and_then(|value| value.as_object())?;
    let role = message.get("role").and_then(|value| value.as_str())?;
    let content = message.get("content")?;
    match role {
        "user" => {
            let text = content_text(content)?;
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            Some(vec![format_line(&format!("user: {text}"))])
        }
        "assistant" => {
            let mut lines = Vec::new();
            if let Some(blocks) = content.as_array() {
                for block in blocks {
                    let block = block.as_object()?;
                    match block.get("type").and_then(|value| value.as_str()) {
                        Some("text") => {
                            let text = block
                                .get("text")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default();
                            let text = text.trim();
                            if !text.is_empty() {
                                lines.push(format_line(&format!("claude: {text}")));
                            }
                        }
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(|value| value.as_str())
                                .unwrap_or("tool");
                            let summary = compact_tool_input(block.get("input"));
                            lines.push(format_line(&format!("tool {name} {summary}")));
                        }
                        _ => {}
                    }
                }
            } else if let Some(text) = content_text(content) {
                let text = text.trim();
                if !text.is_empty() {
                    lines.push(format_line(&format!("claude: {text}")));
                }
            }
            Some(lines)
        }
        _ => None,
    }
}

/// Extracts text from either a plain string content or a text block list.
fn content_text(content: &serde_json::Value) -> Option<&str> {
    if let Some(text) = content.as_str() {
        return Some(text);
    }
    // Block-list content carries tool results for user records; not plain text.
    None
}

/// Builds a compact single-line summary of a tool input object.
fn compact_tool_input(input: Option<&serde_json::Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let Some(object) = input.as_object() else {
        return truncate_chars(&input.to_string(), 80);
    };
    // Prefer a human-meaningful field when present.
    for key in [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
    ] {
        if let Some(value) = object.get(key) {
            let rendered = match value.as_str() {
                Some(text) => text.to_string(),
                None => value.to_string(),
            };
            return truncate_chars(&rendered, 80);
        }
    }
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    truncate_chars(&keys.join(","), 80)
}

fn format_line(line: &str) -> String {
    let single_line: String = line
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    truncate_chars(single_line.trim(), MAX_LINE_LEN)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_user_and_assistant_records() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"role":"user","content":"list the files"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Working on it."},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}"#,
            "\n",
        );
        let lines = render_transcript(jsonl);
        assert_eq!(
            lines,
            vec![
                "user: list the files".to_string(),
                "claude: Working on it.".to_string(),
                "tool Bash ls -la".to_string(),
            ]
        );
    }

    #[test]
    fn skips_malformed_and_unrecognized_lines() {
        let jsonl = concat!(
            "not json at all\n",
            "[1, 2, 3]\n",
            r#"{"type":"summary","summary":"compact"}"#,
            "\n",
            r#"{"message":{"role":"system","content":"prompt"}}"#,
            "\n",
        );
        assert!(render_transcript(jsonl).is_empty());
    }

    #[test]
    fn truncates_long_lines_and_caps_total_lines() {
        let long: String = "x".repeat(MAX_LINE_LEN + 50);
        let record = format!(
            r#"{{"message":{{"role":"user","content":{}}}}}"#,
            serde_json::to_string(&long).expect("serialize")
        );
        let mut jsonl = String::new();
        for _ in 0..(MAX_LINES + 20) {
            jsonl.push_str(&record);
            jsonl.push('\n');
        }
        let lines = render_transcript(&jsonl);
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0].chars().count(), MAX_LINE_LEN);
        assert!(lines[0].ends_with('…'));
    }

    #[test]
    fn tool_input_prefers_meaningful_fields_and_falls_back_to_keys() {
        let jsonl = concat!(
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/a.rs"}}]}}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"x":1,"y":2}}]}}"#,
            "\n",
        );
        let lines = render_transcript(jsonl);
        assert_eq!(lines[0], "tool Read /tmp/a.rs");
        assert_eq!(lines[1], "tool Edit x,y");
    }
}

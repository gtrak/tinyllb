//! Summarization prompt builder for the context compression sidecar.
//!
//! Provides a configurable prompt that compresses conversation turns into
//! compact, information-rich summaries. Supports custom templates loaded
//! from disk or a built-in default.

use crate::config::ContextPolicy;

const DEFAULT_TEMPLATE: &str = r#"You are a conversation summarizer. Your task is to compress a segment of conversation history into a concise summary that preserves all information needed to continue the conversation.

PRESERVE:
- Code snippets, file paths, and technical identifiers (verbatim if short, described if long)
- Decisions made and their rationale
- Factual context: entity names, project names, version numbers, URLs
- The state of any tasks in progress (what's done, what's pending)
- Tool calls and their results (summarize the outcome, not the full output)
- Errors encountered and how they were resolved
- User preferences and constraints stated in these turns

COMPRESS:
- Redundant repetition
- Verbose explanations (condense to key points)
- Full code blocks over 20 lines (describe what they do, keep key snippets)
- Pleasantries and meta-commentary

FORMAT:
- Bullet points, grouped by topic if the conversation spans multiple topics
- Start with "Summary of turns {start}-{end}:"
- Keep under {max_tokens} tokens

Do NOT add information not present in the original turns. Do NOT speculate or infer beyond what was explicitly stated."#;

/// Builds summarization prompts for the compression sidecar.
///
/// Caches the template text (default or custom from `prompt_template_path`)
/// at construction time so per-request prompt building is allocation-free
/// beyond the message serialization.
pub struct PromptBuilder {
    template: String,
    max_tokens: usize,
}

impl PromptBuilder {
    /// Construct from config. Loads a custom template from `prompt_template_path`
    /// if set; falls back to the default on any error.
    pub fn new(config: &ContextPolicy) -> Self {
        let template = match &config.prompt_template_path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(
                        path = %path,
                        error = %e,
                        "failed to load custom prompt template, using default"
                    );
                    DEFAULT_TEMPLATE.to_string()
                }
            },
            None => DEFAULT_TEMPLATE.to_string(),
        };
        Self {
            template,
            max_tokens: config.summary_max_tokens,
        }
    }

    /// Build the messages array for the sidecar `/v1/chat/completions` request.
    ///
    /// Returns `[{ "role":"system","content":"<template>" }, { "role":"user","content":"<serialized turns>" }]`.
    pub fn build(
        &self,
        messages_to_compress: &[serde_json::Value],
        turn_start: usize,
        turn_end: usize,
    ) -> Vec<serde_json::Value> {
        let system_content = self
            .template
            .replace("{start}", &turn_start.to_string())
            .replace("{end}", &turn_end.to_string())
            .replace("{max_tokens}", &self.max_tokens.to_string());
        let user_content = serialize_messages(messages_to_compress, turn_start, turn_end);
        vec![
            serde_json::json!({"role": "system", "content": system_content}),
            serde_json::json!({"role": "user", "content": user_content}),
        ]
    }
}

/// Build a summarization prompt messages array without a PromptBuilder.
///
/// Uses the default template (or `custom_template` if provided) with
/// `{start}`, `{end}`, `{max_tokens}` substituted.
pub fn build_summarization_prompt(
    messages_to_compress: &[serde_json::Value],
    turn_start: usize,
    turn_end: usize,
    max_tokens: usize,
    custom_template: Option<&str>,
) -> Vec<serde_json::Value> {
    let template = custom_template.unwrap_or(DEFAULT_TEMPLATE);
    let system_content = template
        .replace("{start}", &turn_start.to_string())
        .replace("{end}", &turn_end.to_string())
        .replace("{max_tokens}", &max_tokens.to_string());
    let user_content = serialize_messages(messages_to_compress, turn_start, turn_end);
    vec![
        serde_json::json!({"role": "system", "content": system_content}),
        serde_json::json!({"role": "user", "content": user_content}),
    ]
}

/// Truncate `text` to at most `max_chars` characters, appending `[...truncated]` if truncated.
fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{}[...truncated]", truncated)
    }
}

/// Format messages as a readable transcript for the summarizer's user message.
///
/// ```text
/// --- Conversation turns {start} to {end} ---
///
/// [user]: <content>
///
/// [assistant]: <content>
///
/// [tool]: <result summary>
/// ```
fn serialize_messages(messages: &[serde_json::Value], turn_start: usize, turn_end: usize) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "--- Conversation turns {} to {} ---",
        turn_start, turn_end
    ));

    for msg in messages {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Extract and format content
        let content = format_content(msg);

        // Handle tool_calls: append function name + truncated args
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let function_name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let args_raw = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let args_truncated = truncate_text(&args_raw, 200);
                parts.push(format!("[tool_call]: {}({})", function_name, args_truncated));
            }
        }

        parts.push(format!("[{}]: {}", role, content));
    }

    parts.join("\n\n")
}

/// Extract and format the content field from a single message.
fn format_content(msg: &serde_json::Value) -> String {
    let content = msg.get("content");

    match content {
        None | Some(serde_json::Value::Null) => "(empty)".to_string(),
        Some(serde_json::Value::String(s)) => truncate_text(s, 2000),
        Some(serde_json::Value::Array(parts)) => {
            // Multimodal content: concatenate text parts, skip non-text
            let mut texts = Vec::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    texts.push(text);
                }
            }
            if texts.is_empty() {
                "(empty)".to_string()
            } else {
                truncate_text(&texts.join(" "), 2000)
            }
        }
        Some(v) => {
            // Fallback: JSON-stringify anything unexpected
            let raw = v.to_string();
            truncate_text(&raw, 2000)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_prompt_has_placeholders() {
        assert!(DEFAULT_TEMPLATE.contains("{start}"));
        assert!(DEFAULT_TEMPLATE.contains("{end}"));
        assert!(DEFAULT_TEMPLATE.contains("{max_tokens}"));
    }

    #[test]
    fn test_substitution() {
        let policy = ContextPolicy {
            summary_max_tokens: 2048,
            ..Default::default()
        };
        let builder = PromptBuilder::new(&policy);
        let msgs: Vec<serde_json::Value> = vec![];
        let result = builder.build(&msgs, 3, 8);

        let system_content = result[0]["content"].as_str().expect("system content is string");

        // Should contain substituted values
        assert!(system_content.contains("3"), "should contain turn start");
        assert!(system_content.contains("8"), "should contain turn end");
        assert!(system_content.contains("2048"), "should contain max_tokens");

        // Should not contain un-replaced placeholders
        assert!(
            !system_content.contains("{start}"),
            "{{start}} should be replaced"
        );
        assert!(!system_content.contains("{end}"), "{{end}} should be replaced");
        assert!(
            !system_content.contains("{max_tokens}"),
            "{{max_tokens}} should be replaced"
        );

        // The default template says "turns {start}-{end}:", so after substitution it has "3-8"
        assert!(system_content.contains("3-8"), "should have 'turns 3-8'");
    }

    #[test]
    fn test_message_serialization() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "Hi there"}),
            serde_json::json!({"role": "tool", "content": "tool output here"}),
        ];

        let serialized = serialize_messages(&messages, 1, 3);

        assert!(
            serialized.starts_with("--- Conversation turns 1 to 3 ---"),
            "should start with conversation header"
        );
        assert!(serialized.contains("[user]: Hello"), "should contain user message");
        assert!(
            serialized.contains("[assistant]: Hi there"),
            "should contain assistant message"
        );
        assert!(
            serialized.contains("[tool]: tool output here"),
            "should contain tool message"
        );
    }

    #[test]
    fn test_truncation() {
        let long_content = "x".repeat(3000);
        let messages = vec![serde_json::json!({"role": "user", "content": long_content})];

        let serialized = serialize_messages(&messages, 0, 1);

        // The user content should be truncated to 2000 chars + truncation marker
        let expected_truncated = "x".repeat(2000) + "[...truncated]";
        assert!(
            serialized.contains(&expected_truncated),
            "should contain truncated content with marker"
        );
    }

    #[test]
    fn test_tool_call_serialization() {
        let messages = vec![serde_json::json!({
            "role": "assistant",
            "content": "Let me check that for you",
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\": \"/some/file.txt\"}"
                    }
                }
            ]
        })];

        let serialized = serialize_messages(&messages, 0, 1);

        assert!(
            serialized.contains("[tool_call]: read_file("),
            "should contain tool call with function name"
        );
        assert!(
            serialized.contains("/some/file.txt"),
            "should contain function arguments"
        );
    }

    #[test]
    fn test_custom_template() {
        let result = build_summarization_prompt(
            &[],
            5,
            10,
            512,
            Some("Summarize turns {start}-{end} in {max_tokens} tokens"),
        );

        let system_content = result[0]["content"].as_str().expect("system content is string");
        assert_eq!(system_content, "Summarize turns 5-10 in 512 tokens");
    }

    #[test]
    fn test_multimodal_content_serialization() {
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "Here is an image:"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,..."}},
                {"type": "text", "text": "What does it show?"}
            ]
        })];

        let serialized = serialize_messages(&messages, 0, 1);

        assert!(
            serialized.contains("Here is an image:"),
            "should contain first text part"
        );
        assert!(
            serialized.contains("What does it show?"),
            "should contain second text part"
        );
        // Image part should not appear (skipped)
        assert!(
            !serialized.contains("image_url"),
            "should not contain image_url type marker"
        );
        assert!(
            !serialized.contains("base64"),
            "should not contain base64 data"
        );
    }

    #[test]
    fn test_free_function_matches_builder() {
        let policy = ContextPolicy {
            summary_max_tokens: 512,
            ..Default::default()
        };

        let builder = PromptBuilder::new(&policy);
        let messages = vec![serde_json::json!({"role": "user", "content": "test"})];

        let builder_result = builder.build(&messages, 10, 20);
        let free_result = build_summarization_prompt(&messages, 10, 20, 512, None);

        // Compare system content
        let builder_system = builder_result[0]["content"].as_str().unwrap();
        let free_system = free_result[0]["content"].as_str().unwrap();
        assert_eq!(builder_system, free_system, "system content should match");

        // Compare user content
        let builder_user = builder_result[1]["content"].as_str().unwrap();
        let free_user = free_result[1]["content"].as_str().unwrap();
        assert_eq!(builder_user, free_user, "user content should match");
    }
}

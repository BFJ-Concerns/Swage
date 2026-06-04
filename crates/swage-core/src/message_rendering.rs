use std::fmt::Write as _;

use crate::{ChatMessage, ContentPart, Role, ToolReplayPart};

pub fn render_message_for_model_context(message: &ChatMessage) -> String {
    let mut rendered = text_content(message);

    if let Some(replay) = message.tool_replay() {
        for part in replay {
            if !rendered.is_empty() {
                rendered.push(' ');
            }
            rendered.push_str(&render_tool_replay_part(part));
        }
    }

    if rendered.is_empty() {
        "(no text content)".to_string()
    } else {
        rendered
    }
}

pub fn role_label_for_model_context(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn text_content(message: &ChatMessage) -> String {
    message
        .content()
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_tool_replay_part(part: &ToolReplayPart) -> String {
    match part {
        ToolReplayPart::Text { text } => text.clone(),
        ToolReplayPart::Reasoning { content, details } => {
            let mut rendered = "[reasoning".to_string();
            if let Some(details) = details {
                let _ = write!(rendered, " details={}", compact_json(details));
            }
            let _ = write!(
                rendered,
                " content={}]",
                compact_json(&serde_json::json!(content))
            );
            rendered
        }
        ToolReplayPart::ToolCall {
            call_id,
            name,
            arguments,
        } => {
            let input = compact_json(arguments);
            format!("[tool_use id={call_id} name={name} input={input}]")
        }
        ToolReplayPart::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let mut rendered = format!("[tool_result tool_use_id={call_id}");
            if *is_error {
                rendered.push_str(" is_error=true");
            }
            let _ = write!(
                rendered,
                " content={}]",
                compact_json(&serde_json::json!(content))
            );
            rendered
        }
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

#[cfg(test)]
mod tests {
    use crate::{ChatMessage, Role, ToolCall, ToolResult, build_tool_round_messages};

    use super::*;

    #[test]
    fn renders_text_message_content() {
        let message = ChatMessage::new(Role::User, "hello");

        assert_eq!(render_message_for_model_context(&message), "hello");
    }

    #[test]
    fn renders_structured_tool_replay_blocks() {
        let messages = build_tool_round_messages(
            &[ToolCall {
                call_id: "call_1".into(),
                name: "search".into(),
                arguments: serde_json::json!({"query": "weather"}),
            }],
            &[ToolResult {
                call_id: "call_1".into(),
                content: "failed".into(),
                is_error: true,
            }],
            "",
        );

        let tool_use = render_message_for_model_context(&messages[0]);
        let tool_result = render_message_for_model_context(&messages[1]);

        assert!(tool_use.contains("[tool_use id=call_1 name=search"));
        assert!(tool_use.contains(r#""query":"weather""#));
        assert!(tool_result.contains("[tool_result tool_use_id=call_1 is_error=true"));
        assert!(tool_result.contains(r#""failed""#));
    }
}

//! Reading the platform's own documentation from a turn.
//!
//! The documents are the gateway's compiled-in set (`crate::shipped_docs`),
//! so an agent reads how Swage behaves from the gateway it runs on, whatever
//! its working directory. A document longer than the tool's output limit
//! pages by byte offset, the same shape `url_fetch` uses.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;
use swage_tool::tool::ToolDisplayHints;

use crate::shipped_docs;
use crate::tool::Tool;
use crate::tool::workspace::continuable_output_value;

pub const DOCS_READ_TOOL: &str = "docs_read";

pub struct DocsReadTool {
    pub max_output_bytes: usize,
}

#[async_trait]
impl Tool for DocsReadTool {
    fn name(&self) -> &str {
        DOCS_READ_TOOL
    }

    fn description(&self) -> &str {
        "Read Swage's own documentation: how sessions, topics, agents, permissions, tools, surfaces, schedules and workflows behave on this platform. Call with no arguments to list the documents with their descriptions; call with a document name to read it. Prefer this over guessing when asked how Swage itself works."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The document to read, as listed. Omit to list the documents."
                },
                "offset_bytes": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Continue a long document from this byte offset, as the previous result's follow_up gives it."
                }
            },
            "additionalProperties": false
        })
    }

    fn display_hints(&self) -> ToolDisplayHints {
        ToolDisplayHints {
            summary_param: Some("name"),
        }
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let offset_bytes = params
            .get("offset_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;

        let Some(name) = name else {
            return Ok(json!({
                "documents": shipped_docs::list().documents,
            }));
        };

        let document =
            shipped_docs::find_by_name(name).ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: format!(
                    "no document named {name:?}; the documents are: {}",
                    shipped_docs::document_names()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })?;
        let summary = shipped_docs::summary(document);

        let mut arguments = serde_json::Map::new();
        arguments.insert("name".to_owned(), json!(name));
        let mut value = continuable_output_value(
            document.contents,
            self.max_output_bytes,
            offset_bytes,
            self.name(),
            arguments,
        )
        .map_err(|message| ToolError::InvalidParameters {
            name: self.name().to_owned(),
            message,
        })?;
        let object = value
            .as_object_mut()
            .expect("continuable_output_value always returns an object");
        object.insert("name".to_owned(), json!(summary.name));
        object.insert("title".to_owned(), json!(summary.title));
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_tool::context::MockToolContext;

    fn context() -> MockToolContext {
        MockToolContext::new("gateway", uuid::Uuid::new_v4().to_string())
    }

    #[tokio::test]
    async fn without_a_name_the_tool_lists_every_document() {
        let output = DocsReadTool {
            max_output_bytes: 1 << 20,
        }
        .execute(json!({}), &context())
        .await
        .expect("listing");
        let documents = output["documents"].as_array().expect("documents array");
        assert_eq!(documents.len(), shipped_docs::SHIPPED_DOCUMENTS.len());
        assert!(
            documents
                .iter()
                .any(|entry| entry["name"] == "agents-and-permissions"
                    && entry["title"] == "Agents and permissions")
        );
    }

    #[tokio::test]
    async fn a_named_document_is_returned_whole_when_it_fits() {
        let output = DocsReadTool {
            max_output_bytes: 1 << 20,
        }
        .execute(json!({ "name": "first-session" }), &context())
        .await
        .expect("document");
        assert_eq!(output["name"], "first-session");
        assert_eq!(output["title"], "First session");
        assert_eq!(
            output["output"].as_str().expect("output text"),
            shipped_docs::find_by_name("first-session")
                .expect("shipped")
                .contents
        );
        assert!(output.get("follow_up").is_none());
    }

    #[tokio::test]
    async fn a_long_document_pages_with_a_follow_up_naming_this_tool() {
        let tool = DocsReadTool {
            max_output_bytes: 512,
        };
        let first = tool
            .execute(json!({ "name": "workflow-engine" }), &context())
            .await
            .expect("first page");
        assert_eq!(first["truncated"], true);
        assert_eq!(first["follow_up"]["tool"], DOCS_READ_TOOL);
        assert_eq!(first["follow_up"]["arguments"]["name"], "workflow-engine");
        let next = first["follow_up"]["arguments"]["offset_bytes"]
            .as_u64()
            .expect("next offset");
        assert!(next > 0);

        let second = tool
            .execute(
                json!({ "name": "workflow-engine", "offset_bytes": next }),
                &context(),
            )
            .await
            .expect("second page");
        let whole = shipped_docs::find_by_name("workflow-engine")
            .expect("shipped")
            .contents;
        assert!(whole[next as usize..].starts_with(second["output"].as_str().expect("text")));
    }

    #[tokio::test]
    async fn an_unknown_name_names_the_documents_that_exist() {
        let error = DocsReadTool {
            max_output_bytes: 1 << 20,
        }
        .execute(json!({ "name": "nope" }), &context())
        .await
        .expect_err("unknown document");
        let message = error.to_string();
        assert!(message.contains("no document named \"nope\""), "{message}");
        assert!(message.contains("agents-and-permissions"), "{message}");
    }
}

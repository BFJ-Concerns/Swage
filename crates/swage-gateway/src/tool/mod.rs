//! Tool system — trait, registry, and built-in tool implementations.
//!
//! Each built-in tool lives in its own submodule. The registry holds all
//! registered tools and provides lookup by name. The Tool trait defines the
//! execution contract.

pub mod workspace;
pub mod shell;
pub mod file_read;
pub mod file_write;
pub mod file_search;
pub mod file_list;
pub mod web_search;
pub mod url_fetch;
pub mod python_execute;
pub mod python_dispatch;

pub mod memory;

use std::sync::Arc;
use swage_core::tool::ToolDefinition;

/// Result of executing a tool call.
///
/// `truncated` is set when the output exceeded the configured size limit and
/// was cut short. `total_bytes` records the original output size so callers
/// can report it to the model.
pub struct ToolExecutionResult {
    pub content: String,
    pub is_error: bool,
    /// True when output was cut at the size limit.
    pub truncated: bool,
    /// Original byte count before truncation, if applicable.
    pub total_bytes: Option<usize>,
}

/// Contract for all built-in tools.
///
/// Implementors must be `Send + Sync` so they can be held behind an `Arc`
/// and shared across async tasks without cloning.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema (type: "object") describing the tool's input parameters.
    fn parameters(&self) -> serde_json::Value;
    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult;

    /// Convenience: build a ToolDefinition from the trait methods.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.parameters(),
        }
    }
}

/// Holds all registered tools and provides lookup by name.
///
/// The registry is populated at startup; after that it is read-only during
/// request handling so no locking is required on the hot path.
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Register a tool. Tools added later shadow earlier ones with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Look up a tool by its exact name. Returns `None` if not registered.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    /// Return ToolDefinitions for all registered tools, in registration order.
    /// Used to populate the `tools` field of an LlmRequest.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|t| t.definition()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// First mock tool for registry tests.
    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "Echo the input back" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            })
        }
        async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
            let text = arguments["text"].as_str().unwrap_or("").to_string();
            ToolExecutionResult { content: text, is_error: false, truncated: false, total_bytes: None }
        }
    }

    /// Second mock tool — tests that the registry holds multiple tools correctly.
    struct FailTool;

    #[async_trait::async_trait]
    impl Tool for FailTool {
        fn name(&self) -> &str { "fail" }
        fn description(&self) -> &str { "Always returns an error" }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        async fn execute(&self, _arguments: serde_json::Value) -> ToolExecutionResult {
            ToolExecutionResult {
                content: "intentional failure".into(),
                is_error: true,
                truncated: false,
                total_bytes: None,
            }
        }
    }

    #[test]
    fn registry_register_and_get_by_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        registry.register(Arc::new(FailTool));

        let echo = registry.get("echo");
        assert!(echo.is_some(), "should find echo tool by name");
        assert_eq!(echo.unwrap().name(), "echo");

        let fail = registry.get("fail");
        assert!(fail.is_some(), "should find fail tool by name");

        // Unknown name returns None
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn registry_definitions_matches_registered_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        registry.register(Arc::new(FailTool));

        let defs = registry.definitions();
        assert_eq!(defs.len(), 2);

        // definitions() must include both registered tools by name
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"fail"));
    }

    #[test]
    fn tool_definition_convenience_method() {
        // definition() must derive ToolDefinition from the trait methods
        let tool = EchoTool;
        let def = tool.definition();
        assert_eq!(def.name, "echo");
        assert_eq!(def.description, "Echo the input back");
        assert!(def.input_schema.is_object());
    }

    #[tokio::test]
    async fn tool_execute_returns_result() {
        let tool = EchoTool;
        let result = tool.execute(serde_json::json!({ "text": "hello" })).await;
        assert_eq!(result.content, "hello");
        assert!(!result.is_error);
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn tool_execute_error_result() {
        let tool = FailTool;
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_error);
        assert!(!result.truncated);
    }
}

//! Tool registry — registration, lookup, and schema generation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use swage_core::ToolError;

use crate::tool::Tool;

/// Built-in tool names that cannot be overridden by user-registered tools.
///
/// These names are reserved for Swage's built-in tool implementations.
/// Attempting to register a custom tool under any of these names returns an error.
const PROTECTED_NAMES: &[&str] = &[
    "agent_memory",
    "shell",
    "file_edit",
    "file_read",
    "file_write",
    "file_search",
    "file_list",
    "web_search",
    "url_fetch",
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_select",
    "browser_scroll",
    "memory",
    "python_execute",
    "workspace",
    "delegate",
    "schedule",
    "wake_me_later",
];

/// Registry of tools available for dispatch.
///
/// Maintains a protected namespace of built-in tool names that cannot be
/// overridden, and an open set of user-registered tools addressable by name.
///
/// The registry is not thread-safe by design — build it at startup, then share
/// it behind an `Arc<RwLock<ToolRegistry>>` if concurrent access is needed.
pub struct ToolRegistry {
    /// User-registered tools, keyed by tool name.
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Fast membership check for protected built-in names.
    protected: HashSet<&'static str>,
}

impl ToolRegistry {
    /// Create an empty registry with the protected namespace pre-loaded.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            protected: PROTECTED_NAMES.iter().copied().collect(),
        }
    }

    /// Register a tool by its declared name.
    ///
    /// Returns `ToolError::InvalidParameters` when:
    /// - The tool's name collides with a protected built-in name.
    /// - A tool with the same name has already been registered.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError> {
        let name = tool.name();

        if self.protected.contains(name) {
            return Err(ToolError::InvalidParameters {
                name: name.to_owned(),
                message: format!("'{name}' is a protected built-in name and cannot be overridden"),
            });
        }

        if self.tools.contains_key(name) {
            return Err(ToolError::InvalidParameters {
                name: name.to_owned(),
                message: format!("a tool named '{name}' is already registered"),
            });
        }

        self.tools.insert(name.to_owned(), tool);
        Ok(())
    }

    /// Register a built-in tool, bypassing the protected namespace check.
    ///
    /// Used at startup to register the platform's own built-in tools under their
    /// protected names. User-defined tools should use [`register`] which
    /// enforces the namespace.
    pub fn register_builtin(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError> {
        let name = tool.name();

        if self.tools.contains_key(name) {
            return Err(ToolError::InvalidParameters {
                name: name.to_owned(),
                message: format!("a tool named '{name}' is already registered"),
            });
        }

        self.tools.insert(name.to_owned(), tool);
        Ok(())
    }

    /// Look up a registered tool by exact name.
    ///
    /// Returns `None` for unknown names. Built-ins registered through
    /// [`register_builtin`](Self::register_builtin) live in the same map and
    /// are returned here like any other tool.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Return all registered tool names in sorted order.
    ///
    /// Includes built-ins registered through
    /// [`register_builtin`](Self::register_builtin).
    pub fn list(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        // Sorted for deterministic output, which helps tests and schema diffing.
        names.sort_unstable();
        names
    }

    /// Produce schema descriptors for all registered tools.
    ///
    /// Each entry is a JSON object with `name`, `description`, and `parameters`
    /// (the tool's JSON Schema). This is the format expected by providers in
    /// `ToolCompletionRequest.tool_definitions`.
    pub fn generate_schemas(&self) -> Vec<serde_json::Value> {
        // Delegate sorting to list() to avoid duplicating the sort+collect logic.
        self.list()
            .into_iter()
            .map(|name| {
                let tool = &self.tools[name];
                serde_json::json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters(),
                })
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use swage_core::ToolError;

    use crate::context::ToolContext;

    // -------------------------------------------------------------------------
    // Test double
    // -------------------------------------------------------------------------

    struct NamedTool {
        name: &'static str,
        description: &'static str,
    }

    impl NamedTool {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                description: "A test tool.",
            }
        }

        fn with_description(name: &'static str, description: &'static str) -> Self {
            Self { name, description }
        }
    }

    #[async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            self.description
        }

        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }

        async fn execute(
            &self,
            _params: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            Ok(json!(null))
        }
    }

    fn arc(name: &'static str) -> Arc<dyn Tool> {
        Arc::new(NamedTool::new(name))
    }

    // -------------------------------------------------------------------------
    // Registration
    // -------------------------------------------------------------------------

    #[test]
    fn register_and_get_tool_by_name() {
        let mut registry = ToolRegistry::new();
        registry.register(arc("calculator")).unwrap();

        let tool = registry.get("calculator").expect("tool should be found");
        assert_eq!(tool.name(), "calculator");
    }

    #[test]
    fn get_unknown_name_returns_none() {
        let registry = ToolRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn duplicate_registration_returns_error() {
        let mut registry = ToolRegistry::new();
        registry.register(arc("my_tool")).unwrap();

        let err = registry.register(arc("my_tool")).unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { ref name, .. } if name == "my_tool"));
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn all_protected_names_reject_registration() {
        let mut registry = ToolRegistry::new();
        for &name in PROTECTED_NAMES {
            let err = registry.register(arc(name)).unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidParameters { .. }),
                "expected InvalidParameters for protected name '{name}', got {err:?}"
            );
            assert!(
                err.to_string().contains("protected"),
                "error message should mention 'protected' for name '{name}'"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Listing
    // -------------------------------------------------------------------------

    #[test]
    fn list_returns_all_registered_names_sorted() {
        let mut registry = ToolRegistry::new();
        registry.register(arc("zebra")).unwrap();
        registry.register(arc("alpha")).unwrap();
        registry.register(arc("middle")).unwrap();

        let names = registry.list();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn list_on_empty_registry_returns_empty_vec() {
        let registry = ToolRegistry::new();
        assert!(registry.list().is_empty());
    }

    // -------------------------------------------------------------------------
    // Schema generation
    // -------------------------------------------------------------------------

    #[test]
    fn generate_schemas_contains_name_description_parameters() {
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(NamedTool::with_description(
                "echo",
                "Repeats input.",
            )))
            .unwrap();

        let schemas = registry.generate_schemas();
        assert_eq!(schemas.len(), 1);

        let schema = &schemas[0];
        assert_eq!(schema["name"], "echo");
        assert_eq!(schema["description"], "Repeats input.");
        assert!(schema["parameters"].is_object());
    }

    #[test]
    fn generate_schemas_sorted_by_name() {
        let mut registry = ToolRegistry::new();
        registry.register(arc("zzz")).unwrap();
        registry.register(arc("aaa")).unwrap();

        let schemas = registry.generate_schemas();
        assert_eq!(schemas[0]["name"], "aaa");
        assert_eq!(schemas[1]["name"], "zzz");
    }

    #[test]
    fn generate_schemas_on_empty_registry_returns_empty() {
        let registry = ToolRegistry::new();
        assert!(registry.generate_schemas().is_empty());
    }

    // -------------------------------------------------------------------------
    // Protected namespace does not appear in list or schemas
    // -------------------------------------------------------------------------

    #[test]
    fn protected_names_are_not_in_list() {
        let registry = ToolRegistry::new();
        for name in registry.list() {
            assert!(
                !PROTECTED_NAMES.contains(&name),
                "protected name '{name}' should not appear in list()"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Multiple tools
    // -------------------------------------------------------------------------

    #[test]
    fn multiple_tools_register_and_retrieve_correctly() {
        let mut registry = ToolRegistry::new();
        registry.register(arc("tool_a")).unwrap();
        registry.register(arc("tool_b")).unwrap();
        registry.register(arc("tool_c")).unwrap();

        assert!(registry.get("tool_a").is_some());
        assert!(registry.get("tool_b").is_some());
        assert!(registry.get("tool_c").is_some());
        assert_eq!(registry.list().len(), 3);
    }
}

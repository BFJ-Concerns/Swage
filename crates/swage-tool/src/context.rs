//! Tool execution context — the opaque boundary between tools and agent internals.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;


/// Information about the user who initiated the current session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
    /// Arbitrary metadata from the agent's auth layer.
    pub metadata: HashMap<String, Value>,
}

/// Context passed to every tool execution.
///
/// Provides the tool with access to agent and session identity, filesystem
/// constraints, and optional user identity — without exposing agent internals.
/// Must be object-safe so tools can accept `&dyn ToolContext`.
pub trait ToolContext: Send + Sync {
    fn agent_id(&self) -> &str;
    fn session_id(&self) -> &str;
    fn working_directory(&self) -> &Path;
    fn allowed_paths(&self) -> &[PathBuf];
    // Note: returns Option<&UserInfo> rather than Option<UserInfo> to avoid
    // allocating on every call, but this is fine because &UserInfo is a reference
    // into the concrete type's storage.
    fn user_info(&self) -> Option<&UserInfo>;
}

/// A serialisable snapshot of a ToolContext.
///
/// Built from `&dyn ToolContext` and round-trips through serde so it can be
/// sent across process boundaries (e.g. into a sandboxed subprocess).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerializableToolContext {
    pub agent_id: String,
    pub session_id: String,
    pub working_directory: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
    pub user_info: Option<UserInfo>,
}

impl SerializableToolContext {
    /// Snapshot an opaque context into a serialisable form.
    pub fn from_context(ctx: &dyn ToolContext) -> Self {
        Self {
            agent_id: ctx.agent_id().to_owned(),
            session_id: ctx.session_id().to_owned(),
            working_directory: ctx.working_directory().to_owned(),
            allowed_paths: ctx.allowed_paths().to_owned(),
            user_info: ctx.user_info().cloned(),
        }
    }
}

impl ToolContext for SerializableToolContext {
    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    fn allowed_paths(&self) -> &[PathBuf] {
        &self.allowed_paths
    }

    fn user_info(&self) -> Option<&UserInfo> {
        self.user_info.as_ref()
    }
}

/// A concrete ToolContext for unit testing.
///
/// All fields are public so tests can construct it directly, without
/// depending on any real agent infrastructure.
#[derive(Debug, Clone)]
pub struct MockToolContext {
    pub agent_id: String,
    pub session_id: String,
    pub working_directory: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
    pub user_info: Option<UserInfo>,
}

impl MockToolContext {
    /// Minimal constructor for tests that only care about identity fields.
    pub fn new(agent_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            working_directory: PathBuf::from("/tmp"),
            allowed_paths: vec![],
            user_info: None,
        }
    }
}

impl ToolContext for MockToolContext {
    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    fn allowed_paths(&self) -> &[PathBuf] {
        &self.allowed_paths
    }

    fn user_info(&self) -> Option<&UserInfo> {
        self.user_info.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_mock() -> MockToolContext {
        MockToolContext {
            agent_id: "agent-1".to_string(),
            session_id: "sess-abc".to_string(),
            working_directory: PathBuf::from("/home/user/project"),
            allowed_paths: vec![
                PathBuf::from("/home/user/project"),
                PathBuf::from("/tmp/scratch"),
            ],
            user_info: Some(UserInfo {
                id: "u-42".to_string(),
                name: "Alice".to_string(),
                metadata: HashMap::from([
                    ("role".to_string(), Value::String("admin".to_string())),
                ]),
            }),
        }
    }

    #[test]
    fn mock_context_trait_methods_return_correct_values() {
        let ctx = make_mock();

        assert_eq!(ctx.agent_id(), "agent-1");
        assert_eq!(ctx.session_id(), "sess-abc");
        assert_eq!(ctx.working_directory(), Path::new("/home/user/project"));
        assert_eq!(ctx.allowed_paths().len(), 2);

        let user = ctx.user_info().expect("user_info should be Some");
        assert_eq!(user.id, "u-42");
        assert_eq!(user.name, "Alice");
        assert_eq!(user.metadata["role"], Value::String("admin".to_string()));
    }

    #[test]
    fn mock_context_is_object_safe() {
        // Object safety proof: MockToolContext is usable as &dyn ToolContext.
        let ctx = MockToolContext::new("agent-x", "sess-y");
        let dyn_ctx: &dyn ToolContext = &ctx;
        assert_eq!(dyn_ctx.agent_id(), "agent-x");
        assert_eq!(dyn_ctx.session_id(), "sess-y");
    }

    #[test]
    fn serializable_context_round_trips() {
        let mock = make_mock();
        let snap = SerializableToolContext::from_context(&mock);

        let json = serde_json::to_string(&snap).expect("serialise");
        let restored: SerializableToolContext =
            serde_json::from_str(&json).expect("deserialise");

        assert_eq!(restored.agent_id, "agent-1");
        assert_eq!(restored.session_id, "sess-abc");
        assert_eq!(
            restored.working_directory,
            PathBuf::from("/home/user/project")
        );
        assert_eq!(restored.allowed_paths.len(), 2);

        let user = restored.user_info.expect("user_info round-trips");
        assert_eq!(user.id, "u-42");
        assert_eq!(user.name, "Alice");
    }

    #[test]
    fn serializable_context_is_also_a_tool_context() {
        let mock = make_mock();
        let snap = SerializableToolContext::from_context(&mock);
        let dyn_ctx: &dyn ToolContext = &snap;

        assert_eq!(dyn_ctx.agent_id(), "agent-1");
        assert_eq!(dyn_ctx.session_id(), "sess-abc");
    }

    #[test]
    fn mock_without_user_info_returns_none() {
        let ctx = MockToolContext::new("agent-z", "sess-0");
        assert!(ctx.user_info().is_none());
    }
}

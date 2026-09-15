//! ToolContext implementation for the agent runtime.
//!
//! Provides tools with agent identity, session context, and filesystem
//! constraints during execution.

use std::path::{Path, PathBuf};

use swage_tool::context::{ToolContext, UserInfo};
use uuid::Uuid;

/// Immutable execution metadata for a single turn.
#[derive(Debug, Clone, Copy)]
pub struct TurnExecutionContext {
    pub session_id: Uuid,
    pub thread_id: Uuid,
    pub turn_id: Uuid,
    pub topic_id: Option<Uuid>,
}

/// Context for tool execution within an agent turn.
///
/// Constructed by the agent when dispatching tools, populated from the
/// agent's config and active session state.
#[derive(Clone)]
pub struct AgentToolContext {
    pub agent_id: String,
    pub session_id: String,
    pub thread_id: Option<Uuid>,
    pub working_directory: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
    pub user_info: Option<UserInfo>,
    /// The LLM's call_id for the current tool invocation.
    pub call_id: Option<String>,
    /// The turn_id for the currently executing turn.
    pub turn_id: Option<uuid::Uuid>,
}

impl AgentToolContext {
    /// Create a new context from agent identity, session, and working directory.
    ///
    /// Starts with no path restrictions, no user info, and no turn/call context.
    /// Callers set turn-specific fields via the builder methods.
    pub fn new(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        working_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            thread_id: None,
            working_directory: working_directory.into(),
            allowed_paths: Vec::new(),
            user_info: None,
            call_id: None,
            turn_id: None,
        }
    }

    /// Set the call_id for the current tool invocation.
    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    /// Set the turn_id for the currently executing turn.
    pub fn with_turn_id(mut self, turn_id: uuid::Uuid) -> Self {
        self.turn_id = Some(turn_id);
        self
    }

    /// Set the thread_id for the current turn.
    pub fn with_thread_id(mut self, thread_id: Uuid) -> Self {
        self.thread_id = Some(thread_id);
        self
    }
}

impl ToolContext for AgentToolContext {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

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

    fn call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
    }

    fn turn_id(&self) -> Option<uuid::Uuid> {
        self.turn_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // -----------------------------------------------------------------------
    // Constructor
    // -----------------------------------------------------------------------

    #[test]
    fn new_sets_identity_fields() {
        let ctx = AgentToolContext::new("agent-1", "session-abc", "/tmp/work");
        assert_eq!(ctx.agent_id, "agent-1");
        assert_eq!(ctx.session_id, "session-abc");
        assert_eq!(ctx.working_directory, PathBuf::from("/tmp/work"));
    }

    #[test]
    fn new_defaults_to_empty_allowed_paths() {
        let ctx = AgentToolContext::new("a", "s", "/w");
        assert!(ctx.allowed_paths.is_empty());
    }

    #[test]
    fn new_defaults_to_no_user_info() {
        let ctx = AgentToolContext::new("a", "s", "/w");
        assert!(ctx.user_info.is_none());
    }

    // -----------------------------------------------------------------------
    // ToolContext trait methods
    // -----------------------------------------------------------------------

    #[test]
    fn trait_agent_id() {
        let ctx = AgentToolContext::new("swage-default", "sess-1", "/home");
        let tc: &dyn ToolContext = &ctx;
        assert_eq!(tc.agent_id(), "swage-default");
    }

    #[test]
    fn trait_session_id() {
        let ctx = AgentToolContext::new("a", "sess-42", "/home");
        let tc: &dyn ToolContext = &ctx;
        assert_eq!(tc.session_id(), "sess-42");
    }

    #[test]
    fn trait_working_directory() {
        let ctx = AgentToolContext::new("a", "s", "/topic/root");
        let tc: &dyn ToolContext = &ctx;
        assert_eq!(tc.working_directory(), Path::new("/topic/root"));
    }

    #[test]
    fn trait_allowed_paths_empty() {
        let ctx = AgentToolContext::new("a", "s", "/w");
        let tc: &dyn ToolContext = &ctx;
        assert!(tc.allowed_paths().is_empty());
    }

    #[test]
    fn trait_allowed_paths_populated() {
        let mut ctx = AgentToolContext::new("a", "s", "/w");
        ctx.allowed_paths = vec![PathBuf::from("/data"), PathBuf::from("/config")];
        let tc: &dyn ToolContext = &ctx;
        assert_eq!(tc.allowed_paths().len(), 2);
        assert_eq!(tc.allowed_paths()[0], Path::new("/data"));
        assert_eq!(tc.allowed_paths()[1], Path::new("/config"));
    }

    #[test]
    fn trait_user_info_none() {
        let ctx = AgentToolContext::new("a", "s", "/w");
        let tc: &dyn ToolContext = &ctx;
        assert!(tc.user_info().is_none());
    }

    #[test]
    fn trait_user_info_some() {
        let mut ctx = AgentToolContext::new("a", "s", "/w");
        ctx.user_info = Some(UserInfo {
            id: "user-7".into(),
            name: "Test User".into(),
            metadata: HashMap::new(),
        });
        let tc: &dyn ToolContext = &ctx;
        let info = tc.user_info().expect("should have user info");
        assert_eq!(info.id, "user-7");
        assert_eq!(info.name, "Test User");
        assert!(info.metadata.is_empty());
    }

    // -----------------------------------------------------------------------
    // Object safety — compile-time proof that &dyn ToolContext works
    // -----------------------------------------------------------------------

    #[test]
    fn is_object_safe() {
        // If this compiles, ToolContext is object-safe and AgentToolContext
        // can be used behind a trait object.
        fn accept_ctx(_ctx: &dyn ToolContext) {}
        let ctx = AgentToolContext::new("a", "s", "/w");
        accept_ctx(&ctx);
    }
}

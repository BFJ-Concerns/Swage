//! MemoryTool — read and write a session scope's shared memory files.
//!
//! The calling session's scope owns the directory beneath the Swage data root.
//!
//! Operations:
//!   - **read**: read a specific file (with "file" param) or all files (no param)
//!   - **write**: create or overwrite a named file
//!   - **list**: return names of all memory files in sorted order

use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::tool::Tool;

// ---------------------------------------------------------------------------
// File name validation
// ---------------------------------------------------------------------------

/// Validate a memory file name — returns an error string if invalid.
///
/// Allowed: alphanumeric, hyphens, underscores. Dots, slashes, and ".." are
/// rejected to prevent path traversal.  The ".md" extension is appended by
/// the tool; callers must not include it.
fn validate_file_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("file name must not be empty".into());
    }
    // Block path traversal regardless of character set.
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Some(format!(
            "file name '{name}' is invalid: must not contain path separators or '..'"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Some(format!(
            "file name '{name}' is invalid: only alphanumeric characters, \
             hyphens, and underscores are allowed"
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// MemoryTool
// ---------------------------------------------------------------------------

pub struct MemoryTool;

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Read and write the shared memory files of this session's scope — its topic's, or the general memory when no topic is selected: named markdown notes that persist there and are visible to every agent working in that scope. For private records only you will search later, use the agent_memory tool instead."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["read", "write", "list"],
                    "description": "The operation to perform."
                },
                "file": {
                    "type": "string",
                    "description": "Memory file name without the .md extension. \
                                    Required for write; optional for read (reads all files if omitted)."
                },
                "content": {
                    "type": "string",
                    "description": "Markdown content to write. Required for write."
                }
            },
            "required": ["operation"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let failed = |message: String| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message,
        };
        let state = super::require_gateway_state(
            ctx,
            self.name(),
            "memory requires gateway execution context",
        )?;
        let session_id = ctx
            .session_id()
            .parse::<Uuid>()
            .map_err(|_| failed("the calling session has no usable id".into()))?;
        let session = state
            .storage
            .get_session(session_id)
            .await
            .map_err(|error| failed(error.to_string()))?
            .ok_or_else(|| failed("the calling session no longer exists".into()))?;
        let topic = state
            .topic_store
            .get(session.topic_id)
            .await
            .map_err(|error| failed(error.to_string()))?
            .ok_or_else(|| failed("the session's topic no longer exists".into()))?;
        let memory_dir = topic.location().data_dir(&state.config_root).join("memory");

        let operation =
            params["operation"]
                .as_str()
                .ok_or_else(|| ToolError::InvalidParameters {
                    name: self.name().to_owned(),
                    message: "Missing required argument: operation".to_owned(),
                })?;

        match operation {
            "read" => self.read(&memory_dir, &params).await,
            "write" => self.write(&memory_dir, &params).await,
            "list" => self.list(&memory_dir).await,
            other => Err(ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: format!("Unknown operation '{other}': must be read, write, or list"),
            }),
        }
    }
}

impl MemoryTool {
    // -----------------------------------------------------------------------
    // read
    // -----------------------------------------------------------------------

    async fn read(&self, memory_dir: &Path, params: &Value) -> Result<Value, ToolError> {
        match params["file"].as_str() {
            Some(name) => {
                // Read a specific named file.
                if let Some(err) = validate_file_name(name) {
                    return Err(ToolError::InvalidParameters {
                        name: self.name().to_owned(),
                        message: err,
                    });
                }
                let path = memory_dir.join(format!("{name}.md"));
                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => Ok(json!({"output": content})),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        Err(ToolError::ExecutionFailed {
                            name: self.name().to_owned(),
                            message: format!("Memory file '{name}.md' not found"),
                        })
                    }
                    Err(e) => Err(ToolError::ExecutionFailed {
                        name: self.name().to_owned(),
                        message: format!("Failed to read '{name}.md': {e}"),
                    }),
                }
            }
            None => {
                // Read all .md files, prefixed by their filename as a header.
                self.read_all(memory_dir).await
            }
        }
    }

    async fn read_all(&self, memory_dir: &Path) -> Result<Value, ToolError> {
        let mut dir = match tokio::fs::read_dir(memory_dir).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({
                    "output": "No memory files found (memory directory does not exist yet)"
                }));
            }
            Err(e) => {
                return Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to read memory directory: {e}"),
                });
            }
            Ok(d) => d,
        };

        let mut parts: Vec<(String, String)> = Vec::new();
        loop {
            match dir.next_entry().await {
                Err(_) | Ok(None) => break,
                Ok(Some(entry)) => {
                    let fname = entry.file_name();
                    let name = fname.to_string_lossy();
                    if !name.ends_with(".md") {
                        continue;
                    }
                    if let Ok(content) = tokio::fs::read_to_string(entry.path()).await {
                        parts.push((name.into_owned(), content));
                    }
                }
            }
        }

        if parts.is_empty() {
            return Ok(json!({"output": "No memory files found"}));
        }

        // Sort by filename for deterministic output.
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        let combined = parts
            .into_iter()
            .map(|(name, content)| format!("# {name}\n\n{content}"))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        Ok(json!({"output": combined}))
    }

    // -----------------------------------------------------------------------
    // write
    // -----------------------------------------------------------------------

    async fn write(&self, memory_dir: &Path, params: &Value) -> Result<Value, ToolError> {
        let name = params["file"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "write requires a 'file' argument".to_owned(),
            })?;

        let content = params["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "write requires a 'content' argument".to_owned(),
            })?;

        if let Some(err) = validate_file_name(name) {
            return Err(ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: err,
            });
        }

        // Create the memory directory on first write if it doesn't exist.
        tokio::fs::create_dir_all(memory_dir)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Failed to create memory directory: {e}"),
            })?;

        let path = memory_dir.join(format!("{name}.md"));
        tokio::fs::write(&path, content)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Failed to write '{name}.md': {e}"),
            })?;

        Ok(json!({"output": format!("Wrote {name}.md ({} bytes)", content.len())}))
    }

    // -----------------------------------------------------------------------
    // list
    // -----------------------------------------------------------------------

    async fn list(&self, memory_dir: &Path) -> Result<Value, ToolError> {
        let mut dir = match tokio::fs::read_dir(memory_dir).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({
                    "output": "No memory files found (memory directory does not exist yet)"
                }));
            }
            Err(e) => {
                return Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to read memory directory: {e}"),
                });
            }
            Ok(d) => d,
        };

        let mut names: Vec<String> = Vec::new();
        loop {
            match dir.next_entry().await {
                Err(_) | Ok(None) => break,
                Ok(Some(entry)) => {
                    let fname = entry.file_name();
                    let name = fname.to_string_lossy();
                    if name.ends_with(".md") {
                        names.push(name.into_owned());
                    }
                }
            }
        }

        if names.is_empty() {
            return Ok(json!({"output": "No memory files found"}));
        }

        names.sort();
        Ok(json!({"output": names.join("\n")}))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use tempfile::TempDir;

    use swage_provider::LlmProvider;
    use swage_storage::store::Storage;
    use swage_storage::{
        NewSession, NewTopic, SessionOrigin, SessionStore, SqliteStorage, TopicStore,
    };
    use swage_tool::context::MockToolContext;
    use tokio::sync::mpsc;

    use crate::state::AppState;
    use crate::tool_executor::GatewayToolContext;
    use crate::turn::load_topic_memory;
    use crate::turn::tests::MockProvider;

    use super::*;

    /// A real session and scope row, with a distinct scratch working directory.
    async fn ctx_for_scope(dir: &TempDir, named_topic: bool) -> GatewayToolContext {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = if named_topic {
            storage
                .create(NewTopic {
                    name: "My topic".into(),
                    slug: "my-topic".into(),
                })
                .await
                .unwrap()
        } else {
            storage.general_scope().await.unwrap()
        };
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "test-agent".into(),
                origin: SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let provider = Arc::new(MockProvider::new(vec![]));
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            dir.path().to_path_buf(),
        );
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let base = MockToolContext {
            agent_id: "test-agent".into(),
            session_id: session.id.to_string(),
            working_directory: scratch,
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        GatewayToolContext::new(&base, ws_tx, storage as Arc<dyn Storage>)
            .with_app_state(Some(state))
    }

    async fn ctx_with_root(dir: &TempDir) -> GatewayToolContext {
        ctx_for_scope(dir, false).await
    }

    /// Path where MemoryTool will look for files given ctx_with_root.
    fn memory_dir(dir: &TempDir) -> PathBuf {
        dir.path().join("general").join("memory")
    }

    /// Convenience: create the memory directory and write a file into it.
    fn seed_file(dir: &TempDir, name: &str, content: &[u8]) {
        let mdir = memory_dir(dir);
        std::fs::create_dir_all(&mdir).unwrap();
        std::fs::write(mdir.join(name), content).unwrap();
    }

    // -----------------------------------------------------------------------
    // validate_file_name
    // -----------------------------------------------------------------------

    #[test]
    fn valid_file_names_pass() {
        assert!(validate_file_name("notes").is_none());
        assert!(validate_file_name("my-context").is_none());
        assert!(validate_file_name("user_prefs").is_none());
        assert!(validate_file_name("abc123").is_none());
    }

    #[test]
    fn invalid_file_names_rejected() {
        assert!(validate_file_name("").is_some(), "empty should be rejected");
        assert!(
            validate_file_name("../etc/passwd").is_some(),
            ".. should be rejected"
        );
        assert!(
            validate_file_name("foo/bar").is_some(),
            "slash should be rejected"
        );
        assert!(
            validate_file_name("foo\\bar").is_some(),
            "backslash should be rejected"
        );
        assert!(
            validate_file_name("foo.bar").is_some(),
            "dot should be rejected"
        );
        assert!(
            validate_file_name("hello world").is_some(),
            "topic should be rejected"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn general_memory_written_by_one_session_is_read_by_another() {
        let dir = TempDir::new().unwrap();
        let first = ctx_with_root(&dir).await;
        MemoryTool.execute(
            json!({"operation": "write", "file": "general_note", "content": "General scope note"}),
            &first,
        ).await.unwrap();

        let second = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({"operation": "list"}), &second)
            .await
            .unwrap();
        assert_eq!(result["output"], "general_note.md");
        let result = MemoryTool
            .execute(
                json!({"operation": "read", "file": "general_note"}),
                &second,
            )
            .await
            .unwrap();
        assert_eq!(result["output"], "General scope note");
        assert_eq!(
            load_topic_memory(dir.path(), &swage_core::ScopeLocation::General).await,
            "General scope note",
        );
        assert!(memory_dir(&dir).join("general_note.md").exists());
        assert!(
            !dir.path()
                .join("scratch/.swage/memory/general_note.md")
                .exists()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn topic_memory_is_read_only_by_sessions_in_that_topic() {
        let dir = TempDir::new().unwrap();
        let general = ctx_with_root(&dir).await;
        let topic = ctx_for_scope(&dir, true).await;
        MemoryTool
            .execute(
                json!({"operation": "write", "file": "general_note", "content": "General note"}),
                &general,
            )
            .await
            .unwrap();
        MemoryTool
            .execute(
                json!({"operation": "write", "file": "topic_note", "content": "Topic note"}),
                &topic,
            )
            .await
            .unwrap();

        let next_topic_session = ctx_for_scope(&dir, true).await;
        let topic_list = MemoryTool
            .execute(json!({"operation": "list"}), &next_topic_session)
            .await
            .unwrap();
        assert_eq!(topic_list["output"], "topic_note.md");
        let topic_read = MemoryTool
            .execute(
                json!({"operation": "read", "file": "topic_note"}),
                &next_topic_session,
            )
            .await
            .unwrap();
        assert_eq!(topic_read["output"], "Topic note");
        let general_list = MemoryTool
            .execute(json!({"operation": "list"}), &general)
            .await
            .unwrap();
        assert_eq!(general_list["output"], "general_note.md");
        assert!(
            MemoryTool
                .execute(
                    json!({"operation": "read", "file": "general_note"}),
                    &next_topic_session
                )
                .await
                .is_err()
        );
        assert_eq!(
            load_topic_memory(dir.path(), &swage_core::ScopeLocation::topic("my-topic")).await,
            "Topic note",
        );
        assert!(
            dir.path()
                .join("topics/my-topic/memory/topic_note.md")
                .exists()
        );
        assert!(
            !dir.path()
                .join("scratch/.swage/memory/topic_note.md")
                .exists()
        );
    }

    // -----------------------------------------------------------------------
    // write
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn write_creates_file_with_md_extension() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(
                json!({ "operation": "write", "file": "notes", "content": "# My Notes\n\nHello!" }),
                &ctx,
            )
            .await
            .unwrap();

        let output = result["output"].as_str().unwrap();
        assert!(
            output.contains("notes.md"),
            "confirmation should mention the file"
        );

        let written = std::fs::read_to_string(memory_dir(&dir).join("notes.md")).unwrap();
        assert_eq!(written, "# My Notes\n\nHello!");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "notes.md", b"old content");

        let ctx = ctx_with_root(&dir).await;
        MemoryTool
            .execute(
                json!({ "operation": "write", "file": "notes", "content": "new content" }),
                &ctx,
            )
            .await
            .unwrap();

        let written = std::fs::read_to_string(memory_dir(&dir).join("notes.md")).unwrap();
        assert_eq!(written, "new content");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_creates_directory_if_missing() {
        let dir = TempDir::new().unwrap();
        // The .swage/memory directory must not exist yet.
        assert!(!memory_dir(&dir).exists());

        let ctx = ctx_with_root(&dir).await;
        MemoryTool
            .execute(
                json!({ "operation": "write", "file": "context", "content": "data" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(memory_dir(&dir).join("context.md").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_error_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let err = MemoryTool
            .execute(json!({ "operation": "write", "content": "data" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("file"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_error_when_content_missing() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let err = MemoryTool
            .execute(json!({ "operation": "write", "file": "notes" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("content"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_rejects_invalid_file_name() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let err = MemoryTool
            .execute(
                json!({ "operation": "write", "file": "../evil", "content": "bad" }),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }

    // -----------------------------------------------------------------------
    // read — specific file
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn read_specific_file_returns_content() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "facts.md", b"Important fact.");

        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({ "operation": "read", "file": "facts" }), &ctx)
            .await
            .unwrap();

        assert_eq!(result["output"].as_str().unwrap(), "Important fact.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_specific_file_not_found_returns_error() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let err = MemoryTool
            .execute(json!({ "operation": "read", "file": "nonexistent" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("nonexistent.md"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_rejects_invalid_file_name() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let err = MemoryTool
            .execute(json!({ "operation": "read", "file": "foo/bar" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }

    // -----------------------------------------------------------------------
    // read — all files
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn read_all_returns_all_md_files_with_headers() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "alpha.md", b"A content");
        seed_file(&dir, "beta.md", b"B content");
        // Non-.md file — should be ignored.
        std::fs::write(memory_dir(&dir).join("ignored.txt"), b"ignored").unwrap();

        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({ "operation": "read" }), &ctx)
            .await
            .unwrap();

        let output = result["output"].as_str().unwrap();
        assert!(output.contains("alpha.md"), "should include alpha filename");
        assert!(output.contains("A content"), "should include alpha content");
        assert!(output.contains("beta.md"), "should include beta filename");
        assert!(output.contains("B content"), "should include beta content");
        assert!(!output.contains("ignored"), "txt file should be skipped");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_all_empty_directory_returns_no_files_message() {
        let dir = TempDir::new().unwrap();
        // Create the memory dir but leave it empty.
        std::fs::create_dir_all(memory_dir(&dir)).unwrap();

        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({ "operation": "read" }), &ctx)
            .await
            .unwrap();
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("No memory files")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_all_missing_directory_returns_friendly_message() {
        let dir = TempDir::new().unwrap();
        // Don't create the memory dir — it shouldn't exist.
        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({ "operation": "read" }), &ctx)
            .await
            .unwrap();
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("does not exist")
        );
    }

    // -----------------------------------------------------------------------
    // list
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn list_returns_sorted_md_filenames() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "zebra.md", b"z");
        seed_file(&dir, "alpha.md", b"a");
        std::fs::write(memory_dir(&dir).join("ignored.txt"), b"x").unwrap();

        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({ "operation": "list" }), &ctx)
            .await
            .unwrap();

        let output = result["output"].as_str().unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines, ["alpha.md", "zebra.md"], "should be sorted, no txt");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_empty_directory_returns_no_files_message() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(memory_dir(&dir)).unwrap();

        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({ "operation": "list" }), &ctx)
            .await
            .unwrap();
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("No memory files")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_missing_directory_returns_friendly_message() {
        let dir = TempDir::new().unwrap();
        // Don't create the memory dir.
        let ctx = ctx_with_root(&dir).await;
        let result = MemoryTool
            .execute(json!({ "operation": "list" }), &ctx)
            .await
            .unwrap();
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("does not exist")
        );
    }

    // -----------------------------------------------------------------------
    // missing operation
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_operation_returns_error() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let err = MemoryTool.execute(json!({}), &ctx).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }

    // -----------------------------------------------------------------------
    // unknown operation
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_operation_returns_error() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir).await;
        let err = MemoryTool
            .execute(json!({ "operation": "delete" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("delete"), "got: {err}");
    }
}

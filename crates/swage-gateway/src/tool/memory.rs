//! MemoryTool — read and write persistent per-project memory files.
//!
//! The memory directory is resolved from the `PROJECT_CONTEXT` task-local set
//! by the turn executor before each tool invocation.  This keeps the tool
//! stateless: it holds no project reference itself, making it trivially thread-
//! safe and requiring no constructor arguments.
//!
//! Operations:
//!   - **read**: read a specific file (with "file" param) or all files (no param)
//!   - **write**: create or overwrite a named file
//!   - **list**: return names of all memory files in sorted order

use std::path::Path;

use async_trait::async_trait;
use serde_json::json;

use crate::context::PROJECT_CONTEXT;
use crate::tool::{Tool, ToolExecutionResult};

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
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
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
        "Read and write persistent per-project memory files."
    }

    fn parameters(&self) -> serde_json::Value {
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

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        // Resolve the memory directory from the project context task-local.
        // Returns an error if called outside a turn (no task-local set).
        let memory_dir = match PROJECT_CONTEXT.try_with(|ctx| ctx.memory_path.clone()) {
            Ok(path) => path,
            Err(_) => {
                return ToolExecutionResult {
                    content: "memory tool requires a project context \
                              (must be called within a turn)"
                        .into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        let operation = match arguments["operation"].as_str() {
            Some(op) => op,
            None => {
                return ToolExecutionResult {
                    content: "Missing required argument: operation".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        match operation {
            "read" => self.read(&memory_dir, &arguments).await,
            "write" => self.write(&memory_dir, &arguments).await,
            "list" => self.list(&memory_dir).await,
            other => ToolExecutionResult {
                content: format!(
                    "Unknown operation '{other}': must be read, write, or list"
                ),
                is_error: true,
                truncated: false,
                total_bytes: None,
            },
        }
    }
}

impl MemoryTool {
    // -----------------------------------------------------------------------
    // read
    // -----------------------------------------------------------------------

    async fn read(&self, memory_dir: &Path, arguments: &serde_json::Value) -> ToolExecutionResult {
        match arguments["file"].as_str() {
            Some(name) => {
                // Read a specific named file.
                if let Some(err) = validate_file_name(name) {
                    return ToolExecutionResult {
                        content: err,
                        is_error: true,
                        truncated: false,
                        total_bytes: None,
                    };
                }
                let path = memory_dir.join(format!("{name}.md"));
                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => ToolExecutionResult {
                        content,
                        is_error: false,
                        truncated: false,
                        total_bytes: None,
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => ToolExecutionResult {
                        content: format!("Memory file '{name}.md' not found"),
                        is_error: true,
                        truncated: false,
                        total_bytes: None,
                    },
                    Err(e) => ToolExecutionResult {
                        content: format!("Failed to read '{name}.md': {e}"),
                        is_error: true,
                        truncated: false,
                        total_bytes: None,
                    },
                }
            }
            None => {
                // Read all .md files, prefixed by their filename as a header.
                self.read_all(memory_dir).await
            }
        }
    }

    async fn read_all(&self, memory_dir: &Path) -> ToolExecutionResult {
        let mut dir = match tokio::fs::read_dir(memory_dir).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolExecutionResult {
                    content: "No memory files found (memory directory does not exist yet)".into(),
                    is_error: false,
                    truncated: false,
                    total_bytes: None,
                };
            }
            Err(e) => {
                return ToolExecutionResult {
                    content: format!("Failed to read memory directory: {e}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
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
            return ToolExecutionResult {
                content: "No memory files found".into(),
                is_error: false,
                truncated: false,
                total_bytes: None,
            };
        }

        // Sort by filename for deterministic output.
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        let combined = parts
            .into_iter()
            .map(|(name, content)| format!("# {name}\n\n{content}"))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        ToolExecutionResult {
            content: combined,
            is_error: false,
            truncated: false,
            total_bytes: None,
        }
    }

    // -----------------------------------------------------------------------
    // write
    // -----------------------------------------------------------------------

    async fn write(
        &self,
        memory_dir: &Path,
        arguments: &serde_json::Value,
    ) -> ToolExecutionResult {
        let name = match arguments["file"].as_str() {
            Some(n) => n,
            None => {
                return ToolExecutionResult {
                    content: "write requires a 'file' argument".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        let content = match arguments["content"].as_str() {
            Some(c) => c,
            None => {
                return ToolExecutionResult {
                    content: "write requires a 'content' argument".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        if let Some(err) = validate_file_name(name) {
            return ToolExecutionResult {
                content: err,
                is_error: true,
                truncated: false,
                total_bytes: None,
            };
        }

        // Create the memory directory on first write if it doesn't exist.
        if let Err(e) = tokio::fs::create_dir_all(memory_dir).await {
            return ToolExecutionResult {
                content: format!("Failed to create memory directory: {e}"),
                is_error: true,
                truncated: false,
                total_bytes: None,
            };
        }

        let path = memory_dir.join(format!("{name}.md"));
        match tokio::fs::write(&path, content).await {
            Ok(()) => ToolExecutionResult {
                content: format!("Wrote {name}.md ({} bytes)", content.len()),
                is_error: false,
                truncated: false,
                total_bytes: None,
            },
            Err(e) => ToolExecutionResult {
                content: format!("Failed to write '{name}.md': {e}"),
                is_error: true,
                truncated: false,
                total_bytes: None,
            },
        }
    }

    // -----------------------------------------------------------------------
    // list
    // -----------------------------------------------------------------------

    async fn list(&self, memory_dir: &Path) -> ToolExecutionResult {
        let mut dir = match tokio::fs::read_dir(memory_dir).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolExecutionResult {
                    content: "No memory files found (memory directory does not exist yet)".into(),
                    is_error: false,
                    truncated: false,
                    total_bytes: None,
                };
            }
            Err(e) => {
                return ToolExecutionResult {
                    content: format!("Failed to read memory directory: {e}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
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
            return ToolExecutionResult {
                content: "No memory files found".into(),
                is_error: false,
                truncated: false,
                total_bytes: None,
            };
        }

        names.sort();
        ToolExecutionResult {
            content: names.join("\n"),
            is_error: false,
            truncated: false,
            total_bytes: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::context::{ProjectContext, PROJECT_CONTEXT};

    use super::*;

    /// Run a tool operation with PROJECT_CONTEXT set to `memory_dir`.
    ///
    /// All tests use this helper so the task-local is in scope for the async
    /// closure, exactly as it would be in a real turn execution.
    async fn run_with_context(
        memory_dir: PathBuf,
        args: serde_json::Value,
    ) -> ToolExecutionResult {
        let ctx = ProjectContext {
            project_id: Uuid::new_v4(),
            project_slug: "test-project".to_string(),
            memory_path: memory_dir,
        };
        PROJECT_CONTEXT
            .scope(ctx, async move { MemoryTool.execute(args).await })
            .await
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
        assert!(validate_file_name("../etc/passwd").is_some(), ".. should be rejected");
        assert!(validate_file_name("foo/bar").is_some(), "slash should be rejected");
        assert!(validate_file_name("foo\\bar").is_some(), "backslash should be rejected");
        assert!(validate_file_name("foo.bar").is_some(), "dot should be rejected");
        assert!(validate_file_name("hello world").is_some(), "space should be rejected");
    }

    // -----------------------------------------------------------------------
    // write
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn write_creates_file_with_md_extension() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "write", "file": "notes", "content": "# My Notes\n\nHello!" }),
        )
        .await;

        assert!(!result.is_error, "write should succeed: {}", result.content);
        assert!(result.content.contains("notes.md"), "confirmation should mention the file");

        let written = std::fs::read_to_string(dir.path().join("notes.md")).unwrap();
        assert_eq!(written, "# My Notes\n\nHello!");
    }

    #[tokio::test]
    async fn write_overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.md"), b"old content").unwrap();

        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "write", "file": "notes", "content": "new content" }),
        )
        .await;

        assert!(!result.is_error);
        let written = std::fs::read_to_string(dir.path().join("notes.md")).unwrap();
        assert_eq!(written, "new content");
    }

    #[tokio::test]
    async fn write_creates_directory_if_missing() {
        let dir = TempDir::new().unwrap();
        // Point to a subdirectory that doesn't exist yet.
        let memory_dir = dir.path().join("memory");
        assert!(!memory_dir.exists());

        let result = run_with_context(
            memory_dir.clone(),
            json!({ "operation": "write", "file": "context", "content": "data" }),
        )
        .await;

        assert!(!result.is_error, "write should create the directory: {}", result.content);
        assert!(memory_dir.join("context.md").exists());
    }

    #[tokio::test]
    async fn write_error_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "write", "content": "data" }),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("file"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn write_error_when_content_missing() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "write", "file": "notes" }),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("content"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn write_rejects_invalid_file_name() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "write", "file": "../evil", "content": "bad" }),
        )
        .await;
        assert!(result.is_error);
    }

    // -----------------------------------------------------------------------
    // read — specific file
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_specific_file_returns_content() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("facts.md"), b"Important fact.").unwrap();

        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "read", "file": "facts" }),
        )
        .await;

        assert!(!result.is_error, "got: {}", result.content);
        assert_eq!(result.content, "Important fact.");
    }

    #[tokio::test]
    async fn read_specific_file_not_found_returns_error() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "read", "file": "nonexistent" }),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("nonexistent.md"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn read_rejects_invalid_file_name() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "read", "file": "foo/bar" }),
        )
        .await;
        assert!(result.is_error);
    }

    // -----------------------------------------------------------------------
    // read — all files
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_all_returns_all_md_files_with_headers() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("alpha.md"), b"A content").unwrap();
        std::fs::write(dir.path().join("beta.md"), b"B content").unwrap();
        // Non-.md file — should be ignored.
        std::fs::write(dir.path().join("ignored.txt"), b"ignored").unwrap();

        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "read" }),
        )
        .await;

        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("alpha.md"), "should include alpha filename");
        assert!(result.content.contains("A content"), "should include alpha content");
        assert!(result.content.contains("beta.md"), "should include beta filename");
        assert!(result.content.contains("B content"), "should include beta content");
        assert!(!result.content.contains("ignored"), "txt file should be skipped");
    }

    #[tokio::test]
    async fn read_all_empty_directory_returns_no_files_message() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "read" }),
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("No memory files"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn read_all_missing_directory_returns_friendly_message() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("nonexistent");
        let result = run_with_context(
            missing,
            json!({ "operation": "read" }),
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("does not exist"), "got: {}", result.content);
    }

    // -----------------------------------------------------------------------
    // list
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_returns_sorted_md_filenames() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("zebra.md"), b"z").unwrap();
        std::fs::write(dir.path().join("alpha.md"), b"a").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), b"x").unwrap();

        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "list" }),
        )
        .await;

        assert!(!result.is_error, "got: {}", result.content);
        let lines: Vec<&str> = result.content.lines().collect();
        assert_eq!(lines, ["alpha.md", "zebra.md"], "should be sorted, no txt");
    }

    #[tokio::test]
    async fn list_empty_directory_returns_no_files_message() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "list" }),
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("No memory files"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn list_missing_directory_returns_friendly_message() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("nonexistent");
        let result = run_with_context(
            missing,
            json!({ "operation": "list" }),
        )
        .await;
        assert!(!result.is_error);
        assert!(result.content.contains("does not exist"), "got: {}", result.content);
    }

    // -----------------------------------------------------------------------
    // no project context
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_without_project_context_returns_error() {
        // Call execute directly — no PROJECT_CONTEXT.scope(), so try_with fails.
        let result = MemoryTool.execute(json!({ "operation": "list" })).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("project context"),
            "error should explain the requirement: {}",
            result.content
        );
    }

    // -----------------------------------------------------------------------
    // unknown operation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn unknown_operation_returns_error() {
        let dir = TempDir::new().unwrap();
        let result = run_with_context(
            dir.path().to_path_buf(),
            json!({ "operation": "delete" }),
        )
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("delete"), "got: {}", result.content);
    }
}

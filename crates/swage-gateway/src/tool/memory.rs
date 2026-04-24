//! MemoryTool — read and write persistent per-bench memory files.
//!
//! The memory directory is derived from the tool context's working directory:
//! `{working_directory}/.swage/memory`. This keeps the tool stateless and
//! thread-safe without requiring any constructor arguments or task-locals.
//!
//! Operations:
//!   - **read**: read a specific file (with "file" param) or all files (no param)
//!   - **write**: create or overwrite a named file
//!   - **list**: return names of all memory files in sorted order

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

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
        "Read and write persistent per-bench memory files."
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

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Derive the memory directory from the context's working directory.
        // Convention: {working_directory}/.swage/memory
        let memory_dir: PathBuf = ctx.working_directory().join(".swage").join("memory");

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

    use tempfile::TempDir;

    use swage_tool::context::MockToolContext;

    use super::*;

    /// Build a MockToolContext with the given directory as working_directory.
    ///
    /// The memory dir will be {dir}/.swage/memory — derived inside execute().
    fn ctx_with_root(dir: &TempDir) -> MockToolContext {
        MockToolContext {
            agent_id: "test-agent".to_string(),
            session_id: "test-session".to_string(),
            working_directory: dir.path().to_path_buf(),
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        }
    }

    /// Path where MemoryTool will look for files given ctx_with_root.
    fn memory_dir(dir: &TempDir) -> PathBuf {
        dir.path().join(".swage").join("memory")
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
            "space should be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // write
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn write_creates_file_with_md_extension() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn write_overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "notes.md", b"old content");

        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn write_creates_directory_if_missing() {
        let dir = TempDir::new().unwrap();
        // The .swage/memory directory must not exist yet.
        assert!(!memory_dir(&dir).exists());

        let ctx = ctx_with_root(&dir);
        MemoryTool
            .execute(
                json!({ "operation": "write", "file": "context", "content": "data" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(memory_dir(&dir).join("context.md").exists());
    }

    #[tokio::test]
    async fn write_error_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
        let err = MemoryTool
            .execute(json!({ "operation": "write", "content": "data" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("file"), "got: {err}");
    }

    #[tokio::test]
    async fn write_error_when_content_missing() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
        let err = MemoryTool
            .execute(json!({ "operation": "write", "file": "notes" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("content"), "got: {err}");
    }

    #[tokio::test]
    async fn write_rejects_invalid_file_name() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn read_specific_file_returns_content() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "facts.md", b"Important fact.");

        let ctx = ctx_with_root(&dir);
        let result = MemoryTool
            .execute(json!({ "operation": "read", "file": "facts" }), &ctx)
            .await
            .unwrap();

        assert_eq!(result["output"].as_str().unwrap(), "Important fact.");
    }

    #[tokio::test]
    async fn read_specific_file_not_found_returns_error() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
        let err = MemoryTool
            .execute(json!({ "operation": "read", "file": "nonexistent" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("nonexistent.md"), "got: {err}");
    }

    #[tokio::test]
    async fn read_rejects_invalid_file_name() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
        let err = MemoryTool
            .execute(json!({ "operation": "read", "file": "foo/bar" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }

    // -----------------------------------------------------------------------
    // read — all files
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_all_returns_all_md_files_with_headers() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "alpha.md", b"A content");
        seed_file(&dir, "beta.md", b"B content");
        // Non-.md file — should be ignored.
        std::fs::write(memory_dir(&dir).join("ignored.txt"), b"ignored").unwrap();

        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn read_all_empty_directory_returns_no_files_message() {
        let dir = TempDir::new().unwrap();
        // Create the memory dir but leave it empty.
        std::fs::create_dir_all(memory_dir(&dir)).unwrap();

        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn read_all_missing_directory_returns_friendly_message() {
        let dir = TempDir::new().unwrap();
        // Don't create the memory dir — it shouldn't exist.
        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn list_returns_sorted_md_filenames() {
        let dir = TempDir::new().unwrap();
        seed_file(&dir, "zebra.md", b"z");
        seed_file(&dir, "alpha.md", b"a");
        std::fs::write(memory_dir(&dir).join("ignored.txt"), b"x").unwrap();

        let ctx = ctx_with_root(&dir);
        let result = MemoryTool
            .execute(json!({ "operation": "list" }), &ctx)
            .await
            .unwrap();

        let output = result["output"].as_str().unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines, ["alpha.md", "zebra.md"], "should be sorted, no txt");
    }

    #[tokio::test]
    async fn list_empty_directory_returns_no_files_message() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(memory_dir(&dir)).unwrap();

        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn list_missing_directory_returns_friendly_message() {
        let dir = TempDir::new().unwrap();
        // Don't create the memory dir.
        let ctx = ctx_with_root(&dir);
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

    #[tokio::test]
    async fn missing_operation_returns_error() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
        let err = MemoryTool.execute(json!({}), &ctx).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }

    // -----------------------------------------------------------------------
    // unknown operation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn unknown_operation_returns_error() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx_with_root(&dir);
        let err = MemoryTool
            .execute(json!({ "operation": "delete" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("delete"), "got: {err}");
    }
}

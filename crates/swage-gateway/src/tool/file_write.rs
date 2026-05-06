//! File write tool — creates or overwrites files within the workspace.
//!
//! Creates parent directories as needed. Validates the target path stays
//! within the workspace root before writing.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::resolve_new_context_path;

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file in the workspace. Parent directories are created if needed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workspace root."
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let path_str = params["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: path".to_owned(),
            })?
            .to_string();

        let content = params["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: content".to_owned(),
            })?
            .to_string();

        let resolved =
            resolve_new_context_path(ctx, &path_str).map_err(|msg| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: msg,
            })?;

        // Create parent directories so the write doesn't fail on a missing tree.
        if let Some(parent) = resolved.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Permission denied: {path_str}: {e}"),
                })?;
        }

        let byte_count = content.len();

        tokio::fs::write(&resolved.path, content.as_bytes())
            .await
            .map_err(|e| {
                let message = match e.kind() {
                    std::io::ErrorKind::PermissionDenied => {
                        format!("Permission denied: {path_str}")
                    }
                    _ => format!("Failed to write {path_str}: {e}"),
                };
                ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message,
                }
            })?;

        Ok(json!({"output": format!("Wrote {byte_count} bytes to {path_str}")}))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use swage_tool::context::MockToolContext;
    use tempfile::TempDir;

    fn tool(dir: &TempDir) -> FileWriteTool {
        let _ = dir;
        FileWriteTool
    }

    fn ctx(dir: &TempDir) -> MockToolContext {
        let mut ctx = MockToolContext::new("agent-test", "sess-test");
        ctx.working_directory = dir.path().to_path_buf();
        ctx
    }

    #[tokio::test]
    async fn creates_new_file() {
        let dir = TempDir::new().unwrap();
        let val = tool(&dir)
            .execute(json!({ "path": "new.txt", "content": "hello" }), &ctx(&dir))
            .await
            .unwrap();
        assert!(val["output"].as_str().unwrap().contains("5 bytes"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("existing.txt"), b"old content").unwrap();
        tool(&dir)
            .execute(
                json!({ "path": "existing.txt", "content": "new content" }),
                &ctx(&dir),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("existing.txt")).unwrap(),
            "new content"
        );
    }

    #[tokio::test]
    async fn creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        tool(&dir)
            .execute(
                json!({ "path": "a/b/c/file.txt", "content": "deep" }),
                &ctx(&dir),
            )
            .await
            .unwrap();
        assert!(dir.path().join("a/b/c/file.txt").exists());
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(
                json!({ "path": "../../etc/passwd", "content": "bad" }),
                &ctx(&dir),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outside"), "got: {err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_new_file_beneath_symlink_parent_escape() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();

        let err = tool(&dir)
            .execute(
                json!({ "path": "link/escape.txt", "content": "bad" }),
                &ctx(&dir),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("outside"), "got: {err}");
        assert!(!outside.path().join("escape.txt").exists());
    }

    #[tokio::test]
    async fn returns_error_for_missing_path_argument() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "content": "hello" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("path"), "got: {err}");
    }

    #[tokio::test]
    async fn returns_error_for_missing_content_argument() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "file.txt" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("content"), "got: {err}");
    }
}

//! File write tool — creates or overwrites files within the workspace.
//!
//! Creates parent directories as needed. Validates the target path stays
//! within the workspace root before writing.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::validate_path;

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileWriteTool {
    pub workspace_root: PathBuf,
}

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

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
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

        // Validate the target path. The file may not exist yet.
        let resolved =
            validate_path(&self.workspace_root, &path_str).map_err(|msg| {
                ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: msg,
                }
            })?;

        // Create parent directories so the write doesn't fail on a missing tree.
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Permission denied: {path_str}: {e}"),
                }
            })?;
        }

        let byte_count = content.len();

        tokio::fs::write(&resolved, content.as_bytes())
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
        FileWriteTool {
            workspace_root: dir.path().to_path_buf(),
        }
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
    }

    #[tokio::test]
    async fn creates_new_file() {
        let dir = TempDir::new().unwrap();
        let val = tool(&dir)
            .execute(json!({ "path": "new.txt", "content": "hello" }), &ctx())
            .await
            .unwrap();
        assert!(val["output"].as_str().unwrap().contains("5 bytes"));
        assert_eq!(std::fs::read_to_string(dir.path().join("new.txt")).unwrap(), "hello");
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("existing.txt"), b"old content").unwrap();
        tool(&dir)
            .execute(json!({ "path": "existing.txt", "content": "new content" }), &ctx())
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
            .execute(json!({ "path": "a/b/c/file.txt", "content": "deep" }), &ctx())
            .await
            .unwrap();
        assert!(dir.path().join("a/b/c/file.txt").exists());
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "../../etc/passwd", "content": "bad" }), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outside"), "got: {}", err);
    }

    #[tokio::test]
    async fn returns_error_for_missing_path_argument() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "content": "hello" }), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("path"), "got: {}", err);
    }

    #[tokio::test]
    async fn returns_error_for_missing_content_argument() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "file.txt" }), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("content"), "got: {}", err);
    }
}

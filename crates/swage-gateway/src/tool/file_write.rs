//! File write tool — creates or overwrites files within the workspace.
//!
//! Creates parent directories as needed. Validates the target path stays
//! within the workspace root before writing.

use std::path::PathBuf;
use async_trait::async_trait;
use serde_json::json;
use crate::tool::{Tool, ToolExecutionResult};
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

    fn parameters(&self) -> serde_json::Value {
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

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        let path_str = match arguments["path"].as_str() {
            Some(p) => p.to_string(),
            None => {
                return ToolExecutionResult {
                    content: "Missing required argument: path".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        let content = match arguments["content"].as_str() {
            Some(c) => c.to_string(),
            None => {
                return ToolExecutionResult {
                    content: "Missing required argument: content".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        // Validate the target path. The file may not exist yet, so we validate
        // against the normalised path — the workspace check still applies.
        let resolved = match validate_path(&self.workspace_root, &path_str) {
            Ok(p) => p,
            Err(msg) => {
                return ToolExecutionResult {
                    content: msg,
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        // Create parent directories so the write doesn't fail on a missing tree.
        if let Some(parent) = resolved.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolExecutionResult {
                    content: format!("Permission denied: {path_str}: {e}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        }

        let byte_count = content.len();

        if let Err(e) = tokio::fs::write(&resolved, content.as_bytes()).await {
            let msg = match e.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    format!("Permission denied: {path_str}")
                }
                _ => format!("Failed to write {path_str}: {e}"),
            };
            return ToolExecutionResult {
                content: msg,
                is_error: true,
                truncated: false,
                total_bytes: None,
            };
        }

        ToolExecutionResult {
            content: format!("Wrote {byte_count} bytes to {path_str}"),
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
    use super::*;
    use tempfile::TempDir;

    fn tool(dir: &TempDir) -> FileWriteTool {
        FileWriteTool {
            workspace_root: dir.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn creates_new_file() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "new.txt", "content": "hello" }))
            .await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("5 bytes"));
        assert_eq!(std::fs::read_to_string(dir.path().join("new.txt")).unwrap(), "hello");
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("existing.txt"), b"old content").unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "existing.txt", "content": "new content" }))
            .await;
        assert!(!result.is_error, "got: {}", result.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("existing.txt")).unwrap(),
            "new content"
        );
    }

    #[tokio::test]
    async fn creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "a/b/c/file.txt", "content": "deep" }))
            .await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(dir.path().join("a/b/c/file.txt").exists());
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "../../etc/passwd", "content": "bad" }))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("outside"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn returns_error_for_missing_path_argument() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir).execute(json!({ "content": "hello" })).await;
        assert!(result.is_error);
        assert!(result.content.contains("path"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn returns_error_for_missing_content_argument() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir).execute(json!({ "path": "file.txt" })).await;
        assert!(result.is_error);
        assert!(result.content.contains("content"), "got: {}", result.content);
    }
}

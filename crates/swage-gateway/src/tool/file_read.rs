//! File read tool — reads file contents within the workspace.
//!
//! Validates the requested path against the workspace root before reading,
//! rejects binary files, and truncates output that exceeds the configured limit.

use std::path::PathBuf;
use async_trait::async_trait;
use serde_json::json;
use crate::tool::{Tool, ToolExecutionResult};
use crate::tool::workspace::{validate_path, is_binary_content, truncate_output};

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileReadTool {
    pub workspace_root: PathBuf,
    pub max_output_bytes: usize,
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file in the workspace."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workspace root."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        // Extract the requested path from arguments.
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

        // Validate and resolve path against workspace root.
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

        // Read raw bytes from disk.
        let bytes = match tokio::fs::read(&resolved).await {
            Ok(b) => b,
            Err(e) => {
                let msg = match e.kind() {
                    std::io::ErrorKind::NotFound => {
                        format!("File not found: {path_str}")
                    }
                    std::io::ErrorKind::PermissionDenied => {
                        format!("Permission denied: {path_str}")
                    }
                    _ => format!("Failed to read {path_str}: {e}"),
                };
                return ToolExecutionResult {
                    content: msg,
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        // Reject binary files — they cannot be meaningfully displayed as text.
        if is_binary_content(&bytes) {
            return ToolExecutionResult {
                content: format!(
                    "File appears to be binary ({} bytes). Cannot display binary content.",
                    bytes.len()
                ),
                is_error: true,
                truncated: false,
                total_bytes: Some(bytes.len()),
            };
        }

        // Convert to UTF-8, replacing any invalid sequences rather than failing.
        let text = String::from_utf8_lossy(&bytes).into_owned();

        let (content, truncated, total_bytes) =
            truncate_output(&text, self.max_output_bytes);

        ToolExecutionResult {
            content,
            is_error: false,
            truncated,
            total_bytes: if truncated { Some(total_bytes) } else { None },
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

    fn tool(dir: &TempDir) -> FileReadTool {
        FileReadTool {
            workspace_root: dir.path().to_path_buf(),
            max_output_bytes: 1_000_000,
        }
    }

    fn tool_with_limit(dir: &TempDir, limit: usize) -> FileReadTool {
        FileReadTool {
            workspace_root: dir.path().to_path_buf(),
            max_output_bytes: limit,
        }
    }

    #[tokio::test]
    async fn reads_text_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), b"Hello, world!").unwrap();
        let result = tool(&dir).execute(json!({ "path": "hello.txt" })).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "Hello, world!");
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "../../../etc/passwd" }))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("outside"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn returns_error_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "nonexistent.txt" }))
            .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("not found") || result.content.contains("No such"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn detects_binary_file() {
        let dir = TempDir::new().unwrap();
        // Write a file with null bytes — reliably detected as binary.
        std::fs::write(dir.path().join("data.bin"), b"\x00\x01\x02\x03").unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "data.bin" }))
            .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("binary"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn truncates_large_file() {
        let dir = TempDir::new().unwrap();
        let content = "abcdefghij".repeat(10); // 100 bytes
        std::fs::write(dir.path().join("big.txt"), content.as_bytes()).unwrap();
        let result = tool_with_limit(&dir, 20)
            .execute(json!({ "path": "big.txt" }))
            .await;
        assert!(!result.is_error);
        assert!(result.truncated);
        assert_eq!(result.content.len(), 20);
    }

    #[tokio::test]
    async fn returns_error_for_missing_path_argument() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir).execute(json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("Missing"), "got: {}", result.content);
    }
}

//! File read tool — reads file contents within the workspace.
//!
//! Validates the requested path against the workspace root before reading,
//! rejects binary files, and truncates output that exceeds the configured limit.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::{is_binary_content, resolve_context_path, truncate_output};

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileReadTool {
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

    fn parameters(&self) -> Value {
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

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let path_str = params["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: path".to_owned(),
            })?
            .to_string();

        let resolved =
            resolve_context_path(ctx, &path_str).map_err(|msg| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: msg,
            })?;

        // Read raw bytes from disk.
        let bytes = tokio::fs::read(&resolved.path).await.map_err(|e| {
            let message = match e.kind() {
                std::io::ErrorKind::NotFound => format!("File not found: {path_str}"),
                std::io::ErrorKind::PermissionDenied => {
                    format!("Permission denied: {path_str}")
                }
                _ => format!("Failed to read {path_str}: {e}"),
            };
            ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message,
            }
        })?;

        // Reject binary files — they cannot be meaningfully displayed as text.
        if is_binary_content(&bytes) {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!(
                    "File appears to be binary ({} bytes). Cannot display binary content.",
                    bytes.len()
                ),
            });
        }

        // Convert to UTF-8, replacing any invalid sequences rather than failing.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let (content, _truncated, _total_bytes) = truncate_output(&text, self.max_output_bytes);

        Ok(json!({"output": content}))
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

    fn tool(dir: &TempDir) -> FileReadTool {
        let _ = dir;
        FileReadTool {
            max_output_bytes: 1_000_000,
        }
    }

    fn tool_with_limit(dir: &TempDir, limit: usize) -> FileReadTool {
        let _ = dir;
        FileReadTool {
            max_output_bytes: limit,
        }
    }

    fn ctx(dir: &TempDir) -> MockToolContext {
        let mut ctx = MockToolContext::new("agent-test", "sess-test");
        ctx.working_directory = dir.path().to_path_buf();
        ctx
    }

    #[tokio::test]
    async fn reads_text_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), b"Hello, world!").unwrap();
        let val = tool(&dir)
            .execute(json!({ "path": "hello.txt" }), &ctx(&dir))
            .await
            .unwrap();
        assert_eq!(val["output"], "Hello, world!");
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "../../../etc/passwd" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outside"), "got: {err}");
    }

    #[tokio::test]
    async fn returns_error_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "nonexistent.txt" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("No such"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn detects_binary_file() {
        let dir = TempDir::new().unwrap();
        // Write a file with null bytes — reliably detected as binary.
        std::fs::write(dir.path().join("data.bin"), b"\x00\x01\x02\x03").unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "data.bin" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"), "got: {err}");
    }

    #[tokio::test]
    async fn truncates_large_file() {
        let dir = TempDir::new().unwrap();
        let content = "abcdefghij".repeat(10); // 100 bytes
        std::fs::write(dir.path().join("big.txt"), content.as_bytes()).unwrap();
        let val = tool_with_limit(&dir, 20)
            .execute(json!({ "path": "big.txt" }), &ctx(&dir))
            .await
            .unwrap();
        assert!(val["output"].as_str().unwrap().len() <= 20);
    }

    #[tokio::test]
    async fn returns_error_for_missing_path_argument() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir).execute(json!({}), &ctx(&dir)).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }
}

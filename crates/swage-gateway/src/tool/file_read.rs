//! File read tool — reads file contents within the workspace.
//!
//! Validates the requested path against the workspace root before reading,
//! rejects binary files, and truncates output that exceeds the configured limit.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::{
    FileContent, continuable_output_value, location_approval_error, read_file_content,
    resolve_context_path,
};

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

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("path"),
        }
    }

    fn description(&self) -> &str {
        "Read the contents of a file in the workspace, optionally from a byte offset."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workspace root."
                },
                "offset_bytes": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Byte offset to start reading from. Use next_offset_bytes from a truncated result to continue."
                }
            },
            "required": ["path"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let path_str = params["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: path".to_owned(),
            })?
            .to_string();
        let offset_bytes = optional_offset_bytes(&params, self.name())?;

        let resolved = resolve_context_path(ctx, &path_str)
            .map_err(|_| location_approval_error(self.name(), ctx, &path_str, "read", false))?;

        let content = read_file_content(&resolved.path).await.map_err(|e| {
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

        let bytes = match content {
            FileContent::Text(bytes) => bytes,
            // Reject binary files — they cannot be meaningfully displayed as text.
            FileContent::Binary { len } => {
                return Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!(
                        "File appears to be binary ({len} bytes). Cannot display binary content."
                    ),
                });
            }
        };

        // Convert to UTF-8, replacing any invalid sequences rather than failing.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        read_text_from_offset(
            &text,
            &path_str,
            offset_bytes,
            self.max_output_bytes,
            self.name(),
        )
    }
}

fn optional_offset_bytes(params: &Value, tool_name: &str) -> Result<usize, ToolError> {
    match params.get("offset_bytes") {
        Some(Value::Number(number)) => {
            let offset = number
                .as_u64()
                .ok_or_else(|| ToolError::InvalidParameters {
                    name: tool_name.to_owned(),
                    message: "offset_bytes must be a non-negative integer".to_owned(),
                })?;
            usize::try_from(offset).map_err(|_| ToolError::InvalidParameters {
                name: tool_name.to_owned(),
                message: "offset_bytes is too large for this platform".to_owned(),
            })
        }
        Some(Value::Null) | None => Ok(0),
        Some(_) => Err(ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message: "offset_bytes must be a non-negative integer".to_owned(),
        }),
    }
}

fn read_text_from_offset(
    text: &str,
    path: &str,
    offset_bytes: usize,
    max_output_bytes: usize,
    tool_name: &str,
) -> Result<Value, ToolError> {
    if offset_bytes > text.len() || !text.is_char_boundary(offset_bytes) {
        return Err(ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message: "offset_bytes must be a valid UTF-8 byte boundary within the file".to_owned(),
        });
    }

    let mut arguments = serde_json::Map::new();
    arguments.insert("path".to_string(), json!(path));
    continuable_output_value(text, max_output_bytes, offset_bytes, "file_read", arguments).map_err(
        |message| ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message,
        },
    )
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
        assert!(
            val.get("total_bytes").is_none(),
            "small first-surface reads should keep their old shape: {val:?}"
        );
        assert!(
            val.get("follow_up").is_none(),
            "small first-surface reads should not invent follow-up metadata: {val:?}"
        );
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "../../../etc/passwd" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ToolError::RequiresLocationApproval { ref access, .. } if access == "read"
            ),
            "got: {err}"
        );
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
        assert_eq!(val["truncated"], json!(true));
        assert_eq!(val["total_bytes"], json!(100));
    }

    #[tokio::test]
    async fn truncated_file_read_returns_follow_up_offset() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("big.txt"), b"abcdefghij").unwrap();

        let val = tool_with_limit(&dir, 4)
            .execute(json!({ "path": "big.txt" }), &ctx(&dir))
            .await
            .unwrap();

        assert_eq!(val["output"], json!("abcd"));
        assert_eq!(val["truncated"], json!(true));
        assert_eq!(val["total_bytes"], json!(10));
        assert_eq!(val["offset_bytes"], json!(0));
        assert_eq!(val["next_offset_bytes"], json!(4));
        assert_eq!(val["follow_up"]["tool"], json!("file_read"));
        assert_eq!(val["follow_up"]["arguments"]["path"], json!("big.txt"));
        assert_eq!(val["follow_up"]["arguments"]["offset_bytes"], json!(4));
    }

    #[tokio::test]
    async fn offset_reads_continue_from_prior_truncation() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("big.txt"), b"abcdefghij").unwrap();

        let val = tool_with_limit(&dir, 4)
            .execute(json!({ "path": "big.txt", "offset_bytes": 4 }), &ctx(&dir))
            .await
            .unwrap();

        assert_eq!(val["output"], json!("efgh"));
        assert_eq!(val["truncated"], json!(true));
        assert_eq!(val["total_bytes"], json!(10));
        assert_eq!(val["offset_bytes"], json!(4));
        assert_eq!(val["next_offset_bytes"], json!(8));
    }

    #[tokio::test]
    async fn returns_error_for_missing_path_argument() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir).execute(json!({}), &ctx(&dir)).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
    }
}

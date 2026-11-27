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
    binary_size_description, location_approval_error, read_text_sample, resolve_context_path,
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
        "Read a text file and return its contents as `output`. A file longer than the \
         installation's output limit comes back cut at that limit and marked `truncated`, \
         with the file's size in `total_bytes` and the place to carry on from in \
         `next_offset_bytes`; pass that as `offset_bytes` to read the next part. A file that \
         does not exist fails saying so, and a binary file is refused, not shown. Reading \
         changes nothing."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file to read: relative to the session's working directory, or absolute for one elsewhere the session has been allowed to reach."
                },
                "offset_bytes": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Where in the file to start, counted in bytes: the `next_offset_bytes` of the result being continued. Omit to read from the beginning."
                },
                "justification": {
                    "type": "string",
                    "description": swage_agent::JUSTIFICATION_DESCRIPTION
                }
            },
            "required": ["path"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    fn validate_call(&self, params: &Value) -> Result<(), ToolError> {
        optional_offset_bytes(params, self.name())?;
        Ok(())
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

        let max_bytes = self.max_output_bytes;
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&resolved.path).map_err(|error| {
                ToolError::ExecutionFailed {
                    name: "file_read".into(),
                    message: match error.kind() {
                        std::io::ErrorKind::NotFound => format!("File not found: {path_str}"),
                        std::io::ErrorKind::PermissionDenied => {
                            format!("Permission denied: {path_str}")
                        }
                        _ => format!("Failed to read {path_str}: {error}"),
                    },
                }
            })?;
            let len = file
                .metadata()
                .map_err(|error| ToolError::ExecutionFailed {
                    name: "file_read".into(),
                    message: format!("Failed to inspect {path_str}: {error}"),
                })?
                .len();
            // Window validation follows file inspection; I/O failures retain
            // unknown completion rather than claiming the read finished.
            read_window(file, len, &path_str, offset_bytes, max_bytes).map_err(
                |error| match error {
                    ToolError::InvalidParameters { .. } => error.after_execution(),
                    error => error,
                },
            )
        })
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            name: self.name().into(),
            message: error.to_string(),
        })?
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

fn read_window(
    mut source: impl std::io::Read + std::io::Seek,
    total_bytes: u64,
    path: &str,
    offset: usize,
    max_bytes: usize,
) -> Result<Value, ToolError> {
    use std::io::{Read, SeekFrom};
    let failure = |message| ToolError::ExecutionFailed {
        name: "file_read".into(),
        message,
    };
    let invalid = || ToolError::InvalidParameters {
        name: "file_read".into(),
        message: "offset_bytes must be within the file".into(),
    };
    let sample = match read_text_sample(&mut source).map_err(|e| failure(e.to_string()))? {
        Some(sample) => sample,
        None => {
            return Err(failure(format!(
                "File appears to be binary ({}). Cannot display binary content",
                binary_size_description((total_bytes > 0).then_some(total_bytes))
            )));
        }
    };
    let known_total = if total_bytes == 0 && !sample.is_empty() {
        None
    } else {
        Some(total_bytes)
    };
    if known_total.is_some_and(|total| offset as u64 > total) {
        return Err(invalid());
    }
    source
        .seek(SeekFrom::Start(offset as u64))
        .map_err(|e| failure(e.to_string()))?;
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    // Read through the last character that can start inside the window.
    let window_bytes = max_bytes.max(1).saturating_add(3);
    source
        .take(window_bytes as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| failure(e.to_string()))?;

    // Keep page offsets in source bytes even when decoding malformed text.
    let mut output = String::new();
    let mut consumed = 0;
    'window: for chunk in bytes.utf8_chunks() {
        let invalid = (!chunk.invalid().is_empty()).then_some(('\u{fffd}', chunk.invalid().len()));
        for (ch, source_width) in chunk
            .valid()
            .chars()
            .map(|ch| (ch, ch.len_utf8()))
            .chain(invalid)
        {
            // A non-empty window always advances by at least one character.
            if !output.is_empty() && output.len() + ch.len_utf8() > max_bytes {
                break 'window;
            }
            output.push(ch);
            consumed += source_width;
        }
    }
    let next = offset + consumed;
    let has_more = known_total.map_or(bytes.len() > consumed, |total| (next as u64) < total);
    let mut result = json!({"output": output});
    if offset > 0 || has_more {
        result["offset_bytes"] = json!(offset);
        result["total_bytes"] = json!(known_total);
    }
    if has_more {
        result["truncated"] = json!(true);
        result["next_offset_bytes"] = json!(next);
        result["follow_up"] =
            json!({"tool": "file_read", "arguments": {"path": path, "offset_bytes": next}});
    }
    Ok(result)
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
    async fn reads_mostly_ascii_text_with_legacy_encoding() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("note.txt"), b"Meet at the caf\xe9.\n").unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "note.txt" }), &ctx(&dir))
            .await
            .unwrap();
        assert_eq!(result["output"], "Meet at the caf\u{fffd}.\n");
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
        let rendered = swage_agent::agent::format_tool_error_for_model(&err);
        assert!(
            rendered.contains("Cannot display binary content.")
                && !rendered.contains("Cannot display binary content.."),
            "got: {rendered}"
        );
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
    #[tokio::test]
    async fn dispatch_records_rejection_after_reading_the_file() {
        use swage_protocol::client::conversation::{
            ToolCompletion, ToolExecutionStage, ToolOutcome,
        };
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("text.txt"), "settlement text").unwrap();
        let dispatcher = crate::tool::ToolDispatcher::allow_all(vec![
            std::sync::Arc::new(tool(&dir)) as std::sync::Arc<dyn Tool>,
        ]);
        for approved in [false, true] {
            let params = json!({"path":"text.txt", "offset_bytes":100});
            let error = if approved {
                dispatcher
                    .dispatch_approved("file_read", params, &ctx(&dir), "agent")
                    .await
            } else {
                dispatcher
                    .dispatch("file_read", params, &ctx(&dir), "agent")
                    .await
            }
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("offset_bytes must be within the file"),
                "expected a post-read offset rejection: {error}"
            );
            let execution = error.execution();
            assert_eq!(execution.outcome, ToolOutcome::Rejected);
            assert_eq!(execution.stage, ToolExecutionStage::Finished);
            assert_eq!(execution.completion, ToolCompletion::Confirmed);
            let rendered = swage_agent::agent::format_tool_error_for_model(&error);
            assert!(rendered.contains("offset_bytes must be within the file"));
            assert!(rendered.contains("Correct the arguments and retry"));
            assert!(
                rendered.contains("operation ran before its result was rejected"),
                "post-read rejection must disclose execution: {rendered}"
            );
        }
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    struct Guarded {
        inner: std::io::Cursor<Vec<u8>>,
        consumed: usize,
        allowance: usize,
    }
    impl Read for Guarded {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            assert!(
                self.consumed < self.allowance,
                "read exceeded bounded sample and window"
            );
            let n = self.inner.read(buf)?;
            self.consumed += n;
            Ok(n)
        }
    }
    impl Seek for Guarded {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }
    #[test]
    fn regression_read_retains_only_requested_window() {
        let source = Guarded {
            inner: std::io::Cursor::new(vec![b'x'; 1_000_000]),
            consumed: 0,
            allowance: 8192 + 20,
        };
        let result = read_window(source, 1_000_000, "notes", 900_000, 16).unwrap();
        assert_eq!(result["output"], "x".repeat(16));
        assert_eq!(result["next_offset_bytes"], 900_016);
        assert_eq!(result["total_bytes"], 1_000_000);
    }
    #[test]
    fn regression_binary_never_reads_past_sample() {
        let source = Guarded {
            inner: std::io::Cursor::new(vec![0; 20_000]),
            consumed: 0,
            allowance: 8192,
        };
        let error = read_window(source, 20_000, "binary", 0, 16).unwrap_err();
        assert!(error.to_string().contains("20000 bytes"), "{error}");
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_existing_binary_lengths_reach_both_tools() {
        use crate::tool::{file_edit::FileEditTool, workspace::writable_test_context};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("binary"), vec![0; 20_000]).unwrap();
        let ctx = writable_test_context(dir.path(), vec![]);
        let read = FileReadTool {
            max_output_bytes: 16,
        }
        .execute(json!({"path":"binary"}), &ctx)
        .await
        .unwrap_err();
        let edit = FileEditTool.execute(json!({"changes":[{"path":"binary","edits":[{"type":"replace","old_text":"x","new_text":"y"}]}]}), &ctx).await.unwrap_err();
        assert!(read.to_string().contains("20000 bytes"), "{read}");
        assert!(edit.to_string().contains("20000 bytes"), "{edit}");
    }
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_virtual_binary_reports_unknown_length() {
        let ctx = crate::tool::workspace::writable_test_context(
            std::path::Path::new("/proc/self"),
            vec![],
        );
        let read = FileReadTool {
            max_output_bytes: 16,
        }
        .execute(json!({"path":"auxv"}), &ctx)
        .await
        .unwrap_err();
        let edit = crate::tool::file_edit::FileEditTool.execute(json!({"changes":[{"path":"auxv","edits":[{"type":"replace","old_text":"x","new_text":"y"}]}]}), &ctx).await.unwrap_err();
        assert!(read.to_string().contains("unknown byte length"), "{read}");
        assert!(edit.to_string().contains("unknown byte length"), "{edit}");
    }
    #[tokio::test]
    async fn regression_read_utf8_offsets_and_window_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes"), "aébcdef").unwrap();
        let mut ctx = swage_tool::context::MockToolContext::new("agent", "session");
        ctx.working_directory = dir.path().into();
        let tool = FileReadTool {
            max_output_bytes: 2,
        };
        let result = tool.execute(json!({"path":"notes"}), &ctx).await.unwrap();
        assert_eq!(result["output"], "a");
        assert_eq!(result["next_offset_bytes"], 1);
        let result = tool
            .execute(json!({"path":"notes", "offset_bytes":1}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["output"], "é");
        let result = tool
            .execute(json!({"path":"notes", "offset_bytes":2}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["output"], "�");
        assert_eq!(result["next_offset_bytes"], 3);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod virtual_text_regression {
    use super::*;
    #[tokio::test]
    async fn regression_virtual_text_read_keeps_its_continuation() {
        let root = std::path::Path::new("/proc/sys/kernel");
        let expected = std::fs::read_to_string(root.join("ostype")).unwrap();
        let mut ctx = swage_tool::context::MockToolContext::new("agent", "session");
        ctx.working_directory = root.into();
        let tool = FileReadTool {
            max_output_bytes: 2,
        };
        let first = tool.execute(json!({"path":"ostype"}), &ctx).await.unwrap();
        assert_eq!(
            first["truncated"], true,
            "zero metadata length hid the continuation: {first}"
        );
        let mut text = first["output"].as_str().unwrap().to_string();
        let mut result = first;
        while let Some(arguments) = result
            .get("follow_up")
            .map(|value| value["arguments"].clone())
        {
            result = tool.execute(arguments, &ctx).await.unwrap();
            assert!(
                !result["output"].as_str().unwrap().is_empty(),
                "continuation did not advance: {result}"
            );
            text.push_str(result["output"].as_str().unwrap());
        }
        assert_eq!(text, expected);
    }
}

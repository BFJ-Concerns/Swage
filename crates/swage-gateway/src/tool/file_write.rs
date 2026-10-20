//! File write tool — creates, appends to, or replaces files in the workspace.
//!
//! Creates parent directories as needed. Validates the target path stays
//! within the workspace root before writing.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;
use tokio::io::AsyncWriteExt;

use crate::tool::Tool;
use crate::tool::workspace::{
    location_approval_error, require_workspace_write_access, resolve_new_context_path,
};

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("path"),
        }
    }

    fn description(&self) -> &str {
        "Create or overwrite a file in the workspace. Parent directories are created if needed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "justification": {
                    "type": "string",
                    "description": "Why this file is needed when it lies outside the session's granted locations, in one or two sentences. Read by the judge that answers for the request, and by the person when one is watching and the judge is not sure; omitted, the request is weighed without a reason."
                },
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workspace root."
                },
                "append": {
                    "type": "boolean",
                    "description": "Append content to the file instead of replacing it. Defaults to false."
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file."
                }
            },
            "required": ["path", "content"]
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

        let content = params["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: content".to_owned(),
            })?
            .to_string();

        let append = match params.get("append") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(ToolError::InvalidParameters {
                    name: self.name().into(),
                    message: "append must be a boolean".into(),
                });
            }
        };
        let resolved = resolve_new_context_path(ctx, &path_str)
            .map_err(|_| location_approval_error(self.name(), ctx, &path_str, "write", false))?;

        require_workspace_write_access(ctx, &resolved.path, self.name()).await?;

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

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(append)
            .truncate(false)
            .open(&resolved.path)
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: format!("Failed to open {path_str}: {e}"),
            })?;
        let prior_bytes = file
            .metadata()
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: format!("Failed to inspect {path_str}: {e}"),
            })?
            .len();
        if !append {
            file.set_len(0)
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().into(),
                    message: format!("Failed to truncate {path_str}: {e}"),
                })?;
        }
        file.write_all(content.as_bytes()).await.map_err(|e| {
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

        file.flush()
            .await
            .map_err(|error| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: format!("Failed to flush {path_str}: {error}"),
            })?;
        let output = if append {
            format!("Appended {byte_count} bytes to {path_str}")
        } else if prior_bytes > 0 {
            format!("Wrote {byte_count} bytes to {path_str}; replaced {prior_bytes} bytes")
        } else {
            format!("Wrote {byte_count} bytes to {path_str}")
        };
        Ok(json!({"output": output}))
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
        let _ = dir;
        FileWriteTool
    }

    fn ctx(dir: &TempDir) -> crate::tool_executor::GatewayToolContext {
        crate::tool::workspace::writable_test_context(dir.path(), vec![])
    }

    #[tokio::test(flavor = "multi_thread")]
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

    #[tokio::test(flavor = "multi_thread")]
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

    #[tokio::test(flavor = "multi_thread")]
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

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(
                json!({ "path": "../../etc/passwd", "content": "bad" }),
                &ctx(&dir),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ToolError::RequiresLocationApproval { ref access, .. } if access == "write"
            ),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
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

        assert!(
            matches!(
                err,
                ToolError::RequiresLocationApproval { ref access, .. } if access == "write"
            ),
            "got: {err}"
        );
        assert!(!outside.path().join("escape.txt").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn returns_error_for_missing_path_argument() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "content": "hello" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("path"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
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

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_append_accumulates_and_preserves_sections() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = crate::tool::workspace::writable_test_context(dir.path(), vec![]);
        std::fs::write(dir.path().join("plan.md"), "# Ferry\nreference\n").unwrap();
        let mut expected = String::from("# Ferry\nreference\n");
        for i in 0..6 {
            let result = FileWriteTool
                .execute(
                    json!({"path":"plan.md", "content":format!("# Entry {i}\n"), "append":true}),
                    &ctx,
                )
                .await
                .unwrap();
            let content = std::fs::read_to_string(dir.path().join("plan.md")).unwrap();
            expected.push_str(&format!("# Entry {i}\n"));
            assert_eq!(content, expected, "append lost supplied or earlier content");
            assert!(result["output"].as_str().unwrap().starts_with("Appended"));
        }
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_replacement_reports_prior_size() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = crate::tool::workspace::writable_test_context(dir.path(), vec![]);
        std::fs::write(dir.path().join("notes"), "old content").unwrap();
        let result = FileWriteTool
            .execute(json!({"path":"notes", "content":"new"}), &ctx)
            .await
            .unwrap();
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("replaced 11 bytes"),
            "{result}"
        );
        assert_eq!(std::fs::read(dir.path().join("notes")).unwrap(), b"new");
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_append_rejects_non_boolean_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = crate::tool::workspace::writable_test_context(dir.path(), vec![]);
        let result = FileWriteTool
            .execute(
                json!({"path":"notes", "content":"new", "append":"true"}),
                &ctx,
            )
            .await;
        assert!(
            matches!(result, Err(ToolError::InvalidParameters { .. })),
            "{result:?}"
        );
        assert!(!dir.path().join("notes").exists());
    }
}

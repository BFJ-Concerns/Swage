//! File list tool — lists directory contents within the workspace.
//!
//! Returns entries sorted with directories first, then files, each
//! alphabetically. Validates the target path stays within the workspace root.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::{location_approval_error, resolve_context_path};

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileListTool;

#[async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("path"),
        }
    }

    fn description(&self) -> &str {
        "List the contents of a directory in the workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path relative to the workspace root. Use \".\" for the workspace root itself."
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

        let resolved = resolve_context_path(ctx, &path_str)
            .map_err(|_| location_approval_error(self.name(), ctx, &path_str, "read", true))?;

        // Verify the resolved path is a directory.
        match tokio::fs::metadata(&resolved.path).await {
            Ok(meta) if !meta.is_dir() => {
                return Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Not a directory: {path_str}"),
                });
            }
            Err(e) => {
                let message = match e.kind() {
                    std::io::ErrorKind::NotFound => {
                        format!("Directory not found: {path_str}")
                    }
                    std::io::ErrorKind::PermissionDenied => {
                        format!("Permission denied: {path_str}")
                    }
                    _ => format!("Failed to access {path_str}: {e}"),
                };
                return Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message,
                });
            }
            Ok(_) => {} // is a directory — continue
        }

        // Read directory entries.
        let mut read_dir =
            tokio::fs::read_dir(&resolved.path)
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Permission denied: {path_str}: {e}"),
                })?;

        let mut dirs: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = match entry.file_type().await {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            if file_type.is_dir() {
                dirs.push(format!("dir/  {name}"));
            } else if file_type.is_symlink() {
                // Show symlinks with a size of zero — resolving them would add
                // complexity and risk escaping the workspace check.
                files.push(format!("link  {name}"));
            } else {
                // For regular files, include the byte size.
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                files.push(format!("file  {name}  ({size} bytes)"));
            }
        }

        // Directories first, then files — each group sorted alphabetically.
        dirs.sort();
        files.sort();

        let mut lines = dirs;
        lines.extend(files);

        let content = if lines.is_empty() {
            format!("(empty directory: {path_str})")
        } else {
            lines.join("\n")
        };

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

    fn tool(dir: &TempDir) -> FileListTool {
        let _ = dir;
        FileListTool
    }

    fn ctx(dir: &TempDir) -> MockToolContext {
        let mut ctx = MockToolContext::new("agent-test", "sess-test");
        ctx.working_directory = dir.path().to_path_buf();
        ctx
    }

    #[tokio::test]
    async fn lists_files_and_dirs() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), b"hello").unwrap();

        let val = tool(&dir)
            .execute(json!({ "path": "." }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        assert!(output.contains("subdir"), "got: {output}");
        assert!(output.contains("file.txt"), "got: {output}");
    }

    #[tokio::test]
    async fn dirs_sorted_before_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("aaa.txt"), b"").unwrap();
        std::fs::create_dir(dir.path().join("zzz")).unwrap();

        let val = tool(&dir)
            .execute(json!({ "path": "." }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        let dir_pos = output.find("zzz").unwrap();
        let file_pos = output.find("aaa.txt").unwrap();
        assert!(dir_pos < file_pos, "dirs should precede files");
    }

    #[tokio::test]
    async fn includes_file_sizes() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.txt"), b"12345").unwrap();

        let val = tool(&dir)
            .execute(json!({ "path": "." }), &ctx(&dir))
            .await
            .unwrap();
        assert!(val["output"].as_str().unwrap().contains("5 bytes"));
    }

    #[tokio::test]
    async fn empty_directory() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("empty");
        std::fs::create_dir(&sub).unwrap();

        let val = tool(&dir)
            .execute(json!({ "path": "empty" }), &ctx(&dir))
            .await
            .unwrap();
        assert!(val["output"].as_str().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "../../etc" }), &ctx(&dir))
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
    async fn error_on_missing_directory() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "nonexistent" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("No such"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn error_on_file_not_directory() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"").unwrap();
        let err = tool(&dir)
            .execute(json!({ "path": "file.txt" }), &ctx(&dir))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Not a directory"), "got: {err}");
    }
}

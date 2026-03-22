//! File list tool — lists directory contents within the workspace.
//!
//! Returns entries sorted with directories first, then files, each
//! alphabetically. Validates the target path stays within the workspace root.

use std::path::PathBuf;
use async_trait::async_trait;
use serde_json::json;
use crate::tool::{Tool, ToolExecutionResult};
use crate::tool::workspace::validate_path;

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileListTool {
    pub workspace_root: PathBuf,
}

#[async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn description(&self) -> &str {
        "List the contents of a directory in the workspace."
    }

    fn parameters(&self) -> serde_json::Value {
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

        // Verify the resolved path is a directory.
        match tokio::fs::metadata(&resolved).await {
            Ok(meta) if !meta.is_dir() => {
                return ToolExecutionResult {
                    content: format!("Not a directory: {path_str}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
            Err(e) => {
                let msg = match e.kind() {
                    std::io::ErrorKind::NotFound => {
                        format!("Directory not found: {path_str}")
                    }
                    std::io::ErrorKind::PermissionDenied => {
                        format!("Permission denied: {path_str}")
                    }
                    _ => format!("Failed to access {path_str}: {e}"),
                };
                return ToolExecutionResult {
                    content: msg,
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
            Ok(_) => {} // is a directory — continue
        }

        // Read directory entries.
        let mut read_dir = match tokio::fs::read_dir(&resolved).await {
            Ok(rd) => rd,
            Err(e) => {
                return ToolExecutionResult {
                    content: format!("Permission denied: {path_str}: {e}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

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
                let size = entry
                    .metadata()
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
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

        ToolExecutionResult {
            content,
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

    fn tool(dir: &TempDir) -> FileListTool {
        FileListTool {
            workspace_root: dir.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn lists_files_and_dirs() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), b"hello").unwrap();

        let result = tool(&dir).execute(json!({ "path": "." })).await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("subdir"), "got: {}", result.content);
        assert!(result.content.contains("file.txt"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn dirs_sorted_before_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("aaa.txt"), b"").unwrap();
        std::fs::create_dir(dir.path().join("zzz")).unwrap();

        let result = tool(&dir).execute(json!({ "path": "." })).await;
        assert!(!result.is_error, "got: {}", result.content);
        let dir_pos = result.content.find("zzz").unwrap();
        let file_pos = result.content.find("aaa.txt").unwrap();
        assert!(dir_pos < file_pos, "dirs should precede files");
    }

    #[tokio::test]
    async fn includes_file_sizes() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.txt"), b"12345").unwrap();

        let result = tool(&dir).execute(json!({ "path": "." })).await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("5 bytes"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn empty_directory() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("empty");
        std::fs::create_dir(&sub).unwrap();

        let result = tool(&dir).execute(json!({ "path": "empty" })).await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("empty"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "../../etc" }))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("outside"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn error_on_missing_directory() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "nonexistent" }))
            .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("not found") || result.content.contains("No such"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn error_on_file_not_directory() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"").unwrap();
        let result = tool(&dir)
            .execute(json!({ "path": "file.txt" }))
            .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Not a directory"),
            "got: {}",
            result.content
        );
    }
}

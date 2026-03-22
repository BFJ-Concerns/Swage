//! File search tool — searches file contents by pattern within the workspace.
//!
//! Walks the directory tree, skipping hidden directories, node_modules, target,
//! and .git. Returns matching lines in "path:line_number: content" format,
//! truncated if the total output exceeds the configured size limit.

use std::path::{Path, PathBuf};
use async_trait::async_trait;
use serde_json::json;
use crate::tool::{Tool, ToolExecutionResult};
use crate::tool::workspace::{validate_path, is_binary_content, truncate_output};

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileSearchTool {
    pub workspace_root: PathBuf,
    pub max_output_bytes: usize,
}

/// Directories to skip during the tree walk — they are either generated output
/// or version control internals that rarely contain useful search targets.
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", ".svn", "__pycache__", ".venv", "venv",
];

#[async_trait]
impl Tool for FileSearchTool {
    fn name(&self) -> &str {
        "file_search"
    }

    fn description(&self) -> &str {
        "Search file contents for a pattern across the workspace or a subdirectory."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Case-insensitive substring pattern to search for."
                },
                "path": {
                    "type": "string",
                    "description": "Subdirectory relative to workspace root to restrict the search. Defaults to \".\" (entire workspace)."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        let pattern = match arguments["pattern"].as_str() {
            Some(p) => p.to_lowercase(),
            None => {
                return ToolExecutionResult {
                    content: "Missing required argument: pattern".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        // Optional subdirectory — default to workspace root.
        let search_root = match arguments["path"].as_str() {
            Some(sub) => match validate_path(&self.workspace_root, sub) {
                Ok(p) => p,
                Err(msg) => {
                    return ToolExecutionResult {
                        content: msg,
                        is_error: true,
                        truncated: false,
                        total_bytes: None,
                    }
                }
            },
            None => self.workspace_root.clone(),
        };

        // Collect matches by walking the tree synchronously — tokio's fs helpers
        // don't expose a recursive walk, and spinning up hundreds of tasks for
        // each file would add unnecessary overhead for a sequential scan.
        let matches = search_tree(&search_root, &self.workspace_root, &pattern);

        if matches.is_empty() {
            return ToolExecutionResult {
                content: format!("No matches found for pattern: {pattern}"),
                is_error: false,
                truncated: false,
                total_bytes: None,
            };
        }

        let raw = matches.join("\n");
        let (content, truncated, total_bytes) = truncate_output(&raw, self.max_output_bytes);

        ToolExecutionResult {
            content,
            is_error: false,
            truncated,
            total_bytes: if truncated { Some(total_bytes) } else { None },
        }
    }
}

/// Walk `root` recursively, returning "rel_path:line_num: line" for every
/// line in every text file that contains `pattern` (case-insensitive).
fn search_tree(root: &Path, workspace_root: &Path, pattern: &str) -> Vec<String> {
    let mut results = Vec::new();
    search_dir(root, workspace_root, pattern, &mut results);
    results
}

fn search_dir(dir: &Path, workspace_root: &Path, pattern: &str, results: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    // Sort for deterministic output order.
    paths.sort();

    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if path.is_dir() {
            // Skip hidden directories and known build/vcs dirs.
            if name.starts_with('.') || SKIP_DIRS.contains(&name) {
                continue;
            }
            search_dir(&path, workspace_root, pattern, results);
        } else if path.is_file() {
            search_file(&path, workspace_root, pattern, results);
        }
    }
}

fn search_file(path: &Path, workspace_root: &Path, pattern: &str, results: &mut Vec<String>) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };

    // Skip binary files — they produce garbled output and are rarely useful.
    if is_binary_content(&bytes) {
        return;
    }

    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(_) => return, // Invalid UTF-8 — skip
    };

    // Make the path relative to the workspace root for readable output.
    let rel = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy();

    for (line_num, line) in text.lines().enumerate() {
        if line.to_lowercase().contains(pattern) {
            results.push(format!("{}:{}: {}", rel, line_num + 1, line));
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

    fn tool(dir: &TempDir) -> FileSearchTool {
        FileSearchTool {
            workspace_root: dir.path().to_path_buf(),
            max_output_bytes: 1_000_000,
        }
    }

    fn tool_with_limit(dir: &TempDir, limit: usize) -> FileSearchTool {
        FileSearchTool {
            workspace_root: dir.path().to_path_buf(),
            max_output_bytes: limit,
        }
    }

    #[tokio::test]
    async fn finds_match_in_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"Hello world\nfoo bar\n").unwrap();
        let result = tool(&dir)
            .execute(json!({ "pattern": "hello" }))
            .await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("notes.txt"), "got: {}", result.content);
        assert!(result.content.contains(":1:"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"UPPERCASE match here\n").unwrap();
        let result = tool(&dir)
            .execute(json!({ "pattern": "uppercase" }))
            .await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("a.txt"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn no_match_returns_message_not_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"nothing relevant\n").unwrap();
        let result = tool(&dir)
            .execute(json!({ "pattern": "xyzzy_no_match" }))
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("No matches"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn searches_multiple_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"foo in a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"foo in b\n").unwrap();
        let result = tool(&dir).execute(json!({ "pattern": "foo" })).await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("a.txt"), "got: {}", result.content);
        assert!(result.content.contains("b.txt"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn scoped_path_restricts_search() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inside.txt"), b"target pattern\n").unwrap();
        std::fs::write(dir.path().join("outside.txt"), b"target pattern\n").unwrap();

        let result = tool(&dir)
            .execute(json!({ "pattern": "target", "path": "sub" }))
            .await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.content.contains("inside.txt"), "got: {}", result.content);
        // The outside file should not appear in a scoped search.
        assert!(!result.content.contains("outside.txt"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn skips_binary_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("binary.bin"), b"\x00\x01\x02pattern\x03").unwrap();
        // Binary file — even if pattern byte sequence is present, skip it.
        let result = tool(&dir)
            .execute(json!({ "pattern": "pattern" }))
            .await;
        // Either no match or the binary file is not reported as a text match.
        assert!(!result.content.contains("binary.bin"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn truncates_large_results() {
        let dir = TempDir::new().unwrap();
        // Write a file where every line matches — generates lots of output.
        let content: String = (1..=50).map(|i| format!("match line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), content.as_bytes()).unwrap();
        let result = tool_with_limit(&dir, 50)
            .execute(json!({ "pattern": "match" }))
            .await;
        assert!(!result.is_error, "got: {}", result.content);
        assert!(result.truncated);
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let result = tool(&dir)
            .execute(json!({ "pattern": "x", "path": "../../etc" }))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("outside"), "got: {}", result.content);
    }
}

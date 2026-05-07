//! File search tool — searches file contents by pattern within the workspace.
//!
//! Walks the directory tree, skipping hidden directories, node_modules, target,
//! and .git. Returns matching lines in "path:line_number: content" format,
//! truncated if the total output exceeds the configured size limit.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::{
    is_binary_content, location_approval_error, resolve_context_path, truncate_output,
};

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct FileSearchTool {
    pub max_output_bytes: usize,
}

/// Directories to skip during the tree walk — they are either generated output
/// or version control internals that rarely contain useful search targets.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".svn",
    "__pycache__",
    ".venv",
    "venv",
];

#[async_trait]
impl Tool for FileSearchTool {
    fn name(&self) -> &str {
        "file_search"
    }

    fn description(&self) -> &str {
        "Search file contents for a pattern across the workspace or a subdirectory."
    }

    fn parameters(&self) -> Value {
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

    fn requires_host_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let pattern = params["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: pattern".to_owned(),
            })?
            .to_lowercase();

        // Optional subdirectory — default to workspace root.
        let resolved = match params["path"].as_str() {
            Some(sub) => resolve_context_path(ctx, sub)
                .map_err(|_| location_approval_error(self.name(), ctx, sub, "read"))?,
            None => resolve_context_path(ctx, ".")
                .map_err(|_| location_approval_error(self.name(), ctx, ".", "read"))?,
        };

        // Collect matches by walking the tree synchronously.
        let matches = search_tree(&resolved.path, &resolved.root, &pattern);

        if matches.is_empty() {
            return Ok(json!({"output": format!("No matches found for pattern: {pattern}")}));
        }

        let raw = matches.join("\n");
        let (content, _truncated, _total_bytes) = truncate_output(&raw, self.max_output_bytes);

        Ok(json!({"output": content}))
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

    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    // Sort for deterministic output order.
    paths.sort();

    for path in paths {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

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
    use swage_tool::context::MockToolContext;
    use tempfile::TempDir;

    fn tool(dir: &TempDir) -> FileSearchTool {
        let _ = dir;
        FileSearchTool {
            max_output_bytes: 1_000_000,
        }
    }

    fn tool_with_limit(dir: &TempDir, limit: usize) -> FileSearchTool {
        let _ = dir;
        FileSearchTool {
            max_output_bytes: limit,
        }
    }

    fn ctx(dir: &TempDir) -> MockToolContext {
        let mut ctx = MockToolContext::new("agent-test", "sess-test");
        ctx.working_directory = dir.path().to_path_buf();
        ctx
    }

    #[tokio::test]
    async fn finds_match_in_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"Hello world\nfoo bar\n").unwrap();
        let val = tool(&dir)
            .execute(json!({ "pattern": "hello" }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        assert!(output.contains("notes.txt"), "got: {output}");
        assert!(output.contains(":1:"), "got: {output}");
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"UPPERCASE match here\n").unwrap();
        let val = tool(&dir)
            .execute(json!({ "pattern": "uppercase" }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        assert!(output.contains("a.txt"), "got: {output}");
    }

    #[tokio::test]
    async fn no_match_returns_message_not_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"nothing relevant\n").unwrap();
        let val = tool(&dir)
            .execute(json!({ "pattern": "xyzzy_no_match" }), &ctx(&dir))
            .await
            .unwrap();
        assert!(val["output"].as_str().unwrap().contains("No matches"));
    }

    #[tokio::test]
    async fn searches_multiple_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"foo in a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"foo in b\n").unwrap();
        let val = tool(&dir)
            .execute(json!({ "pattern": "foo" }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        assert!(output.contains("a.txt"), "got: {output}");
        assert!(output.contains("b.txt"), "got: {output}");
    }

    #[tokio::test]
    async fn scoped_path_restricts_search() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inside.txt"), b"target pattern\n").unwrap();
        std::fs::write(dir.path().join("outside.txt"), b"target pattern\n").unwrap();

        let val = tool(&dir)
            .execute(json!({ "pattern": "target", "path": "sub" }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        assert!(output.contains("inside.txt"), "got: {output}");
        // The outside file should not appear in a scoped search.
        assert!(!output.contains("outside.txt"), "got: {output}");
    }

    #[tokio::test]
    async fn skips_binary_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("binary.bin"), b"\x00\x01\x02pattern\x03").unwrap();
        let val = tool(&dir)
            .execute(json!({ "pattern": "pattern" }), &ctx(&dir))
            .await
            .unwrap();
        assert!(!val["output"].as_str().unwrap().contains("binary.bin"));
    }

    #[tokio::test]
    async fn truncates_large_results() {
        let dir = TempDir::new().unwrap();
        let content: String = (1..=50).map(|i| format!("match line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), content.as_bytes()).unwrap();
        let val = tool_with_limit(&dir, 50)
            .execute(json!({ "pattern": "match" }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        assert!(
            output.len() <= 50,
            "output should be truncated: len={}",
            output.len()
        );
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let dir = TempDir::new().unwrap();
        let err = tool(&dir)
            .execute(json!({ "pattern": "x", "path": "../../etc" }), &ctx(&dir))
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
}

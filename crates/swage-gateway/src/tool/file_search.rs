//! File search tool — searches file contents by pattern within the workspace.
//!
//! Walks the directory tree, skipping hidden directories, node_modules, target,
//! and .git. Returns matching lines in "path:line_number: content" format,
//! truncated if the total output exceeds the configured size limit. Binary
//! files are recognised from their leading bytes and never read in full.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::{
    continuable_output_value, location_approval_error, read_text_sample, resolve_context_path,
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

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("pattern"),
        }
    }

    fn description(&self) -> &str {
        "Search file contents for a pattern across the workspace or a subdirectory."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "justification": {
                    "type": "string",
                    "description": "Why this file is needed when it lies outside the session's granted locations, in one or two sentences. Read by whoever answers for the request — the person, or the judge when nobody is watching; omitted, the request is weighed without a reason."
                },
                "pattern": {
                    "type": "string",
                    "description": "Case-insensitive substring pattern to search for."
                },
                "path": {
                    "type": "string",
                    "description": "Subdirectory relative to workspace root to restrict the search. Defaults to \".\" (entire workspace)."
                },
                "offset_bytes": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Byte offset into the search result stream. Use next_offset_bytes from a truncated result to continue."
                }
            },
            "required": ["pattern"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let pattern_arg =
            params["pattern"]
                .as_str()
                .ok_or_else(|| ToolError::InvalidParameters {
                    name: self.name().to_owned(),
                    message: "Missing required argument: pattern".to_owned(),
                })?;
        let pattern = pattern_arg.to_lowercase();
        let offset_bytes = optional_offset_bytes(&params, self.name())?;

        // Optional subdirectory — default to workspace root.
        let path_arg = params["path"].as_str().unwrap_or(".");
        let resolved = match params["path"].as_str() {
            Some(sub) => resolve_context_path(ctx, sub)
                .map_err(|_| location_approval_error(self.name(), ctx, sub, "read", true))?,
            None => resolve_context_path(ctx, ".")
                .map_err(|_| location_approval_error(self.name(), ctx, ".", "read", true))?,
        };

        // Collect matches by walking the tree synchronously.
        let matches = search_tree(&resolved.path, &resolved.root, &pattern);

        if matches.is_empty() {
            return Ok(json!({"output": format!("No matches found for pattern: {pattern}")}));
        }

        let raw = matches.join("\n");
        let mut arguments = serde_json::Map::new();
        arguments.insert("pattern".to_string(), json!(pattern_arg));
        if params["path"].as_str().is_some() {
            arguments.insert("path".to_string(), json!(path_arg));
        }
        continuable_output_value(
            &raw,
            self.max_output_bytes,
            offset_bytes,
            "file_search",
            arguments,
        )
        .map_err(|message| ToolError::InvalidParameters {
            name: self.name().to_owned(),
            message,
        })
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
    let Ok(file) = File::open(path) else {
        return;
    };

    // Make the path relative to the workspace root for readable output.
    let rel = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy();

    if let Some(matches) = search_text(file, &rel, pattern) {
        results.extend(matches);
    }
}

/// Returns "rel:line_num: line" for every line of `source` that contains
/// `pattern`, or `None` when the source is not readable text.
///
/// The source is read line by line after its classification sample, so a
/// binary file is skipped without being read further and a large text file is
/// never held whole.
fn search_text(mut source: impl Read, rel: &str, pattern: &str) -> Option<Vec<String>> {
    // Skip binary files — they produce garbled output and are rarely useful.
    let sample = read_text_sample(&mut source).ok().flatten()?;

    let mut matches = Vec::new();
    let lines = BufReader::new(Cursor::new(sample).chain(source)).lines();
    for (line_num, line) in lines.enumerate() {
        // A line that is not UTF-8 or cannot be read leaves the file unsearchable
        // as text — skip it whole.
        let line = line.ok()?;
        if line.to_lowercase().contains(pattern) {
            matches.push(format!("{}:{}: {}", rel, line_num + 1, line));
        }
    }
    Some(matches)
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

    /// An endless stream of NUL bytes, standing in for a binary file too large
    /// to hold in memory. Reading more than a mebibyte of it fails the test.
    struct EndlessBinary {
        consumed: usize,
    }

    impl Read for EndlessBinary {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            assert!(
                self.consumed < 1 << 20,
                "binary content was read past its classification sample"
            );
            buf.fill(0);
            self.consumed += buf.len();
            Ok(buf.len())
        }
    }

    #[test]
    fn binary_content_is_not_read_past_its_sample() {
        let source = EndlessBinary { consumed: 0 };
        assert_eq!(search_text(source, "model.gguf", "pattern"), None);
    }

    #[tokio::test]
    async fn finds_matches_across_and_past_the_classification_sample() {
        let dir = TempDir::new().unwrap();
        let mut content = "a".repeat(8188);
        content.push_str(" needle one\n");
        content.push_str(&"filler\n".repeat(2000));
        content.push_str("needle two\n");
        std::fs::write(dir.path().join("long.txt"), content).unwrap();

        let val = tool(&dir)
            .execute(json!({ "pattern": "needle" }), &ctx(&dir))
            .await
            .unwrap();
        let output = val["output"].as_str().unwrap();
        assert!(output.starts_with("long.txt:1: aaa"), "got: {output}");
        assert!(output.contains(" needle one\n"), "got: {output}");
        assert!(
            output.ends_with("\nlong.txt:2002: needle two"),
            "got: {output}"
        );
    }

    #[tokio::test]
    async fn skips_non_utf8_file_even_after_a_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("latin1.txt"),
            b"needle first\ncaf\xe9 later\n",
        )
        .unwrap();
        let val = tool(&dir)
            .execute(json!({ "pattern": "needle" }), &ctx(&dir))
            .await
            .unwrap();
        assert!(
            val["output"].as_str().unwrap().contains("No matches"),
            "got: {}",
            val["output"]
        );
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
        assert_eq!(val["truncated"], json!(true));
        assert!(val["total_bytes"].as_u64().unwrap() > 50);
    }

    #[tokio::test]
    async fn truncated_search_returns_follow_up_offset() {
        let dir = TempDir::new().unwrap();
        let content: String = (1..=20).map(|i| format!("match line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), content.as_bytes()).unwrap();

        let val = tool_with_limit(&dir, 40)
            .execute(json!({ "pattern": "match", "path": "." }), &ctx(&dir))
            .await
            .unwrap();

        assert_eq!(val["truncated"], json!(true));
        assert_eq!(val["offset_bytes"], json!(0));
        let next_offset = val["next_offset_bytes"]
            .as_u64()
            .expect("truncated search should include next offset");
        assert!(next_offset > 0);
        assert_eq!(val["follow_up"]["tool"], json!("file_search"));
        assert_eq!(val["follow_up"]["arguments"]["pattern"], json!("match"));
        assert_eq!(val["follow_up"]["arguments"]["path"], json!("."));
        assert_eq!(
            val["follow_up"]["arguments"]["offset_bytes"],
            json!(next_offset)
        );
    }

    #[tokio::test]
    async fn offset_search_continues_from_prior_truncation() {
        let dir = TempDir::new().unwrap();
        let content: String = (1..=20).map(|i| format!("match line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), content.as_bytes()).unwrap();

        let first = tool_with_limit(&dir, 40)
            .execute(json!({ "pattern": "match" }), &ctx(&dir))
            .await
            .unwrap();
        let next_offset = first["next_offset_bytes"]
            .as_u64()
            .expect("first surface should be truncated");

        let second = tool_with_limit(&dir, 40)
            .execute(
                json!({ "pattern": "match", "offset_bytes": next_offset }),
                &ctx(&dir),
            )
            .await
            .unwrap();

        assert_ne!(first["output"], second["output"]);
        assert_eq!(second["offset_bytes"], json!(next_offset));
        assert_eq!(second["total_bytes"], first["total_bytes"]);
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

//! File search tool — searches file contents by pattern within the workspace.
//!
//! Walks the directory tree, skipping hidden directories, node_modules, target,
//! and .git. Returns matching lines in "path:line_number: content" format,
//! truncated if the total output exceeds the configured size limit. Binary
//! files are recognised from their leading bytes and never read in full.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::{location_approval_error, read_text_sample, resolve_context_path};

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

        let cap = self.max_output_bytes;
        if cap == 0 {
            return Err(ToolError::InvalidParameters {
                name: self.name().into(),
                message: "max_output_bytes must be positive".into(),
            });
        }
        let search_pattern = pattern.clone();
        let window = run_search(std::time::Duration::from_secs(30), move |control| {
            let mut window = SearchWindow::new(offset_bytes, cap);
            search_tree(
                &resolved.path,
                &resolved.root,
                &search_pattern,
                &mut window,
                &control,
            )?;
            Ok(window)
        })
        .await
        .map_err(|message| ToolError::ExecutionFailed {
            name: self.name().into(),
            message,
        })?;
        if window.matches == 0 {
            return Ok(json!({"output": format!("No matches found for pattern: {pattern}")}));
        }
        if !window.boundary || offset_bytes > window.total_bytes {
            return Err(ToolError::InvalidParameters {
                name: self.name().into(),
                message: "offset_bytes must be a valid UTF-8 byte boundary within the result"
                    .into(),
            });
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("pattern".to_string(), json!(pattern_arg));
        if params["path"].as_str().is_some() {
            arguments.insert("path".to_string(), json!(path_arg));
        }
        let next = offset_bytes + window.output.len();
        let mut result = json!({"output": window.output});
        if offset_bytes > 0 || next < window.total_bytes {
            result["offset_bytes"] = json!(offset_bytes);
            result["total_bytes"] = json!(window.total_bytes);
        }
        if next < window.total_bytes {
            arguments.insert("offset_bytes".into(), json!(next));
            result["truncated"] = json!(true);
            result["next_offset_bytes"] = json!(next);
            result["follow_up"] = json!({"tool": "file_search", "arguments": arguments});
        }
        Ok(result)
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

/// A bounded slice of the result stream, with counts independent of retention.
#[derive(Clone)]
struct SearchWindow {
    output: String,
    total_bytes: usize,
    matches: usize,
    offset: usize,
    limit: usize,
    boundary: bool,
}

impl SearchWindow {
    fn new(offset: usize, limit: usize) -> Self {
        Self {
            output: String::new(),
            total_bytes: 0,
            matches: 0,
            offset,
            limit,
            boundary: offset == 0,
        }
    }

    /// A line retains only its own slice; earlier results never need copying.
    fn commit_line(&mut self, line: &Self) {
        self.output.push_str(&line.output);
        self.total_bytes = line.total_bytes;
        self.boundary = line.boundary;
        self.matches += 1;
    }

    fn push(&mut self, text: &str) {
        if self.total_bytes >= self.offset.saturating_add(self.limit) {
            if self.total_bytes == self.offset {
                self.boundary = true;
            }
            self.total_bytes += text.len();
            return;
        }
        for ch in text.chars() {
            if self.total_bytes == self.offset {
                self.boundary = true;
            }
            let end = self.total_bytes + ch.len_utf8();
            if self.total_bytes >= self.offset && end <= self.offset.saturating_add(self.limit) {
                self.output.push(ch);
            }
            self.total_bytes = end;
        }
        if self.total_bytes == self.offset {
            self.boundary = true;
        }
    }
}

#[derive(Clone)]
struct SearchControl {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    deadline: std::time::Instant,
}

impl SearchControl {
    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            Err("File search cancelled".into())
        } else if std::time::Instant::now() >= self.deadline {
            Err("File search deadline exceeded".into())
        } else {
            Ok(())
        }
    }
}

struct CancelSearch(SearchControl);
impl Drop for CancelSearch {
    fn drop(&mut self) {
        self.0
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

async fn run_search<F>(timeout: std::time::Duration, operation: F) -> Result<SearchWindow, String>
where
    F: FnOnce(SearchControl) -> Result<SearchWindow, String> + Send + 'static,
{
    let control = SearchControl {
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        deadline: std::time::Instant::now() + timeout,
    };
    let _cancel = CancelSearch(control.clone());
    tokio::task::spawn_blocking(move || operation(control))
        .await
        .map_err(|error| error.to_string())?
}

fn search_tree(
    root: &Path,
    workspace_root: &Path,
    pattern: &str,
    window: &mut SearchWindow,
    control: &SearchControl,
) -> Result<(), String> {
    let mut visited = std::collections::HashSet::new();
    search_dir(root, workspace_root, pattern, window, control, &mut visited)
}

fn search_dir(
    dir: &Path,
    workspace_root: &Path,
    pattern: &str,
    window: &mut SearchWindow,
    control: &SearchControl,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), String> {
    control.check()?;
    let Ok(canonical) = dir.canonicalize() else {
        return Ok(());
    };
    if !canonical.starts_with(workspace_root) || !visited.insert(canonical) {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        control.check()?;
        if let Ok(entry) = entry {
            paths.push(entry.path());
        }
    }
    paths.sort();
    for path in paths {
        control.check()?;
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(workspace_root) {
            continue;
        }
        let Ok(metadata) = canonical.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && !SKIP_DIRS.contains(&name) {
                search_dir(&path, workspace_root, pattern, window, control, visited)?;
            }
        } else if metadata.is_file() {
            let Ok(file) = File::open(&canonical) else {
                continue;
            };
            let rel = path
                .strip_prefix(workspace_root)
                .unwrap_or(&path)
                .to_string_lossy();
            search_into(file, &rel, pattern, window, control)?;
        }
    }
    Ok(())
}

/// Streams UTF-8 and matching lines with bounded retention, committing a file's
/// results only after all its bytes have proved readable as text.
fn search_into(
    mut source: impl Read,
    rel: &str,
    pattern: &str,
    window: &mut SearchWindow,
    control: &SearchControl,
) -> Result<(), String> {
    control.check()?;
    let Some(sample) = read_text_sample(&mut source).ok().flatten() else {
        return Ok(());
    };
    let mut source = Cursor::new(sample).chain(source);
    let mut file_window = window.clone();
    let mut line_window = SearchWindow::new(window.offset, window.limit);
    let mut line_number = 1;
    let mut line_started = false;
    let mut matcher = LineMatcher::new(pattern);
    let mut pending_cr = false;
    let mut pending = Vec::with_capacity(8196);
    let mut chunk = [0u8; 8192];
    loop {
        control.check()?;
        let n = match source.read(&mut chunk) {
            Ok(n) => n,
            Err(_) => return Ok(()),
        };
        pending.extend_from_slice(&chunk[..n]);
        let (valid, incomplete) = match std::str::from_utf8(&pending) {
            Ok(text) => (text.len(), false),
            Err(error) if error.error_len().is_none() && n != 0 => (error.valid_up_to(), true),
            Err(_) => return Ok(()),
        };
        let text = std::str::from_utf8(&pending[..valid]).expect("validated prefix");
        for ch in text.chars() {
            if !line_started {
                line_window.output.clear();
                line_window.total_bytes = file_window.total_bytes;
                line_window.boundary = file_window.boundary;
                if file_window.matches > 0 {
                    line_window.push("\n");
                }
                line_window.push(&format!("{rel}:{line_number}: "));
                line_started = true;
            }
            if ch == '\n' {
                if matcher.matched() {
                    file_window.commit_line(&line_window);
                }
                line_number += 1;
                line_started = false;
                matcher.reset();
                pending_cr = false;
            } else {
                if pending_cr {
                    push_search_character('\r', &mut line_window, &mut matcher);
                }
                pending_cr = ch == '\r';
                if !pending_cr {
                    push_search_character(ch, &mut line_window, &mut matcher);
                }
            }
        }
        pending.drain(..valid);
        if n == 0 {
            if incomplete || !pending.is_empty() {
                return Ok(());
            }
            break;
        }
    }
    if pending_cr {
        push_search_character('\r', &mut line_window, &mut matcher);
    }
    if line_started && matcher.matched() {
        file_window.commit_line(&line_window);
    }
    *window = file_window;
    Ok(())
}

fn push_search_character(ch: char, window: &mut SearchWindow, matcher: &mut LineMatcher) {
    window.push(ch.encode_utf8(&mut [0; 4]));
    matcher.push(ch);
}

#[derive(Clone)]
struct MatchState {
    position: usize,
    matched: bool,
}

impl MatchState {
    fn push(&mut self, ch: char, needle: &[char], prefix: &[usize]) {
        if self.matched {
            return;
        }
        while self.position > 0 && needle[self.position] != ch {
            self.position = prefix[self.position - 1];
        }
        if needle[self.position] == ch {
            self.position += 1;
        }
        if self.position == needle.len() {
            self.matched = true;
        }
    }
}

/// Matches the same lowercase stream as `str::to_lowercase`, without retaining
/// a line. A sigma after a cased character needs lookahead through arbitrarily
/// many case-ignorable characters: advance both possible match states until a
/// cased character, a boundary or line end selects the correct one.
struct LineMatcher {
    needle: Vec<char>,
    prefix: Vec<usize>,
    current: MatchState,
    final_sigma: Option<MatchState>,
    preceding_cased: bool,
}

impl LineMatcher {
    fn new(pattern: &str) -> Self {
        let needle: Vec<char> = pattern.chars().collect();
        let mut prefix = vec![0; needle.len()];
        for i in 1..needle.len() {
            let mut j = prefix[i - 1];
            while j > 0 && needle[i] != needle[j] {
                j = prefix[j - 1];
            }
            if needle[i] == needle[j] {
                j += 1;
            }
            prefix[i] = j;
        }
        Self {
            current: MatchState {
                position: 0,
                matched: needle.is_empty(),
            },
            needle,
            prefix,
            final_sigma: None,
            preceding_cased: false,
        }
    }

    fn reset(&mut self) {
        self.current = MatchState {
            position: 0,
            matched: self.needle.is_empty(),
        };
        self.final_sigma = None;
        self.preceding_cased = false;
    }

    fn matched(&self) -> bool {
        self.final_sigma.as_ref().unwrap_or(&self.current).matched
    }

    fn push(&mut self, ch: char) {
        if self.current.matched && self.final_sigma.as_ref().is_none_or(|state| state.matched) {
            return;
        }
        let (ignorable, cased) = case_context(ch);
        if !ignorable
            && let Some(final_sigma) = self.final_sigma.take()
            && !cased
        {
            self.current = final_sigma;
        }
        if ch == 'Σ' && self.preceding_cased {
            let mut final_sigma = self.current.clone();
            final_sigma.push('ς', &self.needle, &self.prefix);
            self.current.push('σ', &self.needle, &self.prefix);
            self.final_sigma = Some(final_sigma);
        } else {
            for lower in ch.to_lowercase() {
                self.current.push(lower, &self.needle, &self.prefix);
                if let Some(final_sigma) = &mut self.final_sigma {
                    final_sigma.push(lower, &self.needle, &self.prefix);
                }
            }
        }
        if !ignorable {
            self.preceding_cased = cased;
        }
    }
}

/// Classifies sigma context with the same Unicode version as pattern lowering.
/// The stable char API does not expose Case_Ignorable. Two fixed-size lowercase
/// probes distinguish a cased follower from an ignorable one: in `AΣx`, sigma
/// is non-final only for a non-ignorable cased x; appending A also makes it
/// non-final when x is ignorable. For a character that is both, ignorable takes precedence.
fn case_context(ch: char) -> (bool, bool) {
    if ch.is_ascii() {
        return (
            matches!(ch, '\'' | '.' | ':' | '^' | '`'),
            ch.is_ascii_alphabetic(),
        );
    }
    let mut probe = [0; 8];
    probe[..3].copy_from_slice("AΣ".as_bytes());
    let end = 3 + ch.encode_utf8(&mut probe[3..7]).len();
    let has_cased_follower = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .expect("UTF-8 case probe")
            .to_lowercase()
            .chars()
            .nth(1)
            == Some('σ')
    };
    if has_cased_follower(&probe[..end]) {
        return (false, true);
    }
    probe[end] = b'A';
    (has_cased_follower(&probe[..=end]), false)
}

#[cfg(test)]
fn search_text(source: impl Read, rel: &str, pattern: &str) -> Option<Vec<String>> {
    let mut window = SearchWindow::new(0, usize::MAX);
    let control = SearchControl {
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
    };
    search_into(source, rel, pattern, &mut window, &control).unwrap();
    (window.matches > 0).then(|| window.output.lines().map(str::to_owned).collect())
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

#[cfg(test)]
mod regression_tests {
    use super::*;
    fn control() -> SearchControl {
        SearchControl {
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(2),
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn regression_search_confines_symlinks_and_stops_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("inside"), "needle").unwrap();
        std::fs::write(outside.path().join("outside"), "needle").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside"),
            dir.path().join("file-escape"),
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("cycle")).unwrap();
        let mut ctx = swage_tool::context::MockToolContext::new("agent", "session");
        ctx.working_directory = dir.path().into();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            FileSearchTool {
                max_output_bytes: 1000,
            }
            .execute(json!({"pattern":"needle"}), &ctx),
        )
        .await;
        assert!(result.is_ok(), "search did not stop at symlink cycle");
        assert_eq!(result.unwrap().unwrap()["output"], "inside:1: needle");
    }
    #[tokio::test]
    async fn regression_search_large_file_after_window_fills() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(dir.path().join("large.txt")).unwrap();
        let first_line = "needle ".repeat(14_286);
        writeln!(file, "{first_line}").unwrap();
        // Short non-matching lines stress retention overhead after the default
        // result window fills, without constructing the input in memory.
        const LINES: usize = 24_000_000;
        let block = "x\n".repeat(10_000);
        for _ in 0..LINES / 10_000 {
            file.write_all(block.as_bytes()).unwrap();
        }
        file.write_all(b"needle tail\n").unwrap();
        drop(file);

        let mut ctx = swage_tool::context::MockToolContext::new("agent", "session");
        ctx.working_directory = dir.path().into();
        let result = FileSearchTool {
            max_output_bytes: 100_000,
        }
        .execute(json!({"pattern": "needle"}), &ctx)
        .await;
        assert!(
            result.is_ok(),
            "search must finish after its retained window fills: {result:?}"
        );
        let result = result.unwrap();
        let expected = format!(
            "large.txt:1: {first_line}\nlarge.txt:{}: needle tail",
            LINES + 2
        );
        assert_eq!(result["output"], &expected[..100_000]);
        assert_eq!(result["total_bytes"], expected.len());
        assert_eq!(result["truncated"], true);
        assert_eq!(result["next_offset_bytes"], 100_000);
    }

    #[test]
    fn regression_search_retains_window_counts_and_rejects_invalid_files() {
        let text = "needle\n".repeat(10_000);
        let mut window = SearchWindow::new(100, 37);
        search_into(text.as_bytes(), "notes", "needle", &mut window, &control()).unwrap();
        let expected = (1..=10_000)
            .map(|n| format!("notes:{n}: needle"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(window.output, expected[100..137]);
        assert_eq!(window.matches, 10_000);
        assert_eq!(window.total_bytes, expected.len());
        let mut invalid = text.into_bytes();
        invalid.push(0xff);
        search_into(
            invalid.as_slice(),
            "invalid",
            "needle",
            &mut window,
            &control(),
        )
        .unwrap();
        assert_eq!(
            window.matches, 10_000,
            "invalid file committed provisional matches"
        );
        struct SampleOnly {
            consumed: usize,
        }
        impl Read for SampleOnly {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    self.consumed + buffer.len() <= 8192,
                    "binary search read past its classification sample"
                );
                buffer.fill(0);
                self.consumed += buffer.len();
                Ok(buffer.len())
            }
        }
        let mut binary = SearchWindow::new(0, 37);
        search_into(
            SampleOnly { consumed: 0 },
            "binary",
            "needle",
            &mut binary,
            &control(),
        )
        .unwrap();
        assert_eq!(binary.matches, 0);
    }
    #[test]
    fn regression_search_preserves_crlf_unicode_and_byte_offsets() {
        let mut window = SearchWindow::new(0, 100);
        search_into(
            "CAFÉ needle\r\nother needle\n".as_bytes(),
            "notes",
            "café",
            &mut window,
            &control(),
        )
        .unwrap();
        assert_eq!(window.output, "notes:1: CAFÉ needle");
    }
    #[tokio::test]
    async fn regression_search_deadline_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes"), "needle").unwrap();
        let path = dir.path().to_path_buf();
        let result = run_search(std::time::Duration::ZERO, move |control| {
            let mut window = SearchWindow::new(0, 100);
            search_tree(&path, &path, "needle", &mut window, &control)?;
            Ok(window)
        })
        .await;
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("deadline")),
            "deadline was not enforced"
        );
    }
    #[tokio::test]
    async fn regression_search_cancellation_reaches_worker() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct SlowText {
            reads: usize,
            started: Arc<AtomicBool>,
        }
        impl Read for SlowText {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.started.store(true, Ordering::Release);
                if self.reads == 256 {
                    return Ok(0);
                }
                self.reads += 1;
                std::thread::sleep(std::time::Duration::from_millis(1));
                buffer.fill(b'a');
                Ok(buffer.len())
            }
        }
        let started = Arc::new(AtomicBool::new(false));
        let reader = SlowText {
            reads: 0,
            started: started.clone(),
        };
        let (settled, result) = tokio::sync::oneshot::channel();
        let execution = tokio::spawn(run_search(
            std::time::Duration::from_secs(2),
            move |control| {
                let mut window = SearchWindow::new(0, 32);
                let outcome = search_into(reader, "notes", "needle", &mut window, &control);
                let _ = settled.send(outcome);
                Ok(window)
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        execution.abort();
        let _ = execution.await;
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), result).await;
        assert!(
            outcome.is_ok(),
            "search worker continued after its caller was dropped"
        );
        assert!(
            outcome
                .unwrap()
                .unwrap()
                .is_err_and(|error| error.contains("cancelled")),
            "worker did not observe cancellation"
        );
    }
    #[tokio::test]
    async fn regression_search_preserves_contextual_unicode_lowercase() {
        let dir = tempfile::tempdir().unwrap();
        let words = [
            "ΟΣ",
            "ΟΣΑ",
            "Ο\u{301}Σ",
            "ΟΣ\u{301}",
            "ΟΣ\u{301}Α",
            "Σ",
            "AΣ'A",
            "AΣ' ",
            "ǅΣ",
            "ªΣ",
            "ΟΣ\u{345} ",
            "ΟΣ\u{200d}Α",
            "ΟΣ.Α",
            "ΟΣ:Α",
            "ΟΣ^Α",
            "ΟΣ`Α",
            "ΟΣ!Α",
            "ΟΣΣ",
            "ΟΣ\u{1ad0}Α",
            "ΟΣ\u{a7ce}",
        ];
        let text = words
            .map(|word| format!("padding padding padding padding {word}"))
            .join("\n");
        std::fs::write(dir.path().join("notes"), &text).unwrap();
        let mut ctx = swage_tool::context::MockToolContext::new("agent", "session");
        ctx.working_directory = dir.path().into();
        for pattern in [
            "ΟΣ",
            "ΟΣΑ",
            "Σ",
            "Ο\u{301}Σ",
            "AΣ'A",
            "ΟΣ\u{345}",
            "ΟΣ!Α",
            "ΟΣΣ",
            "ΟΣ\u{1ad0}Α",
            "ΟΣ\u{a7ce}",
        ] {
            let lower = pattern.to_lowercase();
            let expected = text
                .lines()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&lower))
                .map(|(i, line)| format!("notes:{}: {line}", i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !expected.is_empty(),
                "case has no reference matches: {pattern}"
            );
            let result = FileSearchTool {
                max_output_bytes: 10_000,
            }
            .execute(json!({"pattern":pattern}), &ctx)
            .await
            .unwrap();
            assert_eq!(
                result["output"], expected,
                "whole-line lowercase changed for {pattern}"
            );
        }
    }

    #[test]
    fn regression_search_context_spans_chunks_without_retaining_the_line() {
        let prefix = "p".repeat(8189);
        let marks = "\u{301}".repeat(12_000);
        let text = format!("{prefix}ΟΣ{marks}!\n{prefix}ΟΣ{marks}A\n");
        for pattern in ["ος", "οσ"] {
            let expected = text
                .lines()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(pattern))
                .map(|(i, line)| format!("notes:{}: {line}", i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            let mut window = SearchWindow::new(0, 19);
            search_into(text.as_bytes(), "notes", pattern, &mut window, &control()).unwrap();
            assert_eq!(
                window.matches, 1,
                "context across chunks changed matches for {pattern}"
            );
            assert_eq!(window.total_bytes, expected.len());
            assert_eq!(window.output, expected[..19]);
        }
    }

    #[tokio::test]
    async fn regression_existing_search_rejects_utf8_error_beyond_sample() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = b"needle\n".repeat(2000);
        bytes.push(0xff);
        std::fs::write(dir.path().join("notes"), bytes).unwrap();
        let mut ctx = swage_tool::context::MockToolContext::new("agent", "session");
        ctx.working_directory = dir.path().into();
        let result = FileSearchTool {
            max_output_bytes: 100,
        }
        .execute(json!({"pattern":"needle"}), &ctx)
        .await
        .unwrap();
        assert!(
            result["output"].as_str().unwrap().contains("No matches"),
            "{result}"
        );
    }
}

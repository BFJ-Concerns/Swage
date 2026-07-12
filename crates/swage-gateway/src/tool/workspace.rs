//! Workspace path resolution and scoping for file and shell tools.
//!
//! All file operations and shell commands are scoped to the space's configured
//! workspace directory. This module provides the shared logic for resolving the
//! workspace root and validating that paths don't escape it.

use serde_json::{Value, json};
use std::io;
use std::path::{Component, Path, PathBuf};
use swage_config::ResolvedConfig;
use swage_core::ToolError;
use swage_tool::context::ToolContext;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the canonical workspace root from config.
///
/// Uses `config.workspace.path` if set and non-empty. Canonicalises the
/// result so that symlinks cannot be used to bypass scoping checks later.
/// Falls back to the current working directory if canonicalisation fails.
// TODO Require explicit workspace root resolution
// Stop falling back to the gateway process CWD or "/" when the configured
// workspace path cannot be canonicalised. Daemon/global mode must fail clearly
// or resolve relative paths against a stable config-owned root so tool scope
// cannot drift between launches.
// ---
pub fn resolve_workspace_root(config: &ResolvedConfig) -> PathBuf {
    let raw = PathBuf::from(&config.workspace.path);
    // Canonicalize resolves symlinks and makes the path absolute. If it fails
    // (e.g. the configured path doesn't exist), fall back to current dir.
    raw.canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToolPath {
    pub root: PathBuf,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputArtifactPath {
    pub absolute: PathBuf,
    pub relative: String,
}

/// Builds a workspace-local path for a full tool-output artefact.
///
/// The path includes the turn id and tool-call id where available so repeated
/// calls do not overwrite each other while remaining readable by `file_read`.
pub fn tool_output_artifact_path(ctx: &dyn ToolContext, filename: &str) -> ToolOutputArtifactPath {
    let turn_id = ctx
        .turn_id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "adhoc".to_string());
    let call_id = ctx
        .call_id()
        .map(sanitise_path_segment)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let relative = format!(".swage/tool-output/{turn_id}/{call_id}/{filename}");
    let absolute = ctx.working_directory().join(&relative);

    ToolOutputArtifactPath { absolute, relative }
}

/// Creates a tool-output artefact file and any missing parent directories.
pub async fn create_tool_output_artifact(path: &Path) -> io::Result<tokio::fs::File> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::File::create(path).await
}

pub fn location_approval_error(
    tool_name: &str,
    ctx: &dyn ToolContext,
    requested: &str,
    access: &str,
) -> ToolError {
    let path = approval_root_for_request(ctx, requested).unwrap_or_else(|| requested.to_string());
    ToolError::RequiresLocationApproval {
        name: tool_name.to_string(),
        path,
        access: access.to_string(),
        timeout_secs: None,
        timeout_policy: None,
    }
}

/// Resolves a tool path against the current turn's workspace roots.
///
/// Relative paths are interpreted from `ctx.working_directory()`. Absolute
/// paths are accepted only when they remain inside the working directory or one
/// of `ctx.allowed_paths()`. Built-in file tools use this resolver as their
/// runtime permission boundary so location approvals can take effect without
/// rebuilding a sandbox mount set mid-turn.
pub fn resolve_context_path(
    ctx: &dyn ToolContext,
    requested: &str,
) -> Result<ResolvedToolPath, String> {
    let working_directory = canonical_existing_root(ctx.working_directory())?;
    let allowed_roots = context_roots(ctx)?;

    if Path::new(requested).is_absolute() {
        let mut last_error = None;
        for root in allowed_roots {
            match validate_path(&root, requested) {
                Ok(path) => return Ok(ResolvedToolPath { root, path }),
                Err(error) => last_error = Some(error),
            }
        }
        return Err(
            last_error.unwrap_or_else(|| format!("Path is outside the workspace: {requested}"))
        );
    }

    let path = validate_path(&working_directory, requested)?;
    Ok(ResolvedToolPath {
        root: working_directory,
        path,
    })
}

/// Resolves a not-yet-created tool path against the runtime workspace roots.
pub fn resolve_new_context_path(
    ctx: &dyn ToolContext,
    requested: &str,
) -> Result<ResolvedToolPath, String> {
    let working_directory = canonical_existing_root(ctx.working_directory())?;
    let allowed_roots = context_roots(ctx)?;

    if Path::new(requested).is_absolute() {
        let mut last_error = None;
        for root in allowed_roots {
            match validate_new_path(&root, requested) {
                Ok(path) => return Ok(ResolvedToolPath { root, path }),
                Err(error) => last_error = Some(error),
            }
        }
        return Err(
            last_error.unwrap_or_else(|| format!("Path is outside the workspace: {requested}"))
        );
    }

    let path = validate_new_path(&working_directory, requested)?;
    Ok(ResolvedToolPath {
        root: working_directory,
        path,
    })
}

/// Validates that `requested` resolves to a path inside `workspace_root`.
///
/// Returns the resolved absolute path on success, or an error message if
/// the path escapes the workspace (via `..`, absolute paths, or symlinks).
///
/// Works with both existing and not-yet-existing paths — the normalisation
/// step resolves `..` components without requiring the path to be on disk.
/// If the path does exist, a final `canonicalize` catches any symlink escapes.
pub fn validate_path(workspace_root: &Path, requested: &str) -> Result<PathBuf, String> {
    // Absolute paths are only accepted if they fall inside the workspace root.
    let raw = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        workspace_root.join(requested)
    };

    // Manually normalise `.` and `..` without requiring the path to exist.
    let normalised = normalise_path(&raw);

    // The workspace root itself must be canonical so starts_with is reliable.
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    // Reject traversals that escape the workspace.
    if !normalised.starts_with(&canonical_root) {
        return Err(format!("Path is outside the workspace: {requested}"));
    }

    let resolved = resolve_existing_prefix(&normalised, requested)?;
    if !resolved.starts_with(&canonical_root) {
        return Err(format!("Path is outside the workspace: {requested}"));
    }

    Ok(resolved)
}

/// Validates a not-yet-created path without allowing symlink ancestors to
/// redirect the final write outside the workspace.
pub fn validate_new_path(workspace_root: &Path, requested: &str) -> Result<PathBuf, String> {
    let resolved = validate_path(workspace_root, requested)?;
    if resolved.file_name().is_none() {
        return Err(format!("Path has no file name: {requested}"));
    }
    Ok(resolved)
}

/// Heuristic check: returns `true` if `bytes` appears to be binary content.
///
/// Scans the first 8 KiB for null bytes (reliable binary indicator) and
/// checks the ratio of non-UTF-8 bytes. Plain text files may contain a few
/// high bytes, but binary files tend to have many.
pub fn is_binary_content(bytes: &[u8]) -> bool {
    // Only inspect the first 8 KiB — enough to classify most files quickly.
    let sample = &bytes[..bytes.len().min(8192)];

    if sample.is_empty() {
        return false;
    }

    // Null byte is a reliable indicator of binary content.
    if sample.contains(&0u8) {
        return true;
    }

    // High ratio of non-UTF-8 bytes also suggests binary.
    let non_utf8 = sample.iter().filter(|&&b| b >= 0x80).count();

    // Threshold: more than 30% non-ASCII bytes → binary.
    non_utf8 * 100 / sample.len() > 30
}

/// Truncates `content` to at most `max_bytes` bytes at a valid UTF-8 boundary.
///
/// Returns `(truncated_content, was_truncated, total_bytes)`. When truncation
/// occurs the returned string ends on a character boundary so it is always
/// valid UTF-8.
pub fn truncate_output(content: &str, max_bytes: usize) -> (String, bool, usize) {
    let total = content.len();

    if total <= max_bytes {
        return (content.to_string(), false, total);
    }

    // Find the largest byte index ≤ max_bytes that is a char boundary.
    let mut cut = max_bytes;
    while cut > 0 && !content.is_char_boundary(cut) {
        cut -= 1;
    }

    (content[..cut].to_string(), true, total)
}

/// Builds the standard JSON result for tools that may shorten textual output.
///
/// `output` is always capped before it is returned. Truncation metadata is only
/// included when bytes were actually omitted, preserving the long-standing shape
/// for ordinary small results while making shortened responses explicit.
pub fn truncated_output_value(content: &str, max_bytes: usize) -> Value {
    let (output, truncated, total_bytes) = truncate_output(content, max_bytes);
    if truncated {
        json!({
            "output": output,
            "truncated": true,
            "total_bytes": total_bytes,
        })
    } else {
        json!({ "output": output })
    }
}

/// Builds JSON output for a textual result that can be continued by calling a
/// follow-up tool with a byte offset.
///
/// Small first-page results keep the historical `{ "output": ... }` shape.
/// Offset or truncated results include the offset, total byte count, and a
/// ready-to-use follow-up invocation when more content remains.
pub fn continuable_output_value(
    content: &str,
    max_bytes: usize,
    offset_bytes: usize,
    follow_up_tool: &str,
    mut follow_up_arguments: serde_json::Map<String, Value>,
) -> Result<Value, String> {
    if max_bytes == 0 {
        return Err("max_output_bytes must be positive".to_owned());
    }
    if offset_bytes > content.len() || !content.is_char_boundary(offset_bytes) {
        return Err(
            "offset_bytes must be a valid UTF-8 byte boundary within the result".to_owned(),
        );
    }

    let remaining = &content[offset_bytes..];
    let mut value = truncated_output_value(remaining, max_bytes);
    let output_len = value
        .get("output")
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or(0);
    let next_offset = offset_bytes + output_len;

    if offset_bytes > 0 || next_offset < content.len() {
        let object = value
            .as_object_mut()
            .expect("truncated_output_value always returns an object");
        object.insert("offset_bytes".to_string(), json!(offset_bytes));
        object.insert("total_bytes".to_string(), json!(content.len()));
    }

    if next_offset < content.len() {
        follow_up_arguments.insert("offset_bytes".to_string(), json!(next_offset));
        let object = value
            .as_object_mut()
            .expect("truncated_output_value always returns an object");
        object.insert("truncated".to_string(), json!(true));
        object.insert("next_offset_bytes".to_string(), json!(next_offset));
        object.insert(
            "follow_up".to_string(),
            json!({
                "tool": follow_up_tool,
                "arguments": follow_up_arguments,
            }),
        );
    }

    Ok(value)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Normalises a path by resolving `.` and `..` components without touching
/// the filesystem. This allows safe path checks on paths that don't exist yet.
fn normalise_path(path: &Path) -> PathBuf {
    let mut stack: Vec<Component> = Vec::new();

    for component in path.components() {
        match component {
            // Skip `.` — it contributes nothing.
            Component::CurDir => {}
            // `..` pops the last component unless we're at the root.
            Component::ParentDir => {
                // Only pop non-root, non-prefix components.
                match stack.last() {
                    Some(Component::RootDir) | Some(Component::Prefix(_)) | None => {
                        // Cannot go above root or prefix — keep the `..` to
                        // preserve semantics (the starts_with check will catch it).
                        stack.push(component);
                    }
                    Some(Component::ParentDir) => {
                        // Already accumulated `..` — stack another.
                        stack.push(component);
                    }
                    _ => {
                        stack.pop();
                    }
                }
            }
            c => stack.push(c),
        }
    }

    stack.iter().collect()
}

fn context_roots(ctx: &dyn ToolContext) -> Result<Vec<PathBuf>, String> {
    let mut roots = vec![canonical_existing_root(ctx.working_directory())?];
    for path in ctx.allowed_paths() {
        let root = canonical_existing_root(path)?;
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    Ok(roots)
}

fn canonical_existing_root(path: &Path) -> Result<PathBuf, String> {
    let root = path
        .canonicalize()
        .map_err(|error| format!("Workspace root is unavailable: {}: {error}", path.display()))?;
    if !root.is_dir() {
        return Err(format!(
            "Workspace root is not a directory: {}",
            path.display()
        ));
    }
    Ok(root)
}

fn sanitise_path_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if segment.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        segment
    }
}

fn approval_root_for_request(ctx: &dyn ToolContext, requested: &str) -> Option<String> {
    let raw = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        ctx.working_directory().join(requested)
    };
    let normalised = normalise_path(&raw);
    if normalised.is_dir() {
        normalised.canonicalize().ok()
    } else {
        normalised
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
    }
    .map(|path| path.to_string_lossy().into_owned())
}

fn resolve_existing_prefix(path: &Path, requested: &str) -> Result<PathBuf, String> {
    if path.exists() {
        return path.canonicalize().map_err(|e| e.to_string());
    }

    let mut existing = path;
    let mut missing_components = Vec::new();
    while !existing.exists() {
        let file_name = existing
            .file_name()
            .ok_or_else(|| format!("Path is outside the workspace: {requested}"))?;
        missing_components.push(file_name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| format!("Path is outside the workspace: {requested}"))?;
    }

    let mut resolved = existing.canonicalize().map_err(|e| e.to_string())?;
    for component in missing_components.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_workspace() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("create temp dir")
    }

    // --- validate_path ---

    #[test]
    fn validate_path_accepts_simple_relative() {
        let dir = tmp_workspace();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"").unwrap();
        let result = validate_path(dir.path(), "file.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file.canonicalize().unwrap());
    }

    #[test]
    fn validate_path_accepts_dot_slash_prefix() {
        let dir = tmp_workspace();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"").unwrap();
        let result = validate_path(dir.path(), "./hello.txt");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_path_accepts_subdirectory() {
        let dir = tmp_workspace();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        let file = sub.join("file.txt");
        std::fs::write(&file, b"").unwrap();
        let result = validate_path(dir.path(), "subdir/file.txt");
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn validate_new_path_rejects_symlink_parent_escape() {
        use std::os::unix::fs::symlink;

        let dir = tmp_workspace();
        let outside = tmp_workspace();
        symlink(outside.path(), dir.path().join("link")).unwrap();

        let result = validate_new_path(dir.path(), "link/new.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside"));
    }

    #[cfg(unix)]
    #[test]
    fn validate_path_rejects_missing_file_beneath_symlink_parent_escape() {
        use std::os::unix::fs::symlink;

        let dir = tmp_workspace();
        let outside = tmp_workspace();
        symlink(outside.path(), dir.path().join("link")).unwrap();

        let result = validate_path(dir.path(), "link/new.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside"));
    }

    #[test]
    fn validate_path_rejects_dotdot_escape() {
        let dir = tmp_workspace();
        let result = validate_path(dir.path(), "../../../etc/passwd");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("outside"), "expected 'outside' in: {msg}");
    }

    #[test]
    fn validate_path_rejects_absolute_outside_workspace() {
        let dir = tmp_workspace();
        let result = validate_path(dir.path(), "/etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn validate_path_accepts_dot_workspace_root() {
        let dir = tmp_workspace();
        let result = validate_path(dir.path(), ".");
        assert!(result.is_ok());
    }

    // --- is_binary_content ---

    #[test]
    fn is_binary_detects_null_byte() {
        assert!(is_binary_content(b"hello\x00world"));
    }

    #[test]
    fn is_binary_accepts_utf8_text() {
        assert!(!is_binary_content(b"Hello, world!\nAnother line.\n"));
    }

    #[test]
    fn is_binary_accepts_empty() {
        assert!(!is_binary_content(b""));
    }

    #[test]
    fn is_binary_detects_high_non_utf8_ratio() {
        // A byte sequence that is not valid UTF-8 in bulk
        let bytes: Vec<u8> = (128u8..255u8).collect();
        assert!(is_binary_content(&bytes));
    }

    // --- truncate_output ---

    #[test]
    fn truncate_no_op_when_within_limit() {
        let content = "hello world";
        let (out, truncated, total) = truncate_output(content, 1000);
        assert_eq!(out, "hello world");
        assert!(!truncated);
        assert_eq!(total, content.len());
    }

    #[test]
    fn truncate_cuts_at_limit() {
        let content = "abcdefghij"; // 10 bytes
        let (out, truncated, total) = truncate_output(content, 5);
        assert_eq!(out, "abcde");
        assert!(truncated);
        assert_eq!(total, 10);
    }

    #[test]
    fn truncate_respects_utf8_boundary() {
        // "é" is 2 bytes in UTF-8 (0xC3 0xA9). With limit=3 we should not
        // split in the middle of the second char.
        let content = "aéb"; // a(1) + é(2) + b(1) = 4 bytes
        let (out, truncated, total) = truncate_output(content, 3);
        // Must be valid UTF-8 — either "a" (1 byte) or "aé" (3 bytes).
        assert!(out == "a" || out == "aé", "got: {out:?}");
        assert!(truncated);
        assert_eq!(total, 4);
    }

    #[test]
    fn truncate_returns_total_bytes() {
        let content = "hello";
        let (_, _, total) = truncate_output(content, 2);
        assert_eq!(total, 5);
    }

    #[test]
    fn continuable_output_rejects_zero_limit() {
        let result = continuable_output_value("abcdef", 0, 0, "file_read", serde_json::Map::new());

        assert_eq!(result.unwrap_err(), "max_output_bytes must be positive");
    }

    // --- resolve_workspace_root ---

    #[test]
    fn resolve_workspace_root_uses_config_path() {
        use swage_config::ResolvedConfig;
        let dir = tmp_workspace();
        let mut cfg = ResolvedConfig::default();
        cfg.workspace.path = dir.path().to_string_lossy().into_owned();
        let root = resolve_workspace_root(&cfg);
        assert!(
            root.starts_with(dir.path().canonicalize().unwrap())
                || root == dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_workspace_root_falls_back_to_home() {
        use swage_config::ResolvedConfig;
        let mut cfg = ResolvedConfig::default();
        // Default workspace path is "." — canonicalises to cwd; just verify
        // it returns a valid absolute path and doesn't panic.
        cfg.workspace.path = ".".to_string();
        let root = resolve_workspace_root(&cfg);
        assert!(root.is_absolute());
    }
}

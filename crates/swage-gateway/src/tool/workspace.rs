//! Workspace path resolution and scoping for file and shell tools.
//!
//! All file operations and shell commands are scoped to the bench's configured
//! workspace directory. This module provides the shared logic for resolving the
//! workspace root and validating that paths don't escape it.

use std::path::{Component, Path, PathBuf};
use swage_config::ResolvedConfig;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the canonical workspace root from config.
///
/// Uses `config.workspace.path` if set and non-empty. Canonicalises the
/// result so that symlinks cannot be used to bypass scoping checks later.
/// Falls back to the current working directory if canonicalisation fails.
pub fn resolve_workspace_root(config: &ResolvedConfig) -> PathBuf {
    let raw = PathBuf::from(&config.workspace.path);
    // Canonicalize resolves symlinks and makes the path absolute. If it fails
    // (e.g. the configured path doesn't exist), fall back to current dir.
    raw.canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
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

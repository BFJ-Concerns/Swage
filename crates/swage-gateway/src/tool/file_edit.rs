//! File edit tool - applies guarded surgical edits to text files.
//!
//! Edits are validated against the current file contents before any writes are
//! made. Absolute paths are supported through the same location-approval flow as
//! the other file tools; relative paths resolve from the current workspace.

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::{
    FileContent, location_approval_error, read_file_content, require_workspace_write_access,
    resolve_context_path,
};

// ---------------------------------------------------------------------------
// Public tool
// ---------------------------------------------------------------------------

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Edit one or more text files by replacing exact current text, replacing Markdown sections, or appending to Markdown sections. Use file_read first for current text. Relative paths resolve from the workspace; absolute paths can target other approved machine locations. The tool rejects stale, ambiguous, or unsafe edits before writing."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "changes": {
                    "type": "array",
                    "description": "Files to edit. Each file is read once, its edits are applied in order, and all files are validated before any writes occur.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "File path. Use ./path for workspace-relative paths, or an absolute path for another approved machine location."
                            },
                            "edits": {
                                "type": "array",
                                "description": "Ordered edits for this file.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "type": {
                                            "type": "string",
                                            "enum": ["replace", "replace_section", "append_to_section"],
                                            "description": "replace changes exact old_text. replace_section changes a Markdown section by heading. append_to_section appends to a Markdown section by heading."
                                        },
                                        "old_text": {
                                            "type": "string",
                                            "description": "Current text copied from file_read. Required for every edit. For section edits, pass the complete current section text: either the heading plus body or the complete body under the heading."
                                        },
                                        "new_text": {
                                            "type": "string",
                                            "description": "Replacement or appended text. Can be empty for deletion-style replacements."
                                        },
                                        "heading": {
                                            "type": "string",
                                            "description": "Required for replace_section and append_to_section. Exact visible Markdown heading text without leading # characters."
                                        },
                                        "replace_all": {
                                            "type": "boolean",
                                            "description": "Only valid for type=replace. When true, every old_text occurrence is replaced; otherwise old_text must match exactly once."
                                        }
                                    },
                                    "required": ["type", "old_text", "new_text"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["path", "edits"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["changes"],
            "additionalProperties": false
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        validate_object_keys(&params, &["changes"], self.name())?;
        let changes = parse_changes(&params, ctx, self.name())?;
        let mut prepared = Vec::with_capacity(changes.len());

        for change in &changes {
            require_workspace_write_access(ctx, &change.resolved_path, self.name()).await?;
            let bytes = match read_file_content(&change.resolved_path)
                .await
                .map_err(|error| read_error(self.name(), &change.requested_path, error))?
            {
                FileContent::Text(bytes) => bytes,
                FileContent::Binary { len } => {
                    return Err(ToolError::ExecutionFailed {
                        name: self.name().to_string(),
                        message: format!(
                            "File appears to be binary ({len} bytes): {}",
                            change.requested_path
                        ),
                    });
                }
            };
            let original = String::from_utf8(bytes).map_err(|_| ToolError::ExecutionFailed {
                name: self.name().to_string(),
                message: format!("File is not valid UTF-8: {}", change.requested_path),
            })?;

            let mut content = original.clone();
            let mut replacements = 0usize;
            for edit in &change.edits {
                let applied = apply_edit(&content, edit, self.name())?;
                replacements += applied.replacements;
                content = applied.content;
            }

            prepared.push(PreparedFileEdit {
                requested_path: change.requested_path.clone(),
                resolved_path: change.resolved_path.clone(),
                content,
                edit_count: change.edits.len(),
                replacement_count: replacements,
                original_byte_count: original.len(),
            });
        }

        for file in &prepared {
            tokio::fs::write(&file.resolved_path, file.content.as_bytes())
                .await
                .map_err(|error| write_error(self.name(), &file.requested_path, error))?;
        }

        let files = prepared
            .iter()
            .map(|file| {
                json!({
                    "path": file.requested_path,
                    "edits": file.edit_count,
                    "replacements": file.replacement_count,
                    "old_bytes": file.original_byte_count,
                    "new_bytes": file.content.len(),
                })
            })
            .collect::<Vec<_>>();
        let file_count = files.len();
        let edit_count = prepared.iter().map(|file| file.edit_count).sum::<usize>();

        Ok(json!({
            "output": format!("Applied {edit_count} edit(s) to {file_count} file(s)"),
            "files": files,
        }))
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FileChange {
    requested_path: String,
    resolved_path: PathBuf,
    edits: Vec<FileEditOperation>,
}

#[derive(Debug)]
enum FileEditOperation {
    Replace {
        old_text: String,
        new_text: String,
        replace_all: bool,
    },
    ReplaceSection {
        heading: String,
        old_text: String,
        new_text: String,
    },
    AppendToSection {
        heading: String,
        old_text: String,
        new_text: String,
    },
}

#[derive(Debug)]
struct PreparedFileEdit {
    requested_path: String,
    resolved_path: PathBuf,
    content: String,
    edit_count: usize,
    replacement_count: usize,
    original_byte_count: usize,
}

struct AppliedEdit {
    content: String,
    replacements: usize,
}

/// Parse the target identically for execution and gateway attachment preflight.
pub(super) fn parse_change_path<'a>(
    change: &'a Value,
    tool_name: &str,
) -> Result<&'a str, ToolError> {
    change
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| invalid(tool_name, "change.path must be a non-empty string"))
}

fn parse_changes(
    params: &Value,
    ctx: &dyn ToolContext,
    tool_name: &str,
) -> Result<Vec<FileChange>, ToolError> {
    let changes = params
        .get("changes")
        .and_then(Value::as_array)
        .filter(|changes| !changes.is_empty())
        .ok_or_else(|| invalid(tool_name, "changes must be a non-empty array"))?;
    let mut seen_paths = HashSet::new();
    let mut parsed = Vec::with_capacity(changes.len());

    for change in changes {
        validate_object_keys(change, &["path", "edits"], tool_name)?;
        let requested_path = parse_change_path(change, tool_name)?.to_owned();
        let resolved = resolve_context_path(ctx, &requested_path)
            .map_err(|_| location_approval_error(tool_name, ctx, &requested_path, "write"))?;
        if !seen_paths.insert(resolved.path.clone()) {
            return Err(invalid(
                tool_name,
                format!(
                    "path '{}' appears more than once; combine edits for a file into one edits array",
                    requested_path
                ),
            ));
        }
        let edits = change
            .get("edits")
            .and_then(Value::as_array)
            .filter(|edits| !edits.is_empty())
            .ok_or_else(|| invalid(tool_name, "change.edits must be a non-empty array"))?
            .iter()
            .map(|edit| parse_edit_operation(edit, tool_name))
            .collect::<Result<Vec<_>, _>>()?;

        parsed.push(FileChange {
            requested_path,
            resolved_path: resolved.path,
            edits,
        });
    }

    Ok(parsed)
}

fn parse_edit_operation(edit: &Value, tool_name: &str) -> Result<FileEditOperation, ToolError> {
    validate_object_keys(
        edit,
        &["type", "old_text", "new_text", "heading", "replace_all"],
        tool_name,
    )?;
    let edit_type = edit.get("type").and_then(Value::as_str).ok_or_else(|| {
        invalid(
            tool_name,
            "edit.type must be replace, replace_section, or append_to_section",
        )
    })?;
    let old_text = edit
        .get("old_text")
        .and_then(Value::as_str)
        .filter(|old_text| !old_text.trim().is_empty())
        .ok_or_else(|| invalid(tool_name, "edit.old_text must be a non-empty string"))?
        .to_string();
    let new_text = edit
        .get("new_text")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(tool_name, "edit.new_text must be a string"))?
        .to_string();

    match edit_type {
        "replace" => {
            let replace_all = edit
                .get("replace_all")
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        invalid(tool_name, "edit.replace_all must be a boolean when present")
                    })
                })
                .transpose()?
                .unwrap_or(false);
            if edit.get("heading").is_some() {
                return Err(invalid(
                    tool_name,
                    "edit.heading is only valid for section edits",
                ));
            }
            Ok(FileEditOperation::Replace {
                old_text,
                new_text,
                replace_all,
            })
        }
        "replace_section" => {
            reject_replace_all_for_section(edit, tool_name)?;
            Ok(FileEditOperation::ReplaceSection {
                heading: parse_required_heading(edit, tool_name)?,
                old_text,
                new_text,
            })
        }
        "append_to_section" => {
            reject_replace_all_for_section(edit, tool_name)?;
            Ok(FileEditOperation::AppendToSection {
                heading: parse_required_heading(edit, tool_name)?,
                old_text,
                new_text,
            })
        }
        other => Err(invalid(
            tool_name,
            format!(
                "edit.type must be replace, replace_section, or append_to_section, got '{other}'"
            ),
        )),
    }
}

fn parse_required_heading(edit: &Value, tool_name: &str) -> Result<String, ToolError> {
    edit.get("heading")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|heading| !heading.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid(tool_name, "edit.heading must be a non-empty heading"))
}

fn reject_replace_all_for_section(edit: &Value, tool_name: &str) -> Result<(), ToolError> {
    if edit.get("replace_all").is_some() {
        return Err(invalid(
            tool_name,
            "edit.replace_all is only valid for type=replace",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

fn apply_edit(
    content: &str,
    edit: &FileEditOperation,
    tool_name: &str,
) -> Result<AppliedEdit, ToolError> {
    match edit {
        FileEditOperation::Replace {
            old_text,
            new_text,
            replace_all,
        } => replace_text(content, old_text, new_text, *replace_all, tool_name),
        FileEditOperation::ReplaceSection {
            heading,
            old_text,
            new_text,
        } => replace_section(content, heading, old_text, new_text, tool_name),
        FileEditOperation::AppendToSection {
            heading,
            old_text,
            new_text,
        } => append_to_section(content, heading, old_text, new_text, tool_name),
    }
}

fn replace_text(
    content: &str,
    old_text: &str,
    new_text: &str,
    replace_all: bool,
    tool_name: &str,
) -> Result<AppliedEdit, ToolError> {
    let match_count = content.matches(old_text).count();
    if match_count == 0 {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: "edit.old_text did not match the current file. Use file_read, then retry with current text.".to_string(),
        });
    }
    if !replace_all && match_count > 1 {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "edit.old_text matched {match_count} places. Use a larger unique old_text or set replace_all=true."
            ),
        });
    }
    let content = if replace_all {
        content.replace(old_text, new_text)
    } else {
        content.replacen(old_text, new_text, 1)
    };
    Ok(AppliedEdit {
        content,
        replacements: if replace_all { match_count } else { 1 },
    })
}

fn replace_section(
    content: &str,
    heading: &str,
    old_text: &str,
    new_text: &str,
    tool_name: &str,
) -> Result<AppliedEdit, ToolError> {
    let (start, end) = find_unique_section_range(content, heading, tool_name)?;
    let current_section = &content[start..end];
    ensure_current_section_matches_old_text(current_section, heading, old_text, tool_name)?;
    let replacement = normalise_section_replacement_for_heading(current_section, heading, new_text);
    Ok(AppliedEdit {
        content: join_section_edit(&content[..start], &replacement, &content[end..]),
        replacements: 1,
    })
}

fn append_to_section(
    content: &str,
    heading: &str,
    old_text: &str,
    new_text: &str,
    tool_name: &str,
) -> Result<AppliedEdit, ToolError> {
    let (start, end) = find_unique_section_range(content, heading, tool_name)?;
    ensure_current_section_matches_old_text(&content[start..end], heading, old_text, tool_name)?;
    let insertion = format!("\n\n{}", normalise_section_text(new_text));
    let edited_section = format!("{}{}", content[..end].trim_end(), insertion);
    Ok(AppliedEdit {
        content: join_section_edit("", &edited_section, &content[end..]),
        replacements: 1,
    })
}

fn ensure_current_section_matches_old_text(
    current_section: &str,
    heading: &str,
    old_text: &str,
    tool_name: &str,
) -> Result<(), ToolError> {
    let supplied = old_text.trim();
    let full_section = current_section.trim();
    let section_body = current_section
        .split_once('\n')
        .map_or("", |(_, body)| body)
        .trim();

    if supplied == full_section || supplied == section_body {
        return Ok(());
    }

    Err(ToolError::ExecutionFailed {
        name: tool_name.to_string(),
        message: format!(
            "edit.old_text did not match the current section '{heading}'. Use file_read, then retry with the complete current section text."
        ),
    })
}

fn find_unique_section_range(
    content: &str,
    heading: &str,
    tool_name: &str,
) -> Result<(usize, usize), ToolError> {
    let headings = markdown_heading_ranges(content);
    let matches = headings
        .iter()
        .enumerate()
        .filter(|(_, item)| item.heading == heading)
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "heading '{heading}' was not found. Use file_read to inspect current headings."
            ),
        });
    }
    if matches.len() > 1 {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "heading '{heading}' matched multiple sections. Use a unique heading before editing."
            ),
        });
    }

    let (index, matched) = matches[0];
    let end = headings[index + 1..]
        .iter()
        .find(|candidate| candidate.level <= matched.level)
        .map_or(content.len(), |candidate| candidate.start);
    Ok((matched.start, end))
}

#[derive(Debug)]
struct MarkdownHeadingRange {
    level: usize,
    heading: String,
    start: usize,
}

fn markdown_heading_ranges(content: &str) -> Vec<MarkdownHeadingRange> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|ch| *ch == '#').count();
        if (1..=6).contains(&level) && trimmed[level..].starts_with(' ') {
            ranges.push(MarkdownHeadingRange {
                level,
                heading: trimmed[level..].trim().to_string(),
                start: offset,
            });
        }
        offset += line.len();
    }
    ranges
}

fn normalise_section_replacement_for_heading(
    current_section: &str,
    heading: &str,
    new_text: &str,
) -> String {
    let replacement = normalise_section_text(new_text);
    if replacement_starts_with_heading(&replacement, heading) {
        replacement
    } else {
        let heading_line = current_section
            .lines()
            .next()
            .unwrap_or_default()
            .trim_end();
        if replacement.is_empty() {
            heading_line.to_string()
        } else {
            format!("{heading_line}\n\n{replacement}")
        }
    }
}

fn replacement_starts_with_heading(markdown: &str, heading: &str) -> bool {
    let Some(first_line) = markdown.lines().next() else {
        return false;
    };
    let trimmed = first_line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    (1..=6).contains(&level)
        && trimmed[level..].starts_with(' ')
        && trimmed[level..].trim() == heading
}

fn normalise_section_text(text: &str) -> String {
    text.trim().to_string()
}

fn join_section_edit(prefix: &str, edited_section: &str, tail: &str) -> String {
    let edited_section = edited_section.trim_end();
    if tail.is_empty() {
        format!("{prefix}{edited_section}")
    } else {
        format!("{prefix}{edited_section}\n\n{tail}")
    }
}

// ---------------------------------------------------------------------------
// Errors and validation
// ---------------------------------------------------------------------------

fn validate_object_keys(value: &Value, allowed: &[&str], tool_name: &str) -> Result<(), ToolError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid(tool_name, "expected an object"))?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(tool_name, format!("unknown field '{key}'")));
        }
    }
    Ok(())
}

fn invalid(tool_name: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: tool_name.to_string(),
        message: message.into(),
    }
}

fn read_error(tool_name: &str, requested_path: &str, error: std::io::Error) -> ToolError {
    let message = match error.kind() {
        std::io::ErrorKind::NotFound => format!("File not found: {requested_path}"),
        std::io::ErrorKind::PermissionDenied => format!("Permission denied: {requested_path}"),
        _ => format!("Failed to read {requested_path}: {error}"),
    };
    ToolError::ExecutionFailed {
        name: tool_name.to_string(),
        message,
    }
}

fn write_error(tool_name: &str, requested_path: &str, error: std::io::Error) -> ToolError {
    let message = match error.kind() {
        std::io::ErrorKind::PermissionDenied => format!("Permission denied: {requested_path}"),
        _ => format!("Failed to write {requested_path}: {error}"),
    };
    ToolError::ExecutionFailed {
        name: tool_name.to_string(),
        message,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tool() -> FileEditTool {
        FileEditTool
    }

    fn ctx(dir: &TempDir) -> crate::tool_executor::GatewayToolContext {
        crate::tool::workspace::writable_test_context(dir.path(), vec![])
    }

    fn edit(path: &str, edits: Vec<Value>) -> Value {
        json!({
            "changes": [
                {
                    "path": path,
                    "edits": edits,
                }
            ]
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replaces_unique_text() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("note.txt"), "alpha beta gamma").unwrap();

        let val = tool()
            .execute(
                edit(
                    "note.txt",
                    vec![json!({
                        "type": "replace",
                        "old_text": "beta",
                        "new_text": "delta"
                    })],
                ),
                &ctx(&dir),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "alpha delta gamma"
        );
        assert_eq!(val["files"][0]["replacements"], json!(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_all_replaces_every_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("note.txt"), "one fish, one dish").unwrap();

        let val = tool()
            .execute(
                edit(
                    "note.txt",
                    vec![json!({
                        "type": "replace",
                        "old_text": "one",
                        "new_text": "two",
                        "replace_all": true
                    })],
                ),
                &ctx(&dir),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "two fish, two dish"
        );
        assert_eq!(val["files"][0]["replacements"], json!(2));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_rejects_ambiguous_text_without_replace_all() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("note.txt"), "one fish, one dish").unwrap();

        let err = tool()
            .execute(
                edit(
                    "note.txt",
                    vec![json!({
                        "type": "replace",
                        "old_text": "one",
                        "new_text": "two"
                    })],
                ),
                &ctx(&dir),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("matched 2 places"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "one fish, one dish"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_section_requires_current_old_text() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("note.md"),
            "# Note\n\n## Plan\n\nOld plan.\n\n## Tail\n\nKeep me.",
        )
        .unwrap();

        let err = tool()
            .execute(
                edit(
                    "note.md",
                    vec![json!({
                        "type": "replace_section",
                        "heading": "Plan",
                        "old_text": "Stale plan.",
                        "new_text": "New plan."
                    })],
                ),
                &ctx(&dir),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("current section"), "got: {err}");
        assert!(
            std::fs::read_to_string(dir.path().join("note.md"))
                .unwrap()
                .contains("Old plan.")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_section_preserves_heading_for_body_only_replacement() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("note.md"),
            "# Note\n\n## Plan\n\nOld plan.\n\n## Tail\n\nKeep me.",
        )
        .unwrap();

        tool()
            .execute(
                edit(
                    "note.md",
                    vec![json!({
                        "type": "replace_section",
                        "heading": "Plan",
                        "old_text": "Old plan.",
                        "new_text": "New plan."
                    })],
                ),
                &ctx(&dir),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "# Note\n\n## Plan\n\nNew plan.\n\n## Tail\n\nKeep me."
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_to_section_requires_current_old_text() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("note.md"), "# Note\n\n## Plan\n\nOld plan.").unwrap();

        tool()
            .execute(
                edit(
                    "note.md",
                    vec![json!({
                        "type": "append_to_section",
                        "heading": "Plan",
                        "old_text": "Old plan.",
                        "new_text": "Second paragraph."
                    })],
                ),
                &ctx(&dir),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.md")).unwrap(),
            "# Note\n\n## Plan\n\nOld plan.\n\nSecond paragraph."
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_second_file_does_not_write_first_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "old b").unwrap();

        let err = tool()
            .execute(
                json!({
                    "changes": [
                        {
                            "path": "a.txt",
                            "edits": [{
                                "type": "replace",
                                "old_text": "old a",
                                "new_text": "new a"
                            }]
                        },
                        {
                            "path": "b.txt",
                            "edits": [{
                                "type": "replace",
                                "old_text": "missing",
                                "new_text": "new b"
                            }]
                        }
                    ]
                }),
                &ctx(&dir),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("did not match"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "old a"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "old b"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn absolute_path_in_allowed_root_can_be_edited() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let path = outside.path().join("note.txt");
        std::fs::write(&path, "outside old").unwrap();

        let context = crate::tool::workspace::writable_test_context(
            workspace.path(),
            vec![outside.path().to_path_buf()],
        );

        tool()
            .execute(
                edit(
                    &path.display().to_string(),
                    vec![json!({
                        "type": "replace",
                        "old_text": "old",
                        "new_text": "new"
                    })],
                ),
                &context,
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "outside new");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn absolute_path_outside_allowed_roots_requests_write_approval() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let path = outside.path().join("note.txt");
        std::fs::write(&path, "outside old").unwrap();

        let err = tool()
            .execute(
                edit(
                    &path.display().to_string(),
                    vec![json!({
                        "type": "replace",
                        "old_text": "old",
                        "new_text": "new"
                    })],
                ),
                &ctx(&workspace),
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
        assert_eq!(std::fs::read_to_string(path).unwrap(), "outside old");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_binary_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("data.bin"), b"\x00\x01\x02").unwrap();

        let err = tool()
            .execute(
                edit(
                    "data.bin",
                    vec![json!({
                        "type": "replace",
                        "old_text": "x",
                        "new_text": "y"
                    })],
                ),
                &ctx(&dir),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("binary"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn returns_error_for_missing_changes() {
        let dir = TempDir::new().unwrap();
        let err = tool().execute(json!({}), &ctx(&dir)).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("changes"), "got: {err}");
    }
}

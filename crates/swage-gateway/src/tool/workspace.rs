//! Workspace path validation and tool-output handling.
//!
//! File and shell tools receive their working directory and permitted paths
//! from attached workspaces or managed turn scratch roots. This module keeps
//! path operations within those runtime-provided boundaries.

use serde_json::{Value, json};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

const TOOL_OUTPUT_ROOT: &str = ".swage/tool-output";

/// How many leading bytes of a file decide whether it is text or binary.
const BINARY_SAMPLE_BYTES: usize = 8192;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToolPath {
    pub root: PathBuf,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputArtifactPath {
    pub absolute: PathBuf,
    pub reported: String,
}

/// Builds a runtime path for a full tool-output artefact.
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
    let artifact = format!("{turn_id}/{call_id}/{filename}");
    let absolute = tool_output_root(ctx).join(&artifact);
    let reported = if ctx.tool_output_directory().is_some() {
        absolute.to_string_lossy().into_owned()
    } else {
        format!("{TOOL_OUTPUT_ROOT}/{artifact}")
    };

    ToolOutputArtifactPath { absolute, reported }
}

/// Creates an artefact at `<tool-output root>/<turn>/<call>/<file>`.
///
/// Output directories and files are opened relative to held directory handles:
/// sandboxed commands can edit this tree, so none of its links confer authority.
pub async fn create_tool_output_artifact(
    ctx: &dyn ToolContext,
    path: &Path,
) -> io::Result<tokio::fs::File> {
    let base = ctx
        .tool_output_directory()
        .unwrap_or_else(|| ctx.working_directory())
        .to_owned();
    let root = tool_output_root(ctx);
    let relative = path
        .strip_prefix(&root)
        .map_err(io::Error::other)?
        .to_owned();
    tokio::task::spawn_blocking(move || create_output_file(&base, &relative))
        .await
        .map_err(io::Error::other)?
        .map(tokio::fs::File::from_std)
}

fn tool_output_root(ctx: &dyn ToolContext) -> PathBuf {
    ctx.tool_output_directory()
        .unwrap_or_else(|| ctx.working_directory())
        .join(TOOL_OUTPUT_ROOT)
}

#[cfg(unix)]
fn create_output_file(base: &Path, relative: &Path) -> io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};

    fn name(component: &std::ffi::OsStr) -> io::Result<CString> {
        CString::new(component.as_bytes()).map_err(io::Error::other)
    }
    fn open_at(
        dir: &std::fs::File,
        name: &std::ffi::CStr,
        flags: i32,
    ) -> io::Result<std::fs::File> {
        // SAFETY: the directory descriptor and NUL-terminated name remain valid
        // throughout the call. A successful open returns a newly owned descriptor.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a successful openat result, owned exclusively here.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
    fn directory_at(dir: &std::fs::File, path: &Path) -> io::Result<std::fs::File> {
        let mut dir = dir.try_clone()?;
        for component in path.components() {
            let Component::Normal(component) = component else {
                return Err(io::Error::other("Invalid tool-output path component"));
            };
            let component = name(component)?;
            // SAFETY: the descriptor and name are valid for this call.
            if unsafe { libc::mkdirat(dir.as_raw_fd(), component.as_ptr(), 0o700) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            dir = open_at(&dir, &component, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        Ok(dir)
    }

    let base = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(base)?;
    let root = directory_at(&base, Path::new(TOOL_OUTPUT_ROOT))?;
    let temporary =
        CString::new(format!(".gitignore-{}", uuid::Uuid::new_v4())).expect("UUID has no NUL");
    let mut ignore = open_at(
        &root,
        &temporary,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?;
    let installed = (|| {
        ignore.write_all(b"*\n")?;
        // SAFETY: both names and the held directory descriptor are valid. Rename
        // replaces the directory entry itself, never following its destination.
        if unsafe {
            libc::renameat(
                root.as_raw_fd(),
                temporary.as_ptr(),
                root.as_raw_fd(),
                c".gitignore".as_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if installed.is_err() {
        // SAFETY: removes only this temporary entry relative to the held root.
        unsafe {
            libc::unlinkat(root.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    installed?;
    let parent = directory_at(
        &root,
        relative
            .parent()
            .ok_or_else(|| io::Error::other("Missing tool-output parent"))?,
    )?;
    let filename = relative
        .file_name()
        .ok_or_else(|| io::Error::other("Missing tool-output filename"))?;
    // Exclusive creation also refuses hard links, special files and existing
    // artefacts without ever opening their targets for writing.
    open_at(
        &parent,
        &name(filename)?,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )
}

#[cfg(not(unix))]
fn create_output_file(_base: &Path, _relative: &Path) -> io::Result<std::fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Tool-output storage requires directory-relative file creation",
    ))
}

pub fn location_approval_error(
    tool_name: &str,
    ctx: &dyn ToolContext,
    requested: &str,
    access: &str,
    directory_target: bool,
) -> ToolError {
    let path = approval_root_for_request(ctx, requested, directory_target)
        .unwrap_or_else(|| PathBuf::from(requested));
    let Some(path) = path.to_str() else {
        return ToolError::ExecutionFailed {
            name: tool_name.to_string(),
            message: format!(
                "Cannot request location approval for a non-UTF-8 path: {}",
                path.display()
            ),
        };
    };
    ToolError::RequiresLocationApproval {
        name: tool_name.to_string(),
        path: path.to_owned(),
        access: access.to_string(),
        timeout_secs: None,
        timeout_policy: None,
    }
}

/// Resolves a tool path against the current turn's workspace roots.
///
/// Relative paths are interpreted from `ctx.working_directory()`. Absolute
/// paths are accepted only when they remain inside the working directory, one
/// of `ctx.allowed_paths()`, or `ctx.tool_output_directory()`. Built-in file tools
/// use this resolver as their runtime permission boundary so location approvals
/// can take effect without
/// rebuilding a sandbox mount set mid-turn.
pub fn resolve_context_path(
    ctx: &dyn ToolContext,
    requested: &str,
) -> Result<ResolvedToolPath, String> {
    let working_directory = canonical_existing_root(ctx.working_directory())?;
    let allowed_roots = context_roots(ctx)?;

    let candidate = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        working_directory.join(requested)
    };
    let mut last_error = None;
    for root in allowed_roots {
        match validate_path(&root, &candidate) {
            Ok(path) => return Ok(ResolvedToolPath { root, path }),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| format!("Path is outside the workspace: {requested}")))
}

/// Resolves a not-yet-created tool path against the same permitted roots as
/// `resolve_context_path`, including the tool-output directory.
pub fn resolve_new_context_path(
    ctx: &dyn ToolContext,
    requested: &str,
) -> Result<ResolvedToolPath, String> {
    let working_directory = canonical_existing_root(ctx.working_directory())?;
    let allowed_roots = context_roots(ctx)?;

    let candidate = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        working_directory.join(requested)
    };
    let mut last_error = None;
    for root in allowed_roots {
        match validate_new_path(&root, &candidate) {
            Ok(path) => return Ok(ResolvedToolPath { root, path }),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| format!("Path is outside the workspace: {requested}")))
}

/// Refuses native file mutations covered by an attachment without write access.
///
/// The gateway checks its attachments before local execution or remote dispatch.
/// Device and nested-tool receivers use a serialised context after their
/// gateway preflight. Devices also enforce their own workspace roots. Other
/// contexts without authority fail closed.
pub async fn require_workspace_write_access(
    ctx: &dyn ToolContext,
    path: &Path,
    tool_name: &str,
) -> Result<(), ToolError> {
    // Both DeviceConnection and NestedToolRuntime preflight gateway attachments
    // before dispatching native tools with this wire context. The type alone
    // carries no attachment authority; those callers own the preceding check.
    if ctx
        .as_any()
        .is::<swage_tool::context::SerializableToolContext>()
    {
        return Ok(());
    }
    let attachments = workspace_attachment_access(ctx, tool_name).await?;
    require_attachment_write_access(&attachments, path, tool_name)
}

async fn workspace_attachment_access(
    ctx: &dyn ToolContext,
    tool_name: &str,
) -> Result<Vec<crate::turn::WorkspaceAttachmentAccess>, ToolError> {
    let Some(gateway) = ctx
        .as_any()
        .downcast_ref::<crate::tool_executor::GatewayToolContext>()
    else {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_owned(),
            message: "Permission denied: cannot establish workspace write access without gateway attachment permissions".into(),
        });
    };
    let failure = |message: String| ToolError::ExecutionFailed {
        name: tool_name.to_owned(),
        message,
    };
    let session_id = uuid::Uuid::parse_str(ctx.session_id())
        .map_err(|error| failure(format!("Cannot check workspace write access: {error}")))?;
    let session = gateway
        .storage()
        .get_session(session_id)
        .await
        .map_err(|error| failure(format!("Cannot check workspace write access: {error}")))?
        .ok_or_else(|| failure("Cannot check workspace write access: session is missing".into()))?;
    let attachments =
        crate::turn::load_workspace_attachment_access(gateway.storage(), session.topic_id)
            .await
            .map_err(|error| failure(format!("Cannot check workspace write access: {error}")))?;
    Ok(attachments)
}

fn require_attachment_write_access(
    attachments: &[crate::turn::WorkspaceAttachmentAccess],
    path: &Path,
    tool_name: &str,
) -> Result<(), ToolError> {
    let failure = |message| ToolError::ExecutionFailed {
        name: tool_name.to_owned(),
        message,
    };
    // A more specific attachment overrides its parent. Equally specific,
    // conflicting declarations retain the read-only restriction.
    let effective = attachments
        .iter()
        .filter(|access| access.grant.is_trusted || !access.grant.can_write)
        .filter_map(|access| {
            let root = access
                .grant
                .path
                .canonicalize()
                .unwrap_or_else(|_| access.grant.path.clone());
            path.starts_with(&root).then_some((root, access))
        })
        .max_by_key(|(root, access)| (root.components().count(), !access.grant.can_write));
    if let Some((_, access)) = effective
        && !access.grant.can_write
    {
        return Err(failure(format!(
            "Permission denied: workspace '{}' has read-only access: {}",
            access.attachment.label,
            path.display()
        )));
    }
    Ok(())
}

/// Checks attachment access before native file mutations leave the gateway.
/// Device paths outside gateway attachments remain governed by the device.
pub(crate) async fn require_native_file_write_access(
    tool_name: &str,
    params: &Value,
    ctx: &dyn ToolContext,
    device_owned: bool,
) -> Result<(), ToolError> {
    let paths: Vec<&str> = match tool_name {
        "file_write" => params["path"].as_str().into_iter().collect(),
        "file_edit" => params["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|change| super::file_edit::parse_change_path(change, tool_name))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Ok(()),
    };
    if paths.is_empty() {
        return Ok(());
    }
    let attachments = workspace_attachment_access(ctx, tool_name).await?;
    for requested in paths {
        let path = Path::new(requested);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            ctx.working_directory().join(path)
        };
        let path = normalise_path(&path);
        if device_owned {
            if !attachments.iter().any(|access| {
                path.starts_with(&access.grant.path)
                    || path.starts_with(Path::new(&access.attachment.path))
            }) {
                // This path belongs to the paired machine. Only its receiver
                // can resolve it; traversing the host filesystem is unrelated.
                continue;
            }
        }
        let path = resolve_existing_prefix(&path, requested).map_err(|message| {
            ToolError::ExecutionFailed {
                name: tool_name.to_owned(),
                message,
            }
        })?;
        require_attachment_write_access(&attachments, &path, tool_name)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn writable_test_context(
    working_directory: &Path,
    allowed_paths: Vec<PathBuf>,
) -> crate::tool_executor::GatewayToolContext {
    use std::sync::Arc;
    use swage_storage::{NewSession, SessionOrigin, SessionStore, SqliteStorage, TopicStore};
    let storage = Arc::new(SqliteStorage::in_memory().expect("test storage"));
    let session = futures::executor::block_on(async {
        let topic = storage.general_scope().await.expect("general scope");
        storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "agent-test".into(),
                origin: SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session")
    });
    let mut base = swage_tool::context::MockToolContext::new("agent-test", session.id.to_string());
    base.working_directory = working_directory.to_path_buf();
    base.allowed_paths = allowed_paths;
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    crate::tool_executor::GatewayToolContext::new(&base, tx, storage)
}

/// Validates that `requested` resolves to a path inside `workspace_root`.
///
/// Returns the resolved absolute path on success, or an error message if
/// the path escapes the workspace (via `..`, absolute paths, or symlinks).
///
/// Works with both existing and not-yet-existing paths — the normalisation
/// step resolves `..` components without requiring the path to be on disk.
/// Symlink targets are checked even when their final path does not exist yet.
pub fn validate_path(
    workspace_root: &Path,
    requested: impl AsRef<Path>,
) -> Result<PathBuf, String> {
    let requested = requested.as_ref();
    let display = requested.display();
    // Absolute paths are only accepted if they fall inside the workspace root.
    let raw = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace_root.join(requested)
    };

    // Manually normalise `.` and `..` without requiring the path to exist.
    let normalised = normalise_path(&raw);

    // The workspace root itself must be canonical so starts_with is reliable.
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    let resolved = resolve_existing_prefix(&normalised, &display.to_string())?;
    if !resolved.starts_with(&canonical_root) {
        return Err(format!("Path is outside the workspace: {display}"));
    }

    Ok(resolved)
}

/// Validates a not-yet-created path without allowing symlink ancestors to
/// redirect the final write outside the workspace.
pub fn validate_new_path(
    workspace_root: &Path,
    requested: impl AsRef<Path>,
) -> Result<PathBuf, String> {
    let requested = requested.as_ref();
    let resolved = validate_path(workspace_root, requested)?;
    if resolved.file_name().is_none() {
        return Err(format!("Path has no file name: {}", requested.display()));
    }
    Ok(resolved)
}

/// A file as the file tools see it: text content, or a binary file recognised
/// from its leading sample.
pub enum FileContent {
    Text(Vec<u8>),
    Binary { len: Option<u64> },
}

/// Reads a file for a file tool. Text is read whole; a binary file is never
/// read past its classification sample, so a multi-gigabyte model or archive
/// costs a few kilobytes rather than its size in memory.
pub async fn read_file_content(path: &Path) -> io::Result<FileContent> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)?;
        match read_text_sample(&mut file)? {
            Some(mut bytes) => {
                file.read_to_end(&mut bytes)?;
                Ok(FileContent::Text(bytes))
            }
            None => Ok(FileContent::Binary {
                len: match file.metadata()?.len() {
                    0 => None,
                    len => Some(len),
                },
            }),
        }
    })
    .await
    .map_err(io::Error::other)?
}

/// A binary classification implies non-empty content. Zero metadata length
/// on virtual files therefore means that the byte length is unknown.
pub fn binary_size_description(len: Option<u64>) -> String {
    len.map(|len| format!("{len} bytes"))
        .unwrap_or_else(|| "unknown byte length".into())
}

/// Reads the classification sample from the start of `source`, returning it
/// when the content is text and `None` when it is binary. Nothing past the
/// sample is read, so the caller decides whether the rest is worth reading.
pub fn read_text_sample(source: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut sample = Vec::with_capacity(BINARY_SAMPLE_BYTES);
    source
        .by_ref()
        .take(BINARY_SAMPLE_BYTES as u64)
        .read_to_end(&mut sample)?;
    Ok((!is_binary_content(&sample)).then_some(sample))
}

/// Heuristic check: returns `true` if `bytes` appears to be binary content.
///
/// Scans the first 8 KiB for null bytes (reliable binary indicator) and
/// checks the ratio of non-UTF-8 bytes. Plain text files may contain a few
/// high bytes, but binary files tend to have many.
fn is_binary_content(bytes: &[u8]) -> bool {
    // Only inspect the first 8 KiB — enough to classify most files quickly.
    let sample = &bytes[..bytes.len().min(BINARY_SAMPLE_BYTES)];

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
/// Small first-surface results keep the historical `{ "output": ... }` shape.
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
    for path in ctx
        .allowed_paths()
        .iter()
        .map(PathBuf::as_path)
        .chain(ctx.tool_output_directory())
    {
        let root = if !path.exists() {
            resolve_existing_prefix(&normalise_path(path), &path.to_string_lossy())?
        } else {
            canonical_existing_root(path)?
        };
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

fn approval_root_for_request(
    ctx: &dyn ToolContext,
    requested: &str,
    directory_target: bool,
) -> Option<PathBuf> {
    let raw = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        canonical_existing_root(ctx.working_directory())
            .ok()?
            .join(requested)
    };
    let normalised = normalise_path(&raw);
    // Missing parents are part of the requested location, not a reason to
    // grant access to a broader existing ancestor.
    let root = if normalised.is_dir() || (directory_target && !normalised.exists()) {
        normalised.as_path()
    } else {
        normalised.parent()?
    };
    resolve_existing_prefix(root, requested).ok()
}

/// Follows every symlink before appending missing path components, including
/// links whose targets do not exist yet. Relative link targets start at the
/// link's parent, and a traversal bound refuses cycles.
fn resolve_existing_prefix(path: &Path, requested: &str) -> Result<PathBuf, String> {
    let mut pending: std::collections::VecDeque<_> = path
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect();
    let mut resolved = PathBuf::new();
    let mut followed_links = 0;
    while let Some(component) = pending.pop_front() {
        match Path::new(&component).components().next() {
            Some(Component::CurDir) => continue,
            Some(Component::ParentDir) => {
                resolved.pop();
                continue;
            }
            _ => resolved.push(&component),
        }
        match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                followed_links += 1;
                if followed_links > 40 {
                    return Err(format!("Too many symlinks in path: {requested}"));
                }
                let target = std::fs::read_link(&resolved).map_err(|error| error.to_string())?;
                resolved.pop();
                if target.is_absolute() {
                    resolved.clear();
                }
                for component in target.components().rev() {
                    pending.push_front(component.as_os_str().to_os_string());
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot resolve path {requested}: {error}")),
        }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn regression_output_writes_do_not_follow_planted_links() {
        use std::os::unix::fs::symlink;
        use tokio::io::AsyncWriteExt;

        for site in [".gitignore", "stdout.txt", "call-directory", "output-root"] {
            let dir = tmp_workspace();
            let outside = tmp_workspace();
            let victim = outside.path().join("victim.txt");
            std::fs::write(&victim, b"keep me").unwrap();
            let ctx = swage_tool::context::MockToolContext::new("agent", "session");
            let mut ctx = ctx;
            ctx.working_directory = dir.path().into();
            let artifact = tool_output_artifact_path(&ctx, "stdout.txt");
            let root = tool_output_root(&ctx);
            std::fs::create_dir_all(artifact.absolute.parent().unwrap()).unwrap();
            match site {
                ".gitignore" => symlink(&victim, root.join(".gitignore")).unwrap(),
                "stdout.txt" => symlink(&victim, &artifact.absolute).unwrap(),
                "call-directory" => {
                    let parent = artifact.absolute.parent().unwrap();
                    std::fs::remove_dir(parent).unwrap();
                    symlink(outside.path(), parent).unwrap();
                }
                "output-root" => {
                    std::fs::remove_dir_all(&root).unwrap();
                    symlink(outside.path(), &root).unwrap();
                }
                _ => unreachable!(),
            }
            if let Ok(mut file) = create_tool_output_artifact(&ctx, &artifact.absolute).await {
                file.write_all(b"output").await.unwrap();
                file.flush().await.unwrap();
            }
            assert_eq!(
                std::fs::read(&victim).unwrap(),
                b"keep me",
                "{site} redirected a gateway write"
            );
            assert_eq!(
                std::fs::read_dir(outside.path()).unwrap().count(),
                1,
                "{site} created output outside its root"
            );
        }
    }

    #[tokio::test]
    async fn tool_output_artifact_is_readable_without_untracked_git_changes() {
        use std::process::Command;
        use tokio::io::AsyncWriteExt;

        let dir = tmp_workspace();
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .output()
            .expect("run git init");
        assert!(init.status.success(), "git init failed: {init:?}");

        let ctx = swage_tool::context::MockToolContext {
            agent_id: "test-agent".into(),
            session_id: "test-session".into(),
            working_directory: dir.path().to_path_buf(),
            allowed_paths: vec![],
            user_info: None,
            call_id: Some("test-call".into()),
            turn_id: None,
        };
        let artifact = tool_output_artifact_path(&ctx, "stdout.txt");
        let mut file = create_tool_output_artifact(&ctx, &artifact.absolute)
            .await
            .expect("create artefact");
        file.write_all(b"full tool output").await.unwrap();
        file.flush().await.unwrap();
        drop(file);

        let resolved =
            resolve_context_path(&ctx, &artifact.reported).expect("resolve reported path");
        assert_eq!(
            tokio::fs::read(resolved.path).await.unwrap(),
            b"full tool output"
        );

        let status = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=normal"])
            .current_dir(dir.path())
            .output()
            .expect("run git status");
        assert!(status.status.success(), "git status failed: {status:?}");
        assert_eq!(
            String::from_utf8(status.stdout).unwrap(),
            "",
            "tool output must not dirty the workspace repository"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_new_path_resolves_dangling_symlink_parent() {
        let dir = tmp_workspace();
        std::os::unix::fs::symlink("missing-directory", dir.path().join("link")).unwrap();
        let resolved = validate_new_path(dir.path(), "link/probe.txt").unwrap();
        assert_eq!(resolved, dir.path().join("missing-directory/probe.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn validate_new_path_rejects_dangling_symlink_cycle() {
        let dir = tmp_workspace();
        std::os::unix::fs::symlink("second-link", dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink("link", dir.path().join("second-link")).unwrap();
        assert!(
            validate_new_path(dir.path(), "link").is_err(),
            "symlink cycle must refuse resolution"
        );
    }

    #[cfg(unix)]
    fn symlinked_working_directory_sibling_grant(write: bool) {
        let files = tmp_workspace();
        let real = files.path().join("real");
        let links = files.path().join("links");
        std::fs::create_dir_all(real.join("project")).unwrap();
        std::fs::create_dir_all(real.join("project-Annexe")).unwrap();
        std::fs::create_dir(&links).unwrap();
        std::os::unix::fs::symlink(real.join("project"), links.join("project")).unwrap();
        std::fs::write(real.join("project-Annexe/source.txt"), "marker").unwrap();
        let mut ctx = swage_tool::context::MockToolContext {
            agent_id: "agent".into(),
            session_id: "session".into(),
            working_directory: links.join("project"),
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        let request = if write {
            "../project-Annexe/new/report.md"
        } else {
            "../project-Annexe/source.txt"
        };
        let error = location_approval_error(
            if write { "file_write" } else { "file_read" },
            &ctx,
            request,
            if write { "write" } else { "read" },
            false,
        );
        let ToolError::RequiresLocationApproval { path, .. } = error else {
            panic!("location approval expected")
        };
        ctx.allowed_paths.push(PathBuf::from(path));
        let resolved = if write {
            resolve_new_context_path(&ctx, request)
        } else {
            resolve_context_path(&ctx, request)
        };
        assert!(
            resolved.is_ok(),
            "the location grant must release the sibling request: {resolved:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_working_directory_sibling_read_is_released_by_its_grant() {
        symlinked_working_directory_sibling_grant(false);
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_working_directory_sibling_write_is_released_by_its_grant() {
        symlinked_working_directory_sibling_grant(true);
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

    #[test]
    fn missing_parent_approval_preserves_the_requested_directory_boundary() {
        let dir = tmp_workspace();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let mut ctx = swage_tool::context::MockToolContext {
            agent_id: "test-agent".into(),
            session_id: "test-session".into(),
            working_directory: project,
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        let requested = "../missing/deep/report.md";
        let root = match location_approval_error("file_write", &ctx, requested, "write", false) {
            ToolError::RequiresLocationApproval { path, .. } => path,
            other => panic!("expected location approval, got {other:?}"),
        };
        assert_eq!(root, dir.path().join("missing/deep").to_string_lossy());
        assert!(!Path::new(&root).exists());
        ctx.allowed_paths.push(PathBuf::from(root));
        assert_eq!(
            resolve_new_context_path(&ctx, requested).unwrap().path,
            dir.path().join("missing/deep/report.md")
        );
        assert!(resolve_new_context_path(&ctx, "../missing/neighbour.md").is_err());
        assert!(resolve_new_context_path(&ctx, "../outside.md").is_err());
    }

    #[test]
    fn approval_root_uses_the_directory_flag_instead_of_the_tool_name() {
        let dir = tmp_workspace();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let ctx = swage_tool::context::MockToolContext {
            agent_id: "test-agent".into(),
            session_id: "test-session".into(),
            working_directory: project,
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        let target = dir.path().join("missing-Annexe");
        for name in ["inspect_tree", "file_list"] {
            for directory_target in [true, false] {
                let error = location_approval_error(
                    name,
                    &ctx,
                    "../missing-Annexe",
                    "read",
                    directory_target,
                );
                let ToolError::RequiresLocationApproval { path, .. } = error else {
                    panic!("expected location approval, got {error:?}");
                };
                let expected = if directory_target {
                    target.as_path()
                } else {
                    dir.path()
                };
                assert_eq!(
                    Path::new(&path),
                    expected,
                    "{name}: directory_target={directory_target}"
                );
            }
        }
        assert!(!target.exists());
    }

    #[test]
    fn context_roots_require_directories_but_allow_missing_grants() {
        let dir = tmp_workspace();
        let tool_root = dir.path().join("not-a-directory");
        std::fs::write(&tool_root, b"").unwrap();
        let tool_context = swage_tool::context::MockToolContext {
            agent_id: "test-agent".into(),
            session_id: "test-session".into(),
            working_directory: tool_root.clone(),
            allowed_paths: vec![],
            user_info: None,
            call_id: None,
            turn_id: None,
        };

        assert_eq!(
            resolve_context_path(&tool_context, "readme.md").unwrap_err(),
            format!("Workspace root is not a directory: {}", tool_root.display())
        );

        let attachment_root = dir.path().join("missing");
        let attachment_context = swage_tool::context::MockToolContext {
            agent_id: "test-agent".into(),
            session_id: "test-session".into(),
            working_directory: dir.path().to_path_buf(),
            allowed_paths: vec![attachment_root.clone()],
            user_info: None,
            call_id: None,
            turn_id: None,
        };
        let resolved = resolve_context_path(&attachment_context, attachment_root.to_str().unwrap());
        assert!(
            resolved.is_ok(),
            "a missing grant must remain usable: {resolved:?}"
        );
        assert_eq!(resolved.unwrap().path, attachment_root);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use swage_tool::context::MockToolContext;
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_non_utf8_root_read_write_and_approval() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(std::ffi::OsStr::from_bytes(b"root-\xff"));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("notes"), "hello").unwrap();
        let mut ctx = MockToolContext::new("agent", "session");
        ctx.working_directory = root.clone();
        assert_eq!(
            resolve_context_path(&ctx, "notes").unwrap().path,
            root.join("notes")
        );
        assert_eq!(
            resolve_new_context_path(&ctx, "new/notes").unwrap().path,
            root.join("new/notes")
        );
        assert_eq!(
            approval_root_for_request(&ctx, "new/notes", false).unwrap(),
            root.join("new")
        );
        let error = location_approval_error("file_write", &ctx, "unapproved/notes", "write", false);
        assert!(
            matches!(error, ToolError::ExecutionFailed { ref message, .. } if message.contains("non-UTF-8")),
            "a display path became approval authority: {error:?}"
        );
        let ctx = writable_test_context(&root, vec![]);
        crate::tool::Tool::execute(
            &crate::tool::file_write::FileWriteTool,
            json!({"path":"new/notes", "content":"lossless"}),
            &ctx,
        )
        .await
        .unwrap();
        let read = crate::tool::Tool::execute(
            &crate::tool::file_read::FileReadTool {
                max_output_bytes: 100,
            },
            json!({"path":"new/notes"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(read["output"], "lossless");
    }
    #[cfg(unix)]
    #[test]
    fn regression_canonical_candidate_accepts_approved_symlink_alias() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let allowed = dir.path().join("allowed");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&allowed).unwrap();
        std::fs::write(allowed.join("notes"), "hello").unwrap();
        std::os::unix::fs::symlink(&allowed, workspace.join("alias")).unwrap();
        let mut ctx = MockToolContext::new("agent", "session");
        ctx.working_directory = workspace;
        ctx.allowed_paths.push(allowed.clone());
        assert_eq!(
            resolve_context_path(&ctx, "alias/notes").unwrap().path,
            allowed.join("notes")
        );
        assert_eq!(
            resolve_new_context_path(&ctx, "alias/new").unwrap().path,
            allowed.join("new")
        );
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_untrusted_child_cannot_release_read_only_parent() {
        use swage_storage::NewWorkspaceAttachment;
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let ctx = writable_test_context(dir.path(), vec![]);
        let session = ctx
            .storage()
            .get_session(uuid::Uuid::parse_str(ctx.session_id()).unwrap())
            .await
            .unwrap()
            .unwrap();
        for (path, trusted, permissions) in [
            (dir.path(), true, vec!["read".into()]),
            (child.as_path(), false, vec!["read".into(), "write".into()]),
        ] {
            ctx.storage()
                .create_workspace_attachment(NewWorkspaceAttachment {
                    topic_id: session.topic_id,
                    label: path.display().to_string(),
                    path: path.display().to_string(),
                    permissions,
                    is_trusted: trusted,
                })
                .await
                .unwrap();
        }
        let result = crate::tool::Tool::execute(
            &crate::tool::file_write::FileWriteTool,
            json!({"path":"child/notes", "content":"forbidden"}),
            &ctx,
        )
        .await;
        assert!(
            result.is_err(),
            "untrusted child released a write: {result:?}"
        );
        assert!(!child.join("notes").exists());
    }
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn regression_device_preflight_does_not_traverse_unattached_host_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("cycle", dir.path().join("cycle")).unwrap();
        let ctx = writable_test_context(dir.path(), vec![]);
        let result = require_native_file_write_access(
            "file_write",
            &json!({"path":"cycle/notes"}),
            &ctx,
            true,
        )
        .await;
        assert!(
            result.is_ok(),
            "host traversal blocked device-owned path: {result:?}"
        );
    }
}

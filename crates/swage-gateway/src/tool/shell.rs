//! Shell execution tool — runs commands in the topic's workspace directory.

use std::io;
use std::os::unix::process::CommandExt;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use super::Tool;
use super::workspace::{
    ToolOutputArtifactPath, create_tool_output_artifact, tool_output_artifact_path,
    truncated_output_value,
};

// ---------------------------------------------------------------------------
// ShellTool
// ---------------------------------------------------------------------------

/// Executes shell commands in the turn workspace.
///
/// Commands run via `sh -c` with CWD set to the active ToolContext directory.
/// Output is capped at `max_output_bytes`; the process group stops on timeout
/// or when execution is cancelled.
pub struct ShellTool {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("command"),
        }
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Extract the command string from arguments.
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(cmd) if !cmd.is_empty() => cmd.to_owned(),
            _ => {
                return Err(ToolError::InvalidParameters {
                    name: self.name().to_owned(),
                    message: "Missing or empty 'command' argument".to_owned(),
                });
            }
        };

        // Spawn the command under sh -c so standard shell features are available.
        let mut process = Command::new("sh");
        process
            .arg("-c")
            .arg(&command)
            .current_dir(ctx.working_directory())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Descendants inherit this group, so dropping the executing future
        // can stop the command's whole process tree rather than just `sh`.
        process.as_std_mut().process_group(0);
        let mut child = process.spawn().map_err(|e| ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message: format!("Failed to spawn command: {e}"),
        })?;
        let mut process_group = ShellProcessGroup::new(&child);

        // Take the piped stdout/stderr handles before starting the wait.
        let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");

        let cap = self.max_output_bytes;
        let tool_name = self.name().to_owned();
        let artifact_paths = shell_output_artifact_paths(ctx);

        // Read stdout and stderr concurrently to prevent pipe-buffer deadlocks.
        // Each stream is bounded at max_output_bytes so a high-throughput command
        // (e.g. `yes`, `dd if=/dev/urandom`) cannot exhaust memory before the
        // timeout fires. The pipes are always fully drained to avoid stalling the
        // child process on a full pipe buffer.
        let read_fut = async move {
            let (stdout_res, stderr_res) = tokio::join!(
                // join! drives both reads concurrently — critical to avoid deadlock.
                read_bounded(ctx, &mut stdout_pipe, cap, artifact_paths.stdout_path),
                read_bounded(ctx, &mut stderr_pipe, cap, artifact_paths.stderr_path),
            );

            // Propagate I/O errors rather than panicking — a panic inside
            // tokio::spawn is silently swallowed, leaving the turn in
            // Processing state forever.
            let stdout = stdout_res.map_err(|e| ToolError::ExecutionFailed {
                name: tool_name.clone(),
                message: format!("Failed to read command stdout: {e}"),
            })?;
            let stderr = stderr_res.map_err(|e| ToolError::ExecutionFailed {
                name: tool_name,
                message: format!("Failed to read command stderr: {e}"),
            })?;

            Ok::<_, ToolError>((stdout, stderr))
        };

        // Safe to abandon on timeout: read_fut owns only local pipe handles and
        // output buffers — no shared or persisted state. The timeout arm below
        // kills the child and discards the partial buffers, so dropping the
        // future leaves nothing half-written.
        let read_result = tokio::time::timeout(self.timeout, read_fut).await;

        match read_result {
            Err(_elapsed) => {
                // Timeout expired — stop the process group to free resources.
                kill_child(&mut child, &mut process_group).await;
                Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!(
                        "Command timed out after {:.1}s (shell_timeout_secs)",
                        self.timeout.as_secs_f64()
                    ),
                })
            }

            Ok(Err(e)) => {
                // I/O error reading output — stop the process group and propagate.
                kill_child(&mut child, &mut process_group).await;
                Err(e)
            }

            Ok(Ok((stdout, stderr))) => {
                // Reap the child to get the exit status.
                let exit_status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to wait for command: {e}"),
                })?;
                process_group.disarm();

                let stdout_str = String::from_utf8_lossy(&stdout.bytes);
                let stderr_str = String::from_utf8_lossy(&stderr.bytes);

                // Append truncation notes so the model knows the stream was cut.
                let stdout_display = if stdout.truncated {
                    format!("{stdout_str}\n[stdout truncated: exceeded {cap} bytes]")
                } else {
                    stdout_str.into_owned()
                };
                let stderr_display = if stderr.truncated {
                    format!("{stderr_str}\n[stderr truncated: exceeded {cap} bytes]")
                } else {
                    stderr_str.into_owned()
                };

                // Combine stdout then stderr (prefixed) so the model sees both.
                let combined = if stderr_display.is_empty() {
                    stdout_display
                } else if stdout_display.is_empty() {
                    format!("stderr: {stderr_display}")
                } else {
                    format!("{stdout_display}stderr: {stderr_display}")
                };

                let mut output = truncated_output_value(&combined, self.max_output_bytes);
                output["exit_code"] = json!(exit_status.code());
                if stdout.truncated {
                    output["stdout_truncated"] = json!(true);
                    output["stdout_total_bytes"] = json!(stdout.total_bytes);
                    if let Some(path) = stdout.artifact_relative_path {
                        output["stdout_artifact_path"] = json!(path);
                        output["stdout_follow_up"] = shell_output_follow_up(&path);
                    }
                }
                if stderr.truncated {
                    output["stderr_truncated"] = json!(true);
                    output["stderr_total_bytes"] = json!(stderr.total_bytes);
                    if let Some(path) = stderr.artifact_relative_path {
                        output["stderr_artifact_path"] = json!(path);
                        output["stderr_follow_up"] = shell_output_follow_up(&path);
                    }
                }
                Ok(output)
            }
        }
    }
}

struct BoundedRead {
    bytes: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
    artifact_relative_path: Option<String>,
}

struct ShellOutputArtifactPaths {
    stdout_path: Option<ToolOutputArtifactPath>,
    stderr_path: Option<ToolOutputArtifactPath>,
}

fn shell_output_artifact_paths(ctx: &dyn ToolContext) -> ShellOutputArtifactPaths {
    ShellOutputArtifactPaths {
        stdout_path: Some(tool_output_artifact_path(ctx, "stdout.txt")),
        stderr_path: Some(tool_output_artifact_path(ctx, "stderr.txt")),
    }
}

fn shell_output_follow_up(path: &str) -> Value {
    json!({
        "tool": "file_read",
        "arguments": {
            "path": path
        }
    })
}

/// Reads from `reader` until EOF, storing at most `cap` bytes.
///
/// Continues draining after the cap so the child process never stalls on a
/// full pipe buffer.
async fn read_bounded<R>(
    ctx: &dyn ToolContext,
    reader: &mut R,
    cap: usize,
    artifact_path: Option<ToolOutputArtifactPath>,
) -> io::Result<BoundedRead>
where
    R: AsyncReadExt + Unpin,
{
    let mut buf = Vec::with_capacity(cap.min(65536));
    let mut tmp = [0u8; 4096];
    let mut total = 0usize;
    let mut artifact_writer = match artifact_path.as_ref() {
        Some(path) => Some(create_tool_output_artifact(ctx, &path.absolute).await?),
        None => None,
    };

    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            break; // EOF
        }
        if let Some(writer) = artifact_writer.as_mut() {
            writer.write_all(&tmp[..n]).await?;
        }
        // Store bytes up to the cap; keep reading (but discarding) beyond it
        // so the child's writes never block on a full pipe buffer.
        if total < cap {
            let to_store = (cap - total).min(n);
            buf.extend_from_slice(&tmp[..to_store]);
        }
        total += n;
    }

    if let Some(writer) = artifact_writer.as_mut() {
        writer.flush().await?;
    }
    let truncated = total > cap;
    if !truncated && let Some(path) = artifact_path.as_ref() {
        let _ = tokio::fs::remove_file(&path.absolute).await;
    }

    Ok(BoundedRead {
        bytes: buf,
        total_bytes: total,
        truncated,
        artifact_relative_path: truncated
            .then(|| artifact_path.map(|path| path.relative))
            .flatten(),
    })
}

/// The shell is its group's leader. A live child keeps its pid reserved until
/// reaped, so signalling that group from Drop cannot target a reused pid.
struct ShellProcessGroup {
    id: libc::pid_t,
    armed: bool,
}

impl ShellProcessGroup {
    fn new(child: &Child) -> Self {
        Self {
            id: child.id().expect("a spawned shell has a process id") as libc::pid_t,
            armed: true,
        }
    }

    fn stop(&mut self) {
        if self.armed {
            // The group id is positive and belongs to the shell we spawned.
            unsafe { libc::kill(-self.id, libc::SIGKILL) };
            self.armed = false;
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ShellProcessGroup {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Kill the command's process group, then reap its shell.
async fn kill_child(child: &mut Child, process_group: &mut ShellProcessGroup) {
    process_group.stop();
    // Reap to avoid zombies.
    let _ = child.wait().await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use swage_tool::context::MockToolContext;

    fn tool(timeout_secs: u64) -> ShellTool {
        ShellTool {
            timeout: Duration::from_secs(timeout_secs),
            max_output_bytes: 1024 * 1024, // 1 MiB default
        }
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
    }

    fn ctx_in_dir_with_ids(dir: &tempfile::TempDir) -> MockToolContext {
        let mut ctx = MockToolContext::new("agent-test", "sess-test");
        ctx.working_directory = dir.path().to_path_buf();
        ctx.turn_id = Some(uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000123").unwrap());
        ctx.call_id = Some("call/with topics".to_string());
        ctx
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn echo_hello_returns_output() {
        let t = tool(5);
        let result = t.execute(json!({"command": "echo hello"}), &ctx()).await;
        let val = result.expect("echo should succeed");
        assert_eq!(val["output"].as_str().unwrap().trim(), "hello");
        assert_eq!(val["exit_code"], json!(0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonzero_exit_returns_exit_code() {
        let t = tool(5);
        // `false` always exits with code 1.
        let result = t.execute(json!({"command": "false"}), &ctx()).await;
        let val = result.expect("non-zero exit should still return tool data");
        assert_eq!(val["output"], json!(""));
        assert_eq!(val["exit_code"], json!(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_shell_execution_stops_its_child_process() {
        let workspace = tempfile::tempdir().expect("workspace");
        let context = ctx_in_dir_with_ids(&workspace);
        let ready = workspace.path().join("ready");
        let child_marker = workspace.path().join("child_finished");
        let command = "sh -c 'sleep 2; touch child_finished' & touch ready; wait";
        let execution = tokio::spawn(async move {
            tool(10)
                .execute(json!({ "command": command }), &context)
                .await
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !ready.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "shell never started"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        execution.abort();
        let _ = execution.await;
        tokio::time::sleep(Duration::from_millis(2300)).await;
        assert!(
            !child_marker.exists(),
            "the shell's child continued after execution was dropped"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stderr_output_included_for_nonzero_exit() {
        let t = tool(5);
        let result = t
            .execute(json!({"command": "echo errout >&2; exit 1"}), &ctx())
            .await;
        let val = result.expect("non-zero exit should still return tool data");
        let output = val["output"].as_str().unwrap();
        assert!(
            output.contains("errout"),
            "stderr should appear in output: {output:?}"
        );
        assert_eq!(val["exit_code"], json!(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_command_returns_invalid_params() {
        let t = tool(5);
        let result = t.execute(json!({}), &ctx()).await;
        assert!(matches!(
            result.unwrap_err(),
            ToolError::InvalidParameters { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_kills_process_and_returns_error() {
        let t = ShellTool {
            // Very short timeout — the sleep is 10s, so this should fire.
            timeout: Duration::from_millis(200),
            max_output_bytes: 1024 * 1024,
        };
        let result = t.execute(json!({"command": "sleep 10"}), &ctx()).await;
        assert!(result.is_err(), "timeout should produce an error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("timed out after 0.2s") && msg.contains("shell_timeout_secs"),
            "error message should mention timeout: {msg:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn large_output_truncated() {
        let t = ShellTool {
            timeout: Duration::from_secs(5),
            // Small cap to trigger truncation.
            max_output_bytes: 10,
        };
        let result = t
            .execute(
                json!({"command": "printf '%0.s1234567890' $(seq 1 100)"}),
                &ctx(),
            )
            .await;
        let val = result.expect("command should succeed");
        let output = val["output"].as_str().unwrap();
        assert_eq!(val["exit_code"], json!(0));
        assert!(
            output.len() <= 10,
            "content should be capped: len={}",
            output.len()
        );
        assert_eq!(val["truncated"], json!(true));
        assert!(val["total_bytes"].as_u64().unwrap() > 10);
        assert_eq!(val["stdout_truncated"], json!(true));
        assert!(val["stdout_total_bytes"].as_u64().unwrap() > 10);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn truncated_stdout_writes_full_artifact_and_follow_up() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = ctx_in_dir_with_ids(&dir);
        let t = ShellTool {
            timeout: Duration::from_secs(5),
            max_output_bytes: 10,
        };

        let val = t
            .execute(
                json!({"command": "printf '%0.s1234567890' $(seq 1 10)"}),
                &ctx,
            )
            .await
            .expect("command should succeed");

        let artifact_path = val["stdout_artifact_path"]
            .as_str()
            .expect("truncated stdout should expose an artifact path");
        assert_eq!(
            val["stdout_follow_up"]["arguments"]["path"],
            json!(artifact_path)
        );
        assert!(
            artifact_path.contains("call_with_topics"),
            "call_id should be path-safe: {artifact_path}"
        );

        let full = std::fs::read_to_string(dir.path().join(artifact_path))
            .expect("stdout artifact should be readable");
        assert_eq!(
            full.len(),
            val["stdout_total_bytes"].as_u64().unwrap() as usize
        );
        assert_eq!(full, "1234567890".repeat(10));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn large_stderr_reports_stream_truncation_metadata() {
        let t = ShellTool {
            timeout: Duration::from_secs(5),
            max_output_bytes: 10,
        };
        let result = t
            .execute(
                json!({"command": "printf '%0.s1234567890' $(seq 1 100) >&2; exit 1"}),
                &ctx(),
            )
            .await;
        let val = result.expect("command should return captured stderr");
        let output = val["output"].as_str().unwrap();
        assert_eq!(val["exit_code"], json!(1));
        assert!(
            output.len() <= 10,
            "content should be capped: len={}",
            output.len()
        );
        assert_eq!(val["truncated"], json!(true));
        assert_eq!(val["stderr_truncated"], json!(true));
        assert!(val["stderr_total_bytes"].as_u64().unwrap() > 10);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn truncated_stderr_writes_full_artifact_and_follow_up() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = ctx_in_dir_with_ids(&dir);
        let t = ShellTool {
            timeout: Duration::from_secs(5),
            max_output_bytes: 10,
        };

        let val = t
            .execute(
                json!({"command": "printf '%0.sERR' $(seq 1 20) >&2; exit 1"}),
                &ctx,
            )
            .await
            .expect("command should return captured stderr");

        let artifact_path = val["stderr_artifact_path"]
            .as_str()
            .expect("truncated stderr should expose an artifact path");
        assert_eq!(
            val["stderr_follow_up"]["arguments"]["path"],
            json!(artifact_path)
        );

        let full = std::fs::read_to_string(dir.path().join(artifact_path))
            .expect("stderr artifact should be readable");
        assert_eq!(
            full.len(),
            val["stderr_total_bytes"].as_u64().unwrap() as usize
        );
        assert_eq!(full, "ERR".repeat(20));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cwd_is_workspace_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut ctx = MockToolContext::new("agent-test", "sess-test");
        ctx.working_directory = dir.path().to_path_buf();
        let t = ShellTool {
            timeout: Duration::from_secs(5),
            max_output_bytes: 1024 * 1024,
        };
        // `pwd` should print the workspace root (canonicalised).
        let result = t.execute(json!({"command": "pwd"}), &ctx).await;
        let val = result.expect("pwd should succeed");
        let canonical = dir.path().canonicalize().unwrap();
        let output = val["output"].as_str().unwrap().trim();
        assert_eq!(val["exit_code"], json!(0));
        assert!(
            output.contains(canonical.to_str().unwrap())
                || canonical.to_str().unwrap().contains(output),
            "pwd output {output:?} should match workspace {canonical:?}"
        );
    }
}

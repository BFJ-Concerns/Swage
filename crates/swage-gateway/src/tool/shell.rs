//! Shell execution tool — runs commands in the bench's workspace directory.

use std::io;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::Tool;
use super::workspace::truncate_output;

// ---------------------------------------------------------------------------
// ShellTool
// ---------------------------------------------------------------------------

/// Executes shell commands in the turn workspace.
///
/// Commands run via `sh -c` with CWD set to the active ToolContext directory.
/// Output is capped at `max_output_bytes` and killed after `timeout`.
pub struct ShellTool {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
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
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(ctx.working_directory())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Failed to spawn command: {e}"),
            })?;

        // Take the piped stdout/stderr handles before starting the wait.
        let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");

        let cap = self.max_output_bytes;
        let tool_name = self.name().to_owned();

        // Read stdout and stderr concurrently to prevent pipe-buffer deadlocks.
        // Each stream is bounded at max_output_bytes so a high-throughput command
        // (e.g. `yes`, `dd if=/dev/urandom`) cannot exhaust memory before the
        // timeout fires. The pipes are always fully drained to avoid stalling the
        // child process on a full pipe buffer.
        let read_fut = async move {
            let (stdout_res, stderr_res) = tokio::join!(
                // join! drives both reads concurrently — critical to avoid deadlock.
                read_bounded(&mut stdout_pipe, cap),
                read_bounded(&mut stderr_pipe, cap),
            );

            // Propagate I/O errors rather than panicking — a panic inside
            // tokio::spawn is silently swallowed, leaving the turn in
            // Processing state forever.
            let (stdout_bytes, stdout_over) =
                stdout_res.map_err(|e| ToolError::ExecutionFailed {
                    name: tool_name.clone(),
                    message: format!("Failed to read command stdout: {e}"),
                })?;
            let (stderr_bytes, stderr_over) =
                stderr_res.map_err(|e| ToolError::ExecutionFailed {
                    name: tool_name,
                    message: format!("Failed to read command stderr: {e}"),
                })?;

            Ok::<_, ToolError>((stdout_bytes, stderr_bytes, stdout_over, stderr_over))
        };

        // Safe to abandon on timeout: read_fut owns only local pipe handles and
        // output buffers — no shared or persisted state. The timeout arm below
        // kills the child and discards the partial buffers, so dropping the
        // future leaves nothing half-written.
        let read_result = tokio::time::timeout(self.timeout, read_fut).await;

        match read_result {
            Err(_elapsed) => {
                // Timeout expired — kill the child to free resources.
                let _ = kill_child(&mut child).await;
                Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Command timed out after {:.1}s", self.timeout.as_secs_f64()),
                })
            }

            Ok(Err(e)) => {
                // I/O error reading output — kill the child and propagate.
                let _ = kill_child(&mut child).await;
                Err(e)
            }

            Ok(Ok((stdout_bytes, stderr_bytes, stdout_over, stderr_over))) => {
                // Reap the child to get the exit status.
                let exit_status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to wait for command: {e}"),
                })?;

                let stdout_str = String::from_utf8_lossy(&stdout_bytes);
                let stderr_str = String::from_utf8_lossy(&stderr_bytes);

                // Append truncation notes so the model knows the stream was cut.
                let stdout_display = if stdout_over {
                    format!("{stdout_str}\n[stdout truncated: exceeded {cap} bytes]")
                } else {
                    stdout_str.into_owned()
                };
                let stderr_display = if stderr_over {
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

                let (content, _truncated, _total_bytes) =
                    truncate_output(&combined, self.max_output_bytes);

                Ok(json!({
                    "output": content,
                    "exit_code": exit_status.code(),
                }))
            }
        }
    }
}

/// Reads from `reader` until EOF, storing at most `cap` bytes.
///
/// Continues draining after the cap so the child process never stalls on a
/// full pipe buffer. Returns `(buffer, was_truncated)`.
async fn read_bounded<R>(reader: &mut R, cap: usize) -> io::Result<(Vec<u8>, bool)>
where
    R: AsyncReadExt + Unpin,
{
    let mut buf = Vec::with_capacity(cap.min(65536));
    let mut tmp = [0u8; 4096];
    let mut total = 0usize;

    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            break; // EOF
        }
        // Store bytes up to the cap; keep reading (but discarding) beyond it
        // so the child's writes never block on a full pipe buffer.
        if total < cap {
            let to_store = (cap - total).min(n);
            buf.extend_from_slice(&tmp[..to_store]);
        }
        total += n;
    }

    Ok((buf, total > cap))
}

/// Kill a child process, ignoring errors (it may have already exited).
async fn kill_child(child: &mut Child) {
    let _ = child.kill().await;
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
            msg.contains("timed out"),
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

//! Shell execution tool — runs commands in the project's workspace directory.

use std::path::PathBuf;
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

/// Executes shell commands in the project workspace.
///
/// Commands run via `sh -c` with CWD set to `workspace_root`.
/// Output is capped at `max_output_bytes` and killed after `timeout`.
pub struct ShellTool {
    pub workspace_root: PathBuf,
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

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
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
            .current_dir(&self.workspace_root)
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

        // Read stdout and stderr concurrently to prevent pipe-buffer deadlocks.
        let read_fut = async {
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();
            // join! drives both reads concurrently — critical to avoid deadlock.
            let (stdout_res, stderr_res) = tokio::join!(
                stdout_pipe.read_to_end(&mut stdout_buf),
                stderr_pipe.read_to_end(&mut stderr_buf)
            );
            stdout_res.expect("failed to read child stdout");
            stderr_res.expect("failed to read child stderr");
            (stdout_buf, stderr_buf)
        };

        let read_result = tokio::time::timeout(self.timeout, read_fut).await;

        match read_result {
            Err(_elapsed) => {
                // Timeout expired — kill the child to free resources.
                let _ = kill_child(&mut child).await;
                Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!(
                        "Command timed out after {:.1}s",
                        self.timeout.as_secs_f64()
                    ),
                })
            }

            Ok((stdout_bytes, stderr_bytes)) => {
                // Reap the child to get the exit status.
                let exit_status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to wait for command: {e}"),
                })?;

                let stdout_str = String::from_utf8_lossy(&stdout_bytes);
                let stderr_str = String::from_utf8_lossy(&stderr_bytes);

                // Combine stdout then stderr (prefixed) so the model sees both.
                let combined = if stderr_str.is_empty() {
                    stdout_str.into_owned()
                } else if stdout_str.is_empty() {
                    format!("stderr: {stderr_str}")
                } else {
                    format!("{stdout_str}stderr: {stderr_str}")
                };

                let (content, _truncated, _total_bytes) =
                    truncate_output(&combined, self.max_output_bytes);

                if !exit_status.success() {
                    return Err(ToolError::ExecutionFailed {
                        name: self.name().to_owned(),
                        message: content,
                    });
                }

                Ok(json!({"output": content}))
            }
        }
    }
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
            workspace_root: std::env::temp_dir(),
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
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonzero_exit_returns_err() {
        let t = tool(5);
        // `false` always exits with code 1.
        let result = t.execute(json!({"command": "false"}), &ctx()).await;
        assert!(result.is_err(), "non-zero exit should produce an error");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stderr_output_included_in_error() {
        let t = tool(5);
        let result = t
            .execute(json!({"command": "echo errout >&2; exit 1"}), &ctx())
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("errout"),
            "stderr should appear in error message: {:?}",
            msg
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_command_returns_invalid_params() {
        let t = tool(5);
        let result = t.execute(json!({}), &ctx()).await;
        assert!(matches!(result.unwrap_err(), ToolError::InvalidParameters { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_kills_process_and_returns_error() {
        let t = ShellTool {
            workspace_root: std::env::temp_dir(),
            // Very short timeout — the sleep is 10s, so this should fire.
            timeout: Duration::from_millis(200),
            max_output_bytes: 1024 * 1024,
        };
        let result = t.execute(json!({"command": "sleep 10"}), &ctx()).await;
        assert!(result.is_err(), "timeout should produce an error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("timed out"),
            "error message should mention timeout: {:?}",
            msg
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn large_output_truncated() {
        let t = ShellTool {
            workspace_root: std::env::temp_dir(),
            timeout: Duration::from_secs(5),
            // Small cap to trigger truncation.
            max_output_bytes: 10,
        };
        let result = t
            .execute(json!({"command": "printf '%0.s1234567890' $(seq 1 100)"}), &ctx())
            .await;
        let val = result.expect("command should succeed");
        let output = val["output"].as_str().unwrap();
        assert!(output.len() <= 10, "content should be capped: len={}", output.len());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cwd_is_workspace_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut ctx = MockToolContext::new("agent-test", "sess-test");
        ctx.working_directory = dir.path().to_path_buf();
        let t = ShellTool {
            workspace_root: dir.path().to_path_buf(),
            timeout: Duration::from_secs(5),
            max_output_bytes: 1024 * 1024,
        };
        // `pwd` should print the workspace root (canonicalised).
        let result = t.execute(json!({"command": "pwd"}), &ctx).await;
        let val = result.expect("pwd should succeed");
        let canonical = dir.path().canonicalize().unwrap();
        let output = val["output"].as_str().unwrap().trim();
        assert!(
            output.contains(canonical.to_str().unwrap())
                || canonical.to_str().unwrap().contains(output),
            "pwd output {:?} should match workspace {:?}",
            output,
            canonical
        );
    }
}

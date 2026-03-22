//! Shell execution tool — runs commands in the project's workspace directory.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::process::Child;

use super::{Tool, ToolExecutionResult};
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

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        // Extract the command string from arguments.
        let command = match arguments.get("command").and_then(|v| v.as_str()) {
            Some(cmd) if !cmd.is_empty() => cmd.to_owned(),
            _ => {
                return ToolExecutionResult {
                    content: "Missing or empty 'command' argument.".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        // Spawn the command under sh -c so standard shell features are available.
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&self.workspace_root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ToolExecutionResult {
                    content: format!("Failed to spawn command: {e}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        // Take the piped stdout/stderr handles before starting the wait.
        // We hold `child` so we can kill it on timeout.
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
            // Propagate pipe read errors — unlikely but possible if the child
            // process is killed or the fd is invalidated mid-read.
            stdout_res.expect("failed to read child stdout");
            stderr_res.expect("failed to read child stderr");
            (stdout_buf, stderr_buf)
        };

        let read_result = tokio::time::timeout(self.timeout, read_fut).await;

        match read_result {
            Err(_elapsed) => {
                // Timeout expired — kill the child to free resources.
                let _ = kill_child(&mut child).await;
                ToolExecutionResult {
                    content: format!(
                        "Command timed out after {:.1}s",
                        self.timeout.as_secs_f64()
                    ),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }

            Ok((stdout_bytes, stderr_bytes)) => {
                // Reap the child to get the exit status.
                let exit_status = match child.wait().await {
                    Ok(s) => s,
                    Err(e) => {
                        return ToolExecutionResult {
                            content: format!("Failed to wait for command: {e}"),
                            is_error: true,
                            truncated: false,
                            total_bytes: None,
                        };
                    }
                };

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

                let (content, truncated, total_bytes) =
                    truncate_output(&combined, self.max_output_bytes);

                let total_bytes = if truncated { Some(total_bytes) } else { None };

                ToolExecutionResult {
                    content,
                    is_error: !exit_status.success(),
                    truncated,
                    total_bytes,
                }
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

    fn tool(timeout_secs: u64) -> ShellTool {
        ShellTool {
            workspace_root: std::env::temp_dir(),
            timeout: Duration::from_secs(timeout_secs),
            max_output_bytes: 1024 * 1024, // 1 MiB default
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn echo_hello_returns_output() {
        let t = tool(5);
        let result = t.execute(serde_json::json!({"command": "echo hello"})).await;
        assert!(!result.is_error, "echo should succeed");
        assert_eq!(result.content.trim(), "hello");
        assert!(!result.truncated);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonzero_exit_sets_is_error() {
        let t = tool(5);
        // `false` always exits with code 1.
        let result = t.execute(serde_json::json!({"command": "false"})).await;
        assert!(result.is_error, "non-zero exit should set is_error");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stderr_output_included() {
        let t = tool(5);
        let result = t
            .execute(serde_json::json!({"command": "echo errout >&2; exit 1"}))
            .await;
        assert!(result.is_error);
        assert!(
            result.content.contains("errout"),
            "stderr should appear in content: {:?}",
            result.content
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_command_returns_error() {
        let t = tool(5);
        let result = t.execute(serde_json::json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("command"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_kills_process_and_returns_error() {
        let t = ShellTool {
            workspace_root: std::env::temp_dir(),
            // Very short timeout — the sleep is 10s, so this should fire.
            timeout: Duration::from_millis(200),
            max_output_bytes: 1024 * 1024,
        };
        let result = t.execute(serde_json::json!({"command": "sleep 10"})).await;
        assert!(result.is_error, "timeout should produce an error");
        assert!(
            result.content.contains("timed out"),
            "error message should mention timeout: {:?}",
            result.content
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
        // Generate output well above the cap.
        let result = t
            .execute(serde_json::json!({"command": "printf '%0.s1234567890' $(seq 1 100)"}))
            .await;
        assert!(!result.is_error, "command should succeed");
        assert!(result.truncated, "output should be flagged as truncated");
        assert!(result.total_bytes.is_some());
        assert!(
            result.total_bytes.unwrap() > 10,
            "total_bytes should reflect original size"
        );
        assert!(result.content.len() <= 10, "content should be capped");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cwd_is_workspace_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let t = ShellTool {
            workspace_root: dir.path().to_path_buf(),
            timeout: Duration::from_secs(5),
            max_output_bytes: 1024 * 1024,
        };
        // `pwd` should print the workspace root (canonicalised).
        let result = t.execute(serde_json::json!({"command": "pwd"})).await;
        assert!(!result.is_error);
        // Both paths should be canonicalised — use contains as a stable check.
        let canonical = dir.path().canonicalize().unwrap();
        assert!(
            result.content.trim().contains(canonical.to_str().unwrap())
                || canonical.to_str().unwrap().contains(result.content.trim()),
            "pwd output {:?} should match workspace {:?}",
            result.content.trim(),
            canonical
        );
    }
}

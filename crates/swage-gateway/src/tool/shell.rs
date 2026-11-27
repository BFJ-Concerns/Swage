//! Shell execution tool — runs commands in the topic's workspace directory.

use std::collections::VecDeque;
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
};

// ---------------------------------------------------------------------------
// ShellTool
// ---------------------------------------------------------------------------

/// Executes shell commands in the turn workspace.
///
/// Commands run via `sh -c` with CWD set to the active ToolContext directory.
/// The two streams share `max_output_bytes`; the process group stops on timeout
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
        "Run a command line with `sh` in the session's working directory and return what it \
         printed. `output` holds its standard output, then its standard error after a \
         `stderr:` label; `exit_code` is its exit status, and `success` is true only when \
         that is zero, so a command that ran and failed is reported as failed. Each call \
         starts a new shell: a change of directory or a variable set in one call is gone in \
         the next. A command still running at the installation's time limit is stopped, \
         along with every process it started, and the call fails saying so. The two streams \
         share the installation's output limit. One too long to fit keeps its beginning and \
         its end: a bracketed line in `output` stands where the middle was left out and says \
         how many bytes that was, the stream is flagged (`stdout_truncated`, \
         `stderr_truncated`), and the result gives the path of a file holding the whole of \
         it (`stdout_artifact_path`, `stderr_artifact_path`), which `file_read` opens. \
         Unless the operator has \
         lifted it, the command runs sandboxed: it can change files under the working \
         directory and little outside it, so a write refused elsewhere is that boundary and \
         not a fault to retry. The file tools read, write, edit and search files directly; \
         this tool is for running programs."
    }

    fn execution_outcome(
        &self,
        result: &Value,
    ) -> swage_protocol::client::conversation::ToolExecution {
        if result["success"] == false {
            swage_protocol::client::conversation::ToolExecution::failed()
        } else {
            swage_protocol::client::conversation::ToolExecution::succeeded()
        }
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line, as `sh` reads it: pipes, redirections and several commands joined together all work."
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
        // A call that reports no output leaves no file of it behind.
        let unreported_artifacts = [&artifact_paths.stdout_path, &artifact_paths.stderr_path]
            .into_iter()
            .flatten()
            .map(|path| path.absolute.clone())
            .collect::<Vec<_>>();

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

        // Pipe EOF does not mean the command has exited. Both output collection
        // and process exit share one deadline; timeout stops the process group.
        let execution = async {
            let (stdout, stderr) = read_fut.await?;
            let exit_status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Failed to wait for command: {e}"),
            })?;
            Ok::<_, ToolError>((stdout, stderr, exit_status))
        };
        let result = Box::pin(tokio::time::timeout(self.timeout, execution)).await;

        match result {
            Err(_elapsed) => {
                // Timeout expired — stop the process group to free resources.
                kill_child(&mut child, &mut process_group).await;
                remove_artifacts(&unreported_artifacts).await;
                Err(ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!(
                        "Command timed out after {:.1}s (shell_timeout_secs)",
                        self.timeout.as_secs_f64()
                    ),
                })
            }

            Ok(Err(e)) => {
                // Output or process-wait error — stop the group and propagate.
                kill_child(&mut child, &mut process_group).await;
                remove_artifacts(&unreported_artifacts).await;
                Err(e)
            }

            Ok(Ok((stdout, stderr, exit_status))) => {
                process_group.disarm();

                // The streams share the limit, so that what `output` holds
                // of each is either all of it or flagged as cut, with the
                // whole stream left in its file.
                let (stdout_kept, stderr_kept) =
                    share_output_limit(stdout.total_bytes, stderr.total_bytes, cap);
                let stdout = stdout.kept_to(stdout_kept).await;
                let stderr = stderr.kept_to(stderr_kept).await;

                let stdout_display = stdout.display("stdout");
                let stderr_display = stderr.display("stderr");

                // Combine stdout then stderr (prefixed) so the model sees both.
                let combined = if stderr_display.is_empty() {
                    stdout_display
                } else if stdout_display.is_empty() {
                    format!("stderr: {stderr_display}")
                } else {
                    let separator = if stdout.truncated && !stdout_display.ends_with('\n') {
                        "\n"
                    } else {
                        ""
                    };
                    format!("{stdout_display}{separator}stderr: {stderr_display}")
                };

                let mut output = json!({ "output": combined });
                output["exit_code"] = json!(exit_status.code());
                output["success"] = json!(exit_status.success());
                if stdout.truncated || stderr.truncated {
                    output["truncated"] = json!(true);
                    output["total_bytes"] = json!(stdout.total_bytes + stderr.total_bytes);
                }
                if stdout.truncated {
                    output["stdout_truncated"] = json!(true);
                    output["stdout_total_bytes"] = json!(stdout.total_bytes);
                    if let Some(path) = stdout.reported_artifact_path {
                        output["stdout_artifact_path"] = json!(path);
                        output["stdout_follow_up"] = shell_output_follow_up(&path);
                    }
                }
                if stderr.truncated {
                    output["stderr_truncated"] = json!(true);
                    output["stderr_total_bytes"] = json!(stderr.total_bytes);
                    if let Some(path) = stderr.reported_artifact_path {
                        output["stderr_artifact_path"] = json!(path);
                        output["stderr_follow_up"] = shell_output_follow_up(&path);
                    }
                }
                Ok(output)
            }
        }
    }
}

/// One stream as it was read: at most the limit's worth of its bytes, how
/// many it produced, and the file that holds all of them.
struct BoundedRead {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: usize,
    artifact_path: Option<ToolOutputArtifactPath>,
}

/// One stream as the result reports it.
struct ReportedStream {
    head: Vec<u8>,
    tail: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
    reported_artifact_path: Option<String>,
}

impl BoundedRead {
    /// Fit the stream to its share of the output limit. A cut stream keeps
    /// its complete file for follow-up reads.
    async fn kept_to(mut self, kept: usize) -> ReportedStream {
        let truncated = self.total_bytes > kept;
        if truncated {
            let tail_kept = kept / 2;
            let missing_tail = tail_kept.saturating_sub(self.tail.len());
            // A tail smaller than its final share is contiguous with the
            // captured head. Move its missing bytes before shortening the head.
            for &byte in self.head.iter().rev().take(missing_tail) {
                self.tail.push_front(byte);
            }
            self.tail.drain(..self.tail.len().saturating_sub(tail_kept));
            self.head.truncate(kept.div_ceil(2));
        } else {
            self.head.extend(self.tail.drain(..));
            if let Some(path) = self.artifact_path.as_ref() {
                let _ = tokio::fs::remove_file(&path.absolute).await;
            }
        }
        ReportedStream {
            head: self.head,
            tail: self.tail.into(),
            total_bytes: self.total_bytes,
            truncated,
            reported_artifact_path: truncated
                .then(|| self.artifact_path.map(|path| path.reported))
                .flatten(),
        }
    }
}

impl ReportedStream {
    /// The stream's text, with the cut marked where part of it was left
    /// out so that the reader does not take what it holds for the whole.
    fn display(&self, stream: &str) -> String {
        let head = String::from_utf8_lossy(&self.head);
        if self.truncated {
            let marker = cut_marker(
                stream,
                self.total_bytes - self.head.len() - self.tail.len(),
                self.total_bytes,
            );
            let tail = String::from_utf8_lossy(&self.tail);
            format!("{head}{marker}{tail}")
        } else {
            head.into_owned()
        }
    }
}

/// The line that stands in `output` for the part of a cut stream left out,
/// at the place it was taken from. It sits on a line of its own, in
/// brackets and naming its stream, so that it is not read as something the
/// command printed, and counts what is missing so that the reader knows how
/// much of the stream it has not seen.
fn cut_marker(stream: &str, omitted_bytes: usize, total_bytes: usize) -> String {
    format!("\n[{stream} cut here: {omitted_bytes} of {total_bytes} bytes left out]\n")
}

/// How many bytes of each stream fit under `cap` together. Both are kept
/// whole when they fit. Otherwise the smaller is kept whole where it is
/// within half the limit and the other takes the rest, and two streams
/// that are both over half take half each.
fn share_output_limit(stdout: usize, stderr: usize, cap: usize) -> (usize, usize) {
    if stdout + stderr <= cap {
        return (stdout, stderr);
    }
    let half = cap / 2;
    if stdout <= half {
        (stdout, cap - stdout)
    } else if stderr <= half {
        (cap - stderr, stderr)
    } else {
        (cap - half, half)
    }
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

async fn remove_artifacts(paths: &[std::path::PathBuf]) {
    for path in paths {
        let _ = tokio::fs::remove_file(path).await;
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
    let head_limit = cap.div_ceil(2);
    let tail_limit = cap / 2;
    let mut head = Vec::with_capacity(head_limit.min(32768));
    let mut tail = VecDeque::new();
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
        let prefix_bytes = (head_limit - head.len()).min(n);
        head.extend_from_slice(&tmp[..prefix_bytes]);
        let rest = &tmp[prefix_bytes..n];
        let recent = &rest[rest.len().saturating_sub(tail_limit)..];
        tail.drain(..(tail.len() + recent.len()).saturating_sub(tail_limit));
        tail.extend(recent);
        total += n;
    }

    if let Some(writer) = artifact_writer.as_mut() {
        writer.flush().await?;
    }
    Ok(BoundedRead {
        head,
        tail,
        total_bytes: total,
        artifact_path,
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
    async fn timeout_stops_command_after_both_output_streams_close() {
        let workspace = tempfile::tempdir().expect("workspace");
        let context = ctx_in_dir_with_ids(&workspace);
        let t = ShellTool {
            timeout: Duration::from_millis(200),
            max_output_bytes: 1024 * 1024,
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            t.execute(
                json!({"command": "echo $$ > shell.pid; exec 1>&- 2>&-; sh -c 'echo $$ > descendant.pid; sleep 2; touch finished' & wait"}),
                &context,
            ),
        )
        .await
        .expect("closing output streams must not bypass the shell deadline");
        let error = result.expect_err("the command must time out before it finishes");
        let message = error.to_string();
        assert!(
            message.contains("timed out after 0.2s") && message.contains("shell_timeout_secs"),
            "{message}"
        );
        let pid: libc::pid_t = std::fs::read_to_string(workspace.path().join("shell.pid"))
            .expect("the actual shell must record its PID")
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "the shell must stop");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(
            workspace.path().join("descendant.pid").exists(),
            "the descendant must actually start before the deadline"
        );
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert!(
            !workspace.path().join("finished").exists(),
            "the command's descendants must stop at the deadline"
        );
        let next = t
            .execute(json!({"command": "printf fresh"}), &context)
            .await;
        assert_eq!(
            next.expect("the next shell call must work")["output"],
            "fresh"
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
                json!({"command": "printf start; printf '%0.s0123456789' $(seq 1 99); printf ' final'"}),
                &ctx(),
            )
            .await;
        let val = result.expect("command should succeed");
        let output = val["output"].as_str().unwrap();
        assert_eq!(val["exit_code"], json!(0));
        let (head, tail) = output
            .split_once("\n[stdout cut here: 991 of 1001 bytes left out]\n")
            .expect("stdout cut marker");
        assert!(head.starts_with("start"), "{output}");
        assert!(tail.ends_with("final"), "{output}");
        assert_eq!(head.len() + tail.len(), 10);
        assert_eq!(val["truncated"], json!(true));
        assert_eq!(val["total_bytes"], json!(1001));
        assert_eq!(val["stdout_truncated"], json!(true));
        assert_eq!(val["stdout_total_bytes"], json!(1001));
    }

    /// A command that fills the limit on stdout and then fails keeps its
    /// error: stderr is within its share, so it is reported whole, and
    /// stdout is the stream flagged as cut, with its file.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_stdout_does_not_push_a_short_stderr_out_of_the_output() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = ctx_in_dir_with_ids(&dir);
        let t = ShellTool {
            timeout: Duration::from_secs(5),
            max_output_bytes: 40,
        };

        let val = t
            .execute(
                json!({"command": "printf '%0.s1234567890' $(seq 1 10); echo 'build failed' >&2; exit 1"}),
                &ctx,
            )
            .await
            .expect("command should return its output");

        let output = val["output"].as_str().unwrap();
        assert!(output.ends_with("stderr: build failed\n"), "{output}");
        assert!(val.get("stderr_truncated").is_none(), "{val}");
        assert_eq!(val["stdout_truncated"], json!(true));
        let artifact_path = val["stdout_artifact_path"]
            .as_str()
            .expect("the cut stream keeps its file");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(artifact_path)).expect("stdout file"),
            "1234567890".repeat(10)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_cut_streams_keep_their_final_diagnostics() {
        let tool = ShellTool {
            timeout: Duration::from_secs(5),
            max_output_bytes: 100,
        };
        let result = tool
            .execute(
                json!({"command": "printf '%045dERROR!' 0; printf '%045dFAILED' 0 >&2; exit 1"}),
                &ctx(),
            )
            .await
            .expect("the command should return both streams");

        let output = result["output"].as_str().expect("shell output");
        assert_eq!(result["exit_code"], 1);
        assert_eq!(result["stdout_truncated"], true);
        assert_eq!(result["stderr_truncated"], true);
        assert!(output.contains("ERROR!\nstderr:"), "{output}");
        assert!(output.ends_with("FAILED"), "{output}");
    }

    #[test]
    fn two_long_streams_take_half_the_limit_each() {
        assert_eq!(share_output_limit(3, 4, 10), (3, 4));
        assert_eq!(share_output_limit(100, 2, 10), (8, 2));
        assert_eq!(share_output_limit(2, 100, 10), (2, 8));
        assert_eq!(share_output_limit(100, 100, 11), (6, 5));
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
                json!({"command": "{ printf start; printf '%0.s0123456789' $(seq 1 500); printf ' error'; } >&2; exit 1"}),
                &ctx(),
            )
            .await;
        let val = result.expect("command should return captured stderr");
        let output = val["output"].as_str().unwrap();
        assert_eq!(val["exit_code"], json!(1));
        let (head, tail) = output
            .strip_prefix("stderr: ")
            .expect("stderr label")
            .split_once("\n[stderr cut here: 5001 of 5011 bytes left out]\n")
            .expect("stderr cut marker");
        assert!(head.starts_with("start"), "{output}");
        assert!(tail.ends_with("error"), "{output}");
        assert_eq!(head.len() + tail.len(), 10);
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

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[tokio::test]
    async fn regression_output_uses_runtime_directory_and_reads_back() {
        use swage_tool::context::{MockToolContext, ToolOutputLocation, serialise_tool_context};
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("notes"), "reference").unwrap();
        let mut base = MockToolContext::new("agent", "session");
        base.working_directory = workspace.path().into();
        let mut ctx = serialise_tool_context(&base);
        ctx.tool_output_directory = Some(ToolOutputLocation {
            directory: runtime.path().into(),
        });
        let result = ShellTool {
            timeout: Duration::from_secs(5),
            max_output_bytes: 16,
        }
        .execute(json!({"command":"ls -la; seq 1 100"}), &ctx)
        .await
        .unwrap();
        let path = result["stdout_artifact_path"].as_str().unwrap();
        assert!(
            std::path::Path::new(path).starts_with(runtime.path()),
            "{result}"
        );
        assert!(!workspace.path().join(".swage").exists());
        let read = crate::tool::file_read::FileReadTool {
            max_output_bytes: 10000,
        }
        .execute(result["stdout_follow_up"]["arguments"].clone(), &ctx)
        .await
        .unwrap();
        assert!(read["output"].as_str().unwrap().contains("100\n"), "{read}");
    }
}

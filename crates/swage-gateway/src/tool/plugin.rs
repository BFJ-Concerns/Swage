use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::capability_bundle::{
    CapabilityBundle, CapabilityToolDefinition, discover_capability_bundles,
};
use crate::tool::Tool;

pub struct PluginTool {
    manifest: CapabilityToolDefinition,
    executable_path: PathBuf,
    timeout: Duration,
}

impl PluginTool {
    pub fn new(
        manifest: CapabilityToolDefinition,
        executable_path: PathBuf,
        timeout: Duration,
    ) -> Self {
        Self {
            manifest,
            executable_path,
            timeout,
        }
    }
}

#[async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn parameters(&self) -> Value {
        self.manifest.parameters.clone()
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        use tokio::io::AsyncWriteExt;
        use tokio::process::Command;

        let mut command = Command::new(&self.executable_path);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|error| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("failed to spawn plugin executable: {error}"),
            })?;

        let input = serde_json::json!({
            "params": params,
            "context": {
                "agent_id": ctx.agent_id(),
                "session_id": ctx.session_id(),
                "working_directory": ctx.working_directory(),
            }
        });

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.to_string().as_bytes())
                .await
                .map_err(|error| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("failed to write plugin stdin: {error}"),
                })?;
        }

        // `kill_on_drop` makes timeout cancellation terminate the consumed child
        // handle inside `wait_with_output` instead of leaving a plugin running.
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| ToolError::Timeout {
                name: self.name().to_owned(),
                duration: self.timeout,
            })?
            .map_err(|error| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("plugin execution failed: {error}"),
            })?;

        if !output.status.success() {
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            return Ok(Value::Null);
        }

        serde_json::from_str(&stdout).or(Ok(Value::String(stdout)))
    }
}

pub fn discover_plugin_tools(
    plugin_directory: &Path,
    timeout: Duration,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools = Vec::new();
    let mut warnings = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    let (bundles, bundle_warnings) =
        discover_capability_bundles(plugin_directory, swage_config::HierarchyLevel::Global);
    for warning in bundle_warnings {
        warnings.push(format!("{}: {}", warning.path.display(), warning.reason));
    }

    for bundle in bundles {
        match load_plugin_tool(&bundle, timeout) {
            Ok(tool) => {
                if !seen_names.insert(tool.name().to_string()) {
                    warnings.push(format!(
                        "skipping duplicate plugin tool '{}' from {}",
                        tool.name(),
                        bundle.bundle_dir.display()
                    ));
                    continue;
                }
                tools.push(Arc::new(tool) as Arc<dyn Tool>);
            }
            Err(error) => warnings.push(format!("{}: {error}", bundle.bundle_dir.display())),
        }
    }

    (tools, warnings)
}

fn load_plugin_tool(bundle: &CapabilityBundle, timeout: Duration) -> Result<PluginTool, String> {
    let manifest = bundle
        .tool
        .clone()
        .ok_or_else(|| "capability does not declare a tool contribution".to_string())?;

    let executable_path = bundle.bundle_dir.join(&manifest.executable);
    if !executable_path.is_file() {
        return Err(format!(
            "plugin executable does not exist: {}",
            executable_path.display()
        ));
    }

    Ok(PluginTool::new(manifest, executable_path, timeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_tool::context::MockToolContext;

    #[tokio::test]
    async fn plugin_tool_executes_json_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("echo-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("capability.json"),
            serde_json::json!({
                "description": "Echo plugin",
                "tool": {
                    "name": "echo_plugin",
                    "description": "Echo plugin",
                    "parameters": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
                    "executable": "run.py"
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("run.py"),
            "#!/usr/bin/env python3\nimport json,sys\npayload=json.load(sys.stdin)\nprint(json.dumps({'echo': payload['params']['text']}))\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(plugin_dir.join("run.py"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(plugin_dir.join("run.py"), perms).unwrap();
        }

        let bundle = crate::capability_bundle::read_capability_bundle(
            &plugin_dir,
            "echo-plugin",
            swage_config::HierarchyLevel::Global,
        )
        .unwrap();
        let tool = load_plugin_tool(&bundle, Duration::from_secs(5)).unwrap();
        let result = tool
            .execute(
                serde_json::json!({"text": "hello"}),
                &MockToolContext::new("agent", "session"),
            )
            .await
            .unwrap();

        assert_eq!(result, serde_json::json!({"echo": "hello"}));
    }
}

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;

#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub executable: String,
}

pub struct PluginTool {
    manifest: PluginManifest,
    executable_path: PathBuf,
    timeout: Duration,
}

impl PluginTool {
    pub fn new(manifest: PluginManifest, executable_path: PathBuf, timeout: Duration) -> Self {
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

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        use tokio::io::AsyncWriteExt;
        use tokio::process::Command;

        let mut child = Command::new(&self.executable_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
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

        serde_json::from_str(&stdout).or_else(|_| Ok(Value::String(stdout)))
    }
}

pub fn discover_plugin_tools(
    plugin_directory: &Path,
    timeout: Duration,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools = Vec::new();
    let mut warnings = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    let entries = match std::fs::read_dir(plugin_directory) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "failed to read plugin directory {}: {error}",
                plugin_directory.display()
            ));
            return (tools, warnings);
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        match load_plugin_tool(&path, timeout) {
            Ok(tool) => {
                if !seen_names.insert(tool.name().to_string()) {
                    warnings.push(format!(
                        "skipping duplicate plugin tool '{}' from {}",
                        tool.name(),
                        path.display()
                    ));
                    continue;
                }
                tools.push(Arc::new(tool) as Arc<dyn Tool>);
            }
            Err(error) => warnings.push(format!("{}: {error}", path.display())),
        }
    }

    (tools, warnings)
}

fn load_plugin_tool(path: &Path, timeout: Duration) -> Result<PluginTool, String> {
    let manifest_path = path.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("failed to read manifest.json: {error}"))?;
    let manifest: PluginManifest = serde_json::from_str(&manifest_text)
        .map_err(|error| format!("failed to parse manifest.json: {error}"))?;

    if manifest.name.trim().is_empty() {
        return Err("manifest name must not be empty".into());
    }
    if manifest.description.trim().is_empty() {
        return Err("manifest description must not be empty".into());
    }
    if !manifest.parameters.is_object() {
        return Err("manifest parameters must be a JSON object schema".into());
    }

    let executable_path = path.join(&manifest.executable);
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
            plugin_dir.join("manifest.json"),
            serde_json::json!({
                "name": "echo_plugin",
                "description": "Echo plugin",
                "parameters": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
                "executable": "run.py"
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
            let mut perms = std::fs::metadata(plugin_dir.join("run.py")).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(plugin_dir.join("run.py"), perms).unwrap();
        }

        let tool = load_plugin_tool(&plugin_dir, Duration::from_secs(5)).unwrap();
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

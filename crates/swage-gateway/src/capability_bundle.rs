use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swage_config::{
    HierarchyLevel,
    skill::{SkillDefinition, parse_skill_file},
};
use swage_core::GatewayError;

const CAPABILITY_MANIFEST_FILE: &str = "capability.json";
const SKILL_FILE: &str = "SKILL.md";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub executable: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CapabilityManifest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub requested_tools: Vec<String>,
    #[serde(default)]
    pub provided_ui_modules: Vec<String>,
    #[serde(default)]
    pub provider_integrations: Vec<String>,
    #[serde(default)]
    pub tool: Option<CapabilityToolDefinition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityBundle {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub requested_tools: Vec<String>,
    pub instructions: String,
    pub provided_ui_modules: Vec<String>,
    pub provider_integrations: Vec<String>,
    pub tool: Option<CapabilityToolDefinition>,
    pub bundle_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityBundleWarning {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityWriteRequest {
    pub display_name: Option<String>,
    pub description: String,
    pub requested_tools: Vec<String>,
    pub instructions: String,
    pub provided_ui_modules: Vec<String>,
    pub provider_integrations: Vec<String>,
    pub tool: Option<CapabilityToolDefinition>,
}

pub fn discover_capability_bundles(
    root: &Path,
    level: HierarchyLevel,
) -> (Vec<CapabilityBundle>, Vec<CapabilityBundleWarning>) {
    let mut bundles = Vec::new();
    let mut warnings = Vec::new();

    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (bundles, warnings),
        Err(error) => {
            warnings.push(CapabilityBundleWarning {
                path: root.to_path_buf(),
                reason: format!("failed to read capability directory: {error}"),
            });
            return (bundles, warnings);
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        match read_capability_bundle(&path, name, level.clone()) {
            Ok(bundle) => bundles.push(bundle),
            Err(error) => warnings.push(CapabilityBundleWarning {
                path,
                reason: error.to_string(),
            }),
        }
    }

    bundles.sort_by(|left, right| left.name.cmp(&right.name));
    (bundles, warnings)
}

pub fn read_capability_bundle(
    bundle_dir: &Path,
    bundle_name: &str,
    level: HierarchyLevel,
) -> Result<CapabilityBundle, GatewayError> {
    validate_capability_name(bundle_name)?;

    let manifest_path = bundle_dir.join(CAPABILITY_MANIFEST_FILE);
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(raw) => serde_json::from_str::<CapabilityManifest>(&raw).map_err(|error| {
            GatewayError::ValidationError(format!(
                "failed to parse capability manifest {}: {error}",
                manifest_path.display()
            ))
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CapabilityManifest::default(),
        Err(error) => {
            return Err(GatewayError::ValidationError(format!(
                "failed to read capability manifest {}: {error}",
                manifest_path.display()
            )));
        }
    };

    let skill_path = bundle_dir.join(SKILL_FILE);
    let skill = match std::fs::metadata(&skill_path) {
        Ok(metadata) if metadata.is_file() => Some(
            parse_skill_file(&skill_path, bundle_name, level)
                .map_err(|warning| GatewayError::ValidationError(warning.reason))?,
        ),
        Ok(_) => {
            return Err(GatewayError::ValidationError(format!(
                "{} exists but is not a file",
                skill_path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(GatewayError::ValidationError(format!(
                "failed to inspect capability skill file {}: {error}",
                skill_path.display()
            )));
        }
    };

    let description = skill
        .as_ref()
        .map(|skill| skill.description.clone())
        .or_else(|| manifest.description.clone())
        .ok_or_else(|| {
            GatewayError::ValidationError(format!(
                "capability {bundle_name} must define a description in SKILL.md or capability.json"
            ))
        })?;
    let display_name = skill
        .as_ref()
        .and_then(|skill| skill.display_name.clone())
        .or_else(|| manifest.display_name.clone());
    let requested_tools = if let Some(skill) = &skill {
        skill.tools.clone()
    } else {
        manifest.requested_tools.clone()
    };
    let instructions = skill
        .as_ref()
        .map(|skill| skill.instructions.clone())
        .unwrap_or_default();

    validate_capability_parts(
        &description,
        &requested_tools,
        &instructions,
        &manifest.provided_ui_modules,
        &manifest.provider_integrations,
        manifest.tool.as_ref(),
    )?;

    Ok(CapabilityBundle {
        name: bundle_name.to_string(),
        display_name,
        description,
        requested_tools,
        instructions,
        provided_ui_modules: manifest.provided_ui_modules,
        provider_integrations: manifest.provider_integrations,
        tool: manifest.tool,
        bundle_dir: bundle_dir.to_path_buf(),
    })
}

pub fn validate_capability_name(name: &str) -> Result<String, GatewayError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(GatewayError::ValidationError(
            "capability name must not be empty".into(),
        ));
    }
    if !trimmed
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        return Err(GatewayError::ValidationError(
            "capability name must match [a-z0-9-]+".into(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn validate_capability_parts(
    description: &str,
    requested_tools: &[String],
    instructions: &str,
    provided_ui_modules: &[String],
    provider_integrations: &[String],
    tool: Option<&CapabilityToolDefinition>,
) -> Result<(), GatewayError> {
    if description.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "capability description must not be empty".into(),
        ));
    }
    if requested_tools
        .iter()
        .any(|tool_name| tool_name.trim().is_empty())
    {
        return Err(GatewayError::ValidationError(
            "requested_tools must not contain empty entries".into(),
        ));
    }
    if provided_ui_modules
        .iter()
        .any(|module| module.trim().is_empty())
    {
        return Err(GatewayError::ValidationError(
            "provided_ui_modules must not contain empty entries".into(),
        ));
    }
    if provider_integrations
        .iter()
        .any(|integration| integration.trim().is_empty())
    {
        return Err(GatewayError::ValidationError(
            "provider_integrations must not contain empty entries".into(),
        ));
    }
    if instructions.trim().is_empty()
        && tool.is_none()
        && provided_ui_modules.is_empty()
        && provider_integrations.is_empty()
    {
        return Err(GatewayError::ValidationError(
            "capability must provide instructions, a tool, ui modules, or provider integrations"
                .into(),
        ));
    }
    if let Some(tool) = tool {
        if tool.name.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "tool capability name must not be empty".into(),
            ));
        }
        if tool.description.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "tool capability description must not be empty".into(),
            ));
        }
        if !tool.parameters.is_object() {
            return Err(GatewayError::ValidationError(
                "tool capability parameters must be a JSON object schema".into(),
            ));
        }
        if tool.executable.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "tool capability executable must not be empty".into(),
            ));
        }
        let executable_path = Path::new(&tool.executable);
        if executable_path.is_absolute()
            || tool
                .executable
                .split(std::path::MAIN_SEPARATOR)
                .any(|segment| segment == "..")
        {
            return Err(GatewayError::ValidationError(
                "tool capability executable must stay within the capability directory".into(),
            ));
        }
    }
    Ok(())
}

pub fn write_capability_bundle(
    bundle_dir: &Path,
    capability_name: &str,
    request: &CapabilityWriteRequest,
) -> Result<(), GatewayError> {
    validate_capability_name(capability_name)?;
    validate_capability_parts(
        &request.description,
        &request.requested_tools,
        &request.instructions,
        &request.provided_ui_modules,
        &request.provider_integrations,
        request.tool.as_ref(),
    )?;

    std::fs::create_dir_all(bundle_dir).map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to create capability directory {}: {error}",
            bundle_dir.display()
        ))
    })?;

    let manifest = CapabilityManifest {
        display_name: request.display_name.clone(),
        description: Some(request.description.clone()),
        requested_tools: request.requested_tools.clone(),
        provided_ui_modules: request.provided_ui_modules.clone(),
        provider_integrations: request.provider_integrations.clone(),
        tool: request.tool.clone(),
    };
    let manifest_path = bundle_dir.join(CAPABILITY_MANIFEST_FILE);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest)
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
    )
    .map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to write capability manifest {}: {error}",
            manifest_path.display()
        ))
    })?;

    let skill_path = bundle_dir.join(SKILL_FILE);
    std::fs::write(
        &skill_path,
        render_skill_markdown(
            request.display_name.as_deref(),
            &request.description,
            &request.requested_tools,
            &request.instructions,
        ),
    )
    .map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to write capability skill {}: {error}",
            skill_path.display()
        ))
    })?;

    let _ = read_capability_bundle(bundle_dir, capability_name, HierarchyLevel::Space)?;
    Ok(())
}

pub fn render_skill_markdown(
    display_name: Option<&str>,
    description: &str,
    requested_tools: &[String],
    instructions: &str,
) -> String {
    let mut rendered = String::from("---\n");
    if let Some(display_name) = display_name.filter(|value| !value.trim().is_empty()) {
        rendered.push_str("name: ");
        rendered.push_str(display_name.trim());
        rendered.push('\n');
    }
    rendered.push_str("description: ");
    rendered.push_str(description.trim());
    rendered.push('\n');
    if !requested_tools.is_empty() {
        rendered.push_str("tools:\n");
        for tool in requested_tools {
            rendered.push_str("  - ");
            rendered.push_str(tool);
            rendered.push('\n');
        }
    }
    rendered.push_str("---\n\n");
    rendered.push_str(instructions.trim());
    rendered.push('\n');
    rendered
}

pub fn skill_definition_from_bundle(
    bundle: &CapabilityBundle,
    level: HierarchyLevel,
) -> Option<SkillDefinition> {
    if bundle.instructions.trim().is_empty() {
        return None;
    }
    Some(SkillDefinition {
        name: bundle.name.clone(),
        display_name: bundle.display_name.clone(),
        description: bundle.description.clone(),
        instructions: bundle.instructions.clone(),
        tools: bundle.requested_tools.clone(),
        level,
        source_path: bundle.bundle_dir.join(SKILL_FILE),
        shipped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bundle_round_trips_tool_and_ui_contributions() {
        let dir = tempfile::tempdir().unwrap();
        let capability_dir = dir.path().join("delivery-brief");

        write_capability_bundle(
            &capability_dir,
            "delivery-brief",
            &CapabilityWriteRequest {
                display_name: Some("Delivery Brief".into()),
                description: "Space delivery capability.".into(),
                requested_tools: vec!["shell".into()],
                instructions: "Keep delivery work concise.".into(),
                provided_ui_modules: vec!["brief-board".into()],
                provider_integrations: vec!["openai.responses".into()],
                tool: Some(CapabilityToolDefinition {
                    name: "delivery_brief".into(),
                    description: "Generate a delivery brief".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {"topic": {"type": "string"}}
                    }),
                    executable: "run.py".into(),
                }),
            },
        )
        .unwrap();

        let bundle =
            read_capability_bundle(&capability_dir, "delivery-brief", HierarchyLevel::Space)
                .unwrap();

        assert_eq!(bundle.display_name.as_deref(), Some("Delivery Brief"));
        assert_eq!(bundle.provided_ui_modules, vec!["brief-board"]);
        assert_eq!(bundle.provider_integrations, vec!["openai.responses"]);
        assert_eq!(bundle.tool.as_ref().unwrap().name, "delivery_brief");
    }

    #[test]
    fn discover_capability_bundles_reports_invalid_entries_without_stopping() {
        let dir = tempfile::tempdir().unwrap();
        let valid_dir = dir.path().join("valid");
        let invalid_dir = dir.path().join("invalid");
        std::fs::create_dir_all(&valid_dir).unwrap();
        std::fs::create_dir_all(&invalid_dir).unwrap();
        std::fs::write(
            valid_dir.join("capability.json"),
            serde_json::json!({
                "description": "Valid capability",
                "provided_ui_modules": ["valid-module"]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(invalid_dir.join("capability.json"), "{not json").unwrap();

        let (bundles, warnings) = discover_capability_bundles(dir.path(), HierarchyLevel::Global);

        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].name, "valid");
        assert_eq!(warnings.len(), 1);
    }
}

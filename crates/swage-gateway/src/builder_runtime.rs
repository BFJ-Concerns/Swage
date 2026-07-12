use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use swage_core::{ChatMessage, CompletionRequest, GatewayError, Role};
use swage_storage::{ConfigurationVersion, NewAuditEvent, NewConfigurationVersion};

use crate::{
    lens::{
        CreateLensRequest, InstallLensRequest, LensDescriptor, UpdateLensRequest,
        validate_lens_payload,
    },
    service::{
        CreateTeamRequest, TeamAgentPayload, TeamResponse, TeamService, TeamTaskPayload,
        UpdateTeamRequest,
    },
    space_extension as space_extension_api,
    space_extension::{
        CreateSpaceExtensionRequest, SpaceExtensionResponse, UpdateSpaceExtensionRequest,
        validate_extension_body, validate_extension_name,
    },
    state::AppState,
};

const BUILDER_CONFIG_SERVICE_NAME: &str = "builder_configuration";

#[derive(Debug, Clone, Deserialize)]
pub struct BuilderProposalRequest {
    pub prompt: String,
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuilderApplyRequest {
    #[serde(default)]
    pub request_text: Option<String>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub operations: Vec<BuilderConfigOperation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuilderProposalResponse {
    pub summary: String,
    pub operations: Vec<BuilderReviewedOperation>,
    pub diagnostics: Vec<String>,
    pub can_apply: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuilderReviewedOperation {
    pub kind: String,
    pub summary: String,
    pub ready: bool,
    pub diagnostics: Vec<String>,
    pub operation: BuilderConfigOperation,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuilderApplyResponse {
    pub success: bool,
    pub summary: String,
    pub diagnostics: Vec<String>,
    pub applied: Vec<BuilderAppliedOperation>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ConfigurationVersionQuery {
    pub entity_kind: Option<String>,
    pub entity_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigurationVersionResponse {
    pub id: Uuid,
    pub space_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub entity_kind: String,
    pub entity_id: String,
    pub version_number: i64,
    pub operation_kind: String,
    pub source: String,
    pub summary: String,
    pub snapshot: serde_json::Value,
    pub diff: serde_json::Value,
    pub previous_version_id: Option<Uuid>,
    pub rollback_to_version_id: Option<Uuid>,
    pub created_at: String,
    pub is_current: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigurationVersionListResponse {
    pub versions: Vec<ConfigurationVersionResponse>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RollbackConfigurationVersionRequest {
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RollbackConfigurationVersionResponse {
    pub restored_version: ConfigurationVersionResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuilderAppliedOperation {
    pub kind: String,
    pub summary: String,
    pub entity_kind: String,
    pub entity_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuilderConfigOperation {
    CreateLens {
        id: String,
        title: String,
        summary: String,
        default_surface: String,
        #[serde(default)]
        ui_modules: Vec<String>,
        #[serde(default)]
        prominent_team_ids: Vec<String>,
    },
    UpdateLens {
        lens_id: String,
        title: String,
        summary: String,
        default_surface: String,
        #[serde(default)]
        ui_modules: Vec<String>,
        #[serde(default)]
        prominent_team_ids: Vec<String>,
    },
    InstallLensPreset {
        preset_id: String,
    },
    CreateTeam {
        name: String,
        #[serde(default)]
        description: String,
        entry_agent_id: String,
        #[serde(default)]
        members: Vec<String>,
    },
    UpdateTeam {
        #[serde(default)]
        team_id: Option<Uuid>,
        #[serde(default)]
        team_name: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        entry_agent_id: Option<String>,
        #[serde(default)]
        members: Option<Vec<String>>,
    },
    CreateSpaceExtension {
        name: String,
        #[serde(default)]
        display_name: Option<String>,
        description: String,
        #[serde(default)]
        requested_tools: Vec<String>,
        #[serde(default)]
        provided_ui_modules: Vec<String>,
        #[serde(default)]
        provider_integrations: Vec<String>,
        #[serde(default)]
        tool: Option<crate::capability_bundle::CapabilityToolDefinition>,
        instructions: String,
    },
    UpdateSpaceExtension {
        extension_name: String,
        #[serde(default)]
        display_name: Option<String>,
        description: String,
        #[serde(default)]
        requested_tools: Vec<String>,
        #[serde(default)]
        provided_ui_modules: Vec<String>,
        #[serde(default)]
        provider_integrations: Vec<String>,
        #[serde(default)]
        tool: Option<crate::capability_bundle::CapabilityToolDefinition>,
        instructions: String,
    },
    ApproveWorkspaceExtension {
        extension_name: String,
    },
    CreateAgent {
        agent_id: String,
        #[serde(default)]
        team_name: Option<String>,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        instructions: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    UpdateAgent {
        agent_id: String,
        #[serde(default)]
        team_name: Option<String>,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        instructions: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    CreateTask {
        task_name: String,
        #[serde(default)]
        team_name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        instructions: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        output_schema: Option<serde_json::Value>,
    },
    UpdateTask {
        task_name: String,
        #[serde(default)]
        team_name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        instructions: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        output_schema: Option<serde_json::Value>,
    },
}

#[derive(Debug, Deserialize)]
struct BuilderPlannerOutput {
    summary: String,
    #[serde(default)]
    operations: Vec<BuilderConfigOperation>,
    #[serde(default)]
    diagnostics: Vec<String>,
}

#[derive(Clone)]
pub struct BuilderConfigurationService {
    state: AppState,
}

impl BuilderConfigurationService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub async fn propose_changes(
        &self,
        space_id: Uuid,
        request: BuilderProposalRequest,
    ) -> Result<BuilderProposalResponse, GatewayError> {
        if request.prompt.trim().is_empty() {
            return Err(GatewayError::ValidationError(
                "builder prompt must not be empty".into(),
            ));
        }

        let space = self
            .state
            .space_store
            .get(space_id)
            .await?
            .ok_or(GatewayError::SpaceNotFound { id: space_id })?;

        let lenses = self.state.storage.list_lenses().await?;
        let teams = TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
            .list_teams(space_id)
            .await?
            .teams;
        let extensions = space_extension_api::list_space_extensions(&self.state, space_id)
            .await?
            .extensions;
        let workspaces = self
            .state
            .storage
            .list_workspace_attachments_by_space(space_id)
            .await?;

        let prompt_context = json!({
            "space": {
                "id": space.id,
                "name": space.name,
                "slug": space.slug,
                "workspaces": workspaces,
            },
            "lenses": lenses,
            "teams": teams,
            "extensions": extensions,
            "constraints": [
                "Return strict JSON only. Do not wrap it in markdown fences.",
                "Plan only operations that the builder runtime can persist.",
                "Agent authoring is not supported yet. If the user asks for agent creation or edits, emit a create_agent or update_agent operation so the runtime can return an explicit diagnostic instead of inventing unsupported storage.",
                "Prefer updating existing entities when the request clearly refers to them."
            ],
        });

        let system_prompt = [
            "You are the Swage builder runtime planner.",
            "Translate the user's natural-language request into a JSON change proposal.",
            "Use this exact shape:",
            r#"{"summary":"string","operations":[{"kind":"create_lens"}],"diagnostics":["string"]}"#,
            "Valid operation kinds are: create_lens, update_lens, install_lens_preset, create_team, update_team, create_space_extension, update_space_extension, approve_workspace_extension, create_agent, update_agent.",
            "Do not include commentary outside JSON.",
        ]
        .join("\n");

        let user_prompt = format!(
            "Space context:\n{}\n\nUser request:\n{}",
            serde_json::to_string_pretty(&prompt_context)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            request.prompt.trim()
        );

        let completion = self
            .state
            .provider
            .complete(CompletionRequest {
                messages: vec![
                    ChatMessage::new(Role::System, system_prompt),
                    ChatMessage::new(Role::User, user_prompt),
                ],
                model: None,
                max_tokens: Some(1_200),
                temperature: Some(0.0),
                stop_sequences: None,
                metadata: None,
                tool_definitions: Vec::new(),
            })
            .await
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "builder proposal generation failed: {error}"
                ))
            })?;

        let planner_output = parse_builder_output(&completion.content)?;
        let reviewed = self
            .review_operations(space_id, &planner_output.operations)
            .await?;
        let can_apply = !reviewed.is_empty() && reviewed.iter().all(|operation| operation.ready);

        Ok(BuilderProposalResponse {
            summary: planner_output.summary,
            operations: reviewed,
            diagnostics: planner_output.diagnostics,
            can_apply,
        })
    }

    pub async fn apply_operations(
        &self,
        space_id: Uuid,
        request: BuilderApplyRequest,
    ) -> Result<BuilderApplyResponse, GatewayError> {
        let reviewed = self
            .review_operations(space_id, &request.operations)
            .await?;
        let diagnostics: Vec<String> = reviewed
            .iter()
            .flat_map(|operation| operation.diagnostics.clone())
            .collect();

        if reviewed.is_empty() {
            return Ok(BuilderApplyResponse {
                success: false,
                summary: "No changes were applied.".into(),
                diagnostics: vec!["The builder proposal contained no operations.".into()],
                applied: Vec::new(),
            });
        }

        if reviewed.iter().any(|operation| !operation.ready) {
            return Ok(BuilderApplyResponse {
                success: false,
                summary: "Builder review blocked the requested changes.".into(),
                diagnostics,
                applied: Vec::new(),
            });
        }

        let mut applied = Vec::new();
        for operation in request.operations {
            applied.push(
                self.apply_operation(
                    space_id,
                    operation,
                    request.session_id,
                    "conversational_builder",
                    request.request_text.as_deref(),
                )
                .await?,
            );
        }

        Ok(BuilderApplyResponse {
            success: true,
            summary: format!("Applied {} builder change(s).", applied.len()),
            diagnostics: Vec::new(),
            applied,
        })
    }

    pub async fn create_lens(
        &self,
        request: CreateLensRequest,
        space_id: Option<Uuid>,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<LensDescriptor, GatewayError> {
        let before = self.capture_lens_snapshot(&request.id).await?;
        let saved = crate::lens::create_lens(&self.state, request).await?;
        self.record_configuration_change(
            "lens",
            saved.id.to_string(),
            "create_lens",
            source,
            space_id,
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn update_lens(
        &self,
        lens_id: &str,
        request: UpdateLensRequest,
        space_id: Option<Uuid>,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<LensDescriptor, GatewayError> {
        let before = self.capture_lens_snapshot(lens_id).await?;
        let saved = crate::lens::update_lens(&self.state, lens_id, request).await?;
        self.record_configuration_change(
            "lens",
            saved.id.to_string(),
            "update_lens",
            source,
            space_id,
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn install_lens_preset(
        &self,
        request: InstallLensRequest,
        space_id: Option<Uuid>,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<LensDescriptor, GatewayError> {
        let before = self.capture_lens_preset_target(&request.preset_id).await?;
        let saved = crate::lens::install_lens(&self.state, request).await?;
        self.record_configuration_change(
            "lens",
            saved.id.to_string(),
            "install_lens_preset",
            source,
            space_id,
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn create_team(
        &self,
        space_id: Uuid,
        request: CreateTeamRequest,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<TeamResponse, GatewayError> {
        let before = None;
        let saved = TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
            .create_team(space_id, request)
            .await?;
        self.record_configuration_change(
            "team",
            saved.id.to_string(),
            "create_team",
            source,
            Some(space_id),
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn update_team(
        &self,
        team_id: Uuid,
        request: UpdateTeamRequest,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<TeamResponse, GatewayError> {
        let before = self.capture_team_snapshot(team_id).await?;
        let saved = TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
            .update_team(team_id, request)
            .await?;
        self.record_configuration_change(
            "team",
            saved.id.to_string(),
            "update_team",
            source,
            Some(saved.space_id),
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn create_space_extension(
        &self,
        space_id: Uuid,
        request: CreateSpaceExtensionRequest,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<SpaceExtensionResponse, GatewayError> {
        let before = self
            .capture_space_extension_snapshot(space_id, &request.name)
            .await?;
        let saved =
            space_extension_api::create_space_extension(&self.state, space_id, request).await?;
        self.record_configuration_change(
            "space_extension",
            saved.name.clone(),
            "create_space_extension",
            source,
            Some(space_id),
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn update_space_extension(
        &self,
        space_id: Uuid,
        extension_name: &str,
        request: UpdateSpaceExtensionRequest,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<SpaceExtensionResponse, GatewayError> {
        let before = self
            .capture_space_extension_snapshot(space_id, extension_name)
            .await?;
        let saved = space_extension_api::update_space_extension(
            &self.state,
            space_id,
            extension_name,
            request,
        )
        .await?;
        self.record_configuration_change(
            "space_extension",
            saved.name.clone(),
            "update_space_extension",
            source,
            Some(space_id),
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn approve_workspace_extension(
        &self,
        space_id: Uuid,
        extension_name: &str,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<SpaceExtensionResponse, GatewayError> {
        let before = self
            .capture_space_extension_snapshot(space_id, extension_name)
            .await?;
        let saved =
            space_extension_api::approve_workspace_extension(&self.state, space_id, extension_name)
                .await?;
        self.record_configuration_change(
            "space_extension",
            saved.name.clone(),
            "approve_workspace_extension",
            source,
            Some(space_id),
            session_id,
            before,
            serde_json::to_value(&saved)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            None,
        )
        .await?;
        Ok(saved)
    }

    pub async fn list_configuration_versions(
        &self,
        space_id: Uuid,
        query: ConfigurationVersionQuery,
    ) -> Result<ConfigurationVersionListResponse, GatewayError> {
        let versions = match (query.entity_kind.as_deref(), query.entity_id.as_deref()) {
            (Some(entity_kind), Some(entity_id)) => {
                self.state
                    .storage
                    .list_configuration_versions_for_entity(entity_kind, entity_id)
                    .await?
            }
            (None, None) => {
                self.state
                    .storage
                    .list_configuration_versions(Some(space_id))
                    .await?
            }
            _ => {
                return Err(GatewayError::ValidationError(
                    "entity_kind and entity_id must be provided together".into(),
                ));
            }
        };

        let mut latest_by_entity = std::collections::HashMap::new();
        for version in &versions {
            latest_by_entity
                .entry((version.entity_kind.clone(), version.entity_id.clone()))
                .or_insert(version.id);
        }

        Ok(ConfigurationVersionListResponse {
            versions: versions
                .into_iter()
                .filter(|version| version.space_id == Some(space_id))
                .map(|version| {
                    configuration_version_to_response(
                        version.clone(),
                        latest_by_entity
                            .get(&(version.entity_kind.clone(), version.entity_id.clone()))
                            .copied()
                            == Some(version.id),
                    )
                })
                .collect(),
        })
    }

    pub async fn rollback_configuration_version(
        &self,
        version_id: Uuid,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<RollbackConfigurationVersionResponse, GatewayError> {
        let version = self
            .state
            .storage
            .get_configuration_version(version_id)
            .await?
            .ok_or_else(|| {
                GatewayError::ValidationError(format!(
                    "configuration version {version_id} does not exist"
                ))
            })?;

        let restored = match version.entity_kind.as_str() {
            "lens" => {
                self.rollback_lens_version(&version, session_id, source)
                    .await?
            }
            "team" => {
                self.rollback_team_version(&version, session_id, source)
                    .await?
            }
            "team_agent" => {
                self.rollback_team_agent_version(&version, session_id, source)
                    .await?
            }
            "team_task" => {
                self.rollback_team_task_version(&version, session_id, source)
                    .await?
            }
            "space_extension" => {
                self.rollback_space_extension_version(&version, session_id, source)
                    .await?
            }
            other => {
                return Err(GatewayError::ValidationError(format!(
                    "rollback is not supported for configuration entity kind {other}"
                )));
            }
        };

        Ok(RollbackConfigurationVersionResponse {
            restored_version: configuration_version_to_response(restored, true),
        })
    }

    async fn review_operations(
        &self,
        space_id: Uuid,
        operations: &[BuilderConfigOperation],
    ) -> Result<Vec<BuilderReviewedOperation>, GatewayError> {
        let mut reviewed = Vec::with_capacity(operations.len());
        for operation in operations {
            reviewed.push(self.review_operation(space_id, operation.clone()).await?);
        }
        Ok(reviewed)
    }

    async fn review_operation(
        &self,
        space_id: Uuid,
        operation: BuilderConfigOperation,
    ) -> Result<BuilderReviewedOperation, GatewayError> {
        let mut diagnostics = Vec::new();

        match &operation {
            BuilderConfigOperation::CreateLens {
                id,
                title,
                summary,
                default_surface,
                ui_modules,
                ..
            } => {
                validate_lens_payload(title, summary, default_surface, ui_modules)?;
                swage_storage::AppLens::new(id.clone()).map_err(GatewayError::ValidationError)?;
                if self
                    .state
                    .storage
                    .get_lens(
                        &swage_storage::AppLens::new(id.clone())
                            .map_err(GatewayError::ValidationError)?,
                    )
                    .await?
                    .is_some()
                {
                    diagnostics.push(format!("Lens {id} already exists."));
                }
            }
            BuilderConfigOperation::UpdateLens {
                lens_id,
                title,
                summary,
                default_surface,
                ui_modules,
                ..
            } => {
                validate_lens_payload(title, summary, default_surface, ui_modules)?;
                let lens_id = swage_storage::AppLens::new(lens_id.clone())
                    .map_err(GatewayError::ValidationError)?;
                let existing = self.state.storage.get_lens(&lens_id).await?;
                match existing {
                    None => diagnostics.push(format!("Lens {lens_id} does not exist.")),
                    Some(record)
                        if record.source_kind == swage_storage::LensSourceKind::BuiltIn =>
                    {
                        diagnostics.push(format!("Built-in lens {lens_id} cannot be edited."))
                    }
                    Some(_) => {}
                }
            }
            BuilderConfigOperation::InstallLensPreset { preset_id } => {
                crate::lens::preset_lens(preset_id)?;
            }
            BuilderConfigOperation::CreateTeam {
                name,
                entry_agent_id,
                members,
                ..
            } => {
                if name.trim().is_empty() {
                    diagnostics.push("Team name must not be empty.".into());
                }
                if entry_agent_id.trim().is_empty() {
                    diagnostics.push("entry_agent_id must not be empty.".into());
                }
                if members.iter().any(|member| member.trim().is_empty()) {
                    diagnostics.push("members must not contain empty agent IDs.".into());
                }
                if self
                    .state
                    .storage
                    .get_team_by_name(space_id, name)
                    .await?
                    .is_some()
                {
                    diagnostics.push(format!("A team named {name} already exists."));
                }
            }
            BuilderConfigOperation::UpdateTeam {
                team_id,
                team_name,
                entry_agent_id,
                members,
                ..
            } => {
                if team_id.is_none() && team_name.as_deref().unwrap_or("").trim().is_empty() {
                    diagnostics.push("update_team needs team_id or team_name.".into());
                }
                if let Some(entry_agent_id) = entry_agent_id
                    && entry_agent_id.trim().is_empty()
                {
                    diagnostics.push("entry_agent_id must not be empty.".into());
                }
                if let Some(members) = members
                    && members.iter().any(|member| member.trim().is_empty())
                {
                    diagnostics.push("members must not contain empty agent IDs.".into());
                }
                if self
                    .resolve_team(space_id, *team_id, team_name.as_deref())
                    .await?
                    .is_none()
                {
                    diagnostics.push("The referenced team does not exist in this space.".into());
                }
            }
            BuilderConfigOperation::CreateSpaceExtension {
                name,
                description,
                requested_tools,
                provided_ui_modules,
                provider_integrations,
                tool,
                instructions,
                ..
            } => {
                validate_extension_name(name)?;
                validate_extension_body(
                    description,
                    requested_tools,
                    instructions,
                    provided_ui_modules,
                    provider_integrations,
                    tool.as_ref(),
                )?;
                let extensions =
                    space_extension_api::list_space_extensions(&self.state, space_id).await?;
                if extensions
                    .extensions
                    .iter()
                    .any(|extension| extension.name == *name && extension.source_kind == "managed")
                {
                    diagnostics.push(format!("Managed extension {name} already exists."));
                }
            }
            BuilderConfigOperation::UpdateSpaceExtension {
                extension_name,
                description,
                requested_tools,
                provided_ui_modules,
                provider_integrations,
                tool,
                instructions,
                ..
            } => {
                validate_extension_name(extension_name)?;
                validate_extension_body(
                    description,
                    requested_tools,
                    instructions,
                    provided_ui_modules,
                    provider_integrations,
                    tool.as_ref(),
                )?;
                let extensions =
                    space_extension_api::list_space_extensions(&self.state, space_id).await?;
                if !extensions.extensions.iter().any(|extension| {
                    extension.name == *extension_name && extension.source_kind == "managed"
                }) {
                    diagnostics.push(format!(
                        "Managed extension {extension_name} does not exist."
                    ));
                }
            }
            BuilderConfigOperation::ApproveWorkspaceExtension { extension_name } => {
                validate_extension_name(extension_name)?;
                let extensions =
                    space_extension_api::list_space_extensions(&self.state, space_id).await?;
                if !extensions.extensions.iter().any(|extension| {
                    extension.name == *extension_name && extension.status == "approval_required"
                }) {
                    diagnostics.push(format!(
                        "Workspace extension {extension_name} is not awaiting approval."
                    ));
                }
            }
            BuilderConfigOperation::CreateAgent {
                agent_id,
                team_name,
                ..
            } => {
                if agent_id.trim().is_empty() {
                    diagnostics.push("agent_id must not be empty.".into());
                }
                let Some(team) = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                else {
                    diagnostics.push("create_agent needs a target team.".into());
                    return Ok(reviewed_operation(operation, diagnostics));
                };
                if team.agents.iter().any(|agent| agent.id == *agent_id) {
                    diagnostics.push(format!(
                        "Team {} already defines agent {}.",
                        team.name, agent_id
                    ));
                }
            }
            BuilderConfigOperation::UpdateAgent {
                agent_id,
                team_name,
                ..
            } => {
                if agent_id.trim().is_empty() {
                    diagnostics.push("agent_id must not be empty.".into());
                }
                let Some(team) = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                else {
                    diagnostics.push("update_agent needs a target team.".into());
                    return Ok(reviewed_operation(operation, diagnostics));
                };
                if !team.agents.iter().any(|agent| agent.id == *agent_id) {
                    diagnostics.push(format!(
                        "Team {} does not define agent {}.",
                        team.name, agent_id
                    ));
                }
            }
            BuilderConfigOperation::CreateTask {
                task_name,
                team_name,
                ..
            } => {
                if task_name.trim().is_empty() {
                    diagnostics.push("task_name must not be empty.".into());
                }
                let Some(team) = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                else {
                    diagnostics.push("create_task needs a target team.".into());
                    return Ok(reviewed_operation(operation, diagnostics));
                };
                if team.tasks.iter().any(|task| task.name == *task_name) {
                    diagnostics.push(format!(
                        "Team {} already defines task {}.",
                        team.name, task_name
                    ));
                }
            }
            BuilderConfigOperation::UpdateTask {
                task_name,
                team_name,
                ..
            } => {
                if task_name.trim().is_empty() {
                    diagnostics.push("task_name must not be empty.".into());
                }
                let Some(team) = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                else {
                    diagnostics.push("update_task needs a target team.".into());
                    return Ok(reviewed_operation(operation, diagnostics));
                };
                if !team.tasks.iter().any(|task| task.name == *task_name) {
                    diagnostics.push(format!(
                        "Team {} does not define task {}.",
                        team.name, task_name
                    ));
                }
            }
        }

        Ok(reviewed_operation(operation, diagnostics))
    }

    async fn apply_operation(
        &self,
        space_id: Uuid,
        operation: BuilderConfigOperation,
        session_id: Option<Uuid>,
        source: &str,
        request_text: Option<&str>,
    ) -> Result<BuilderAppliedOperation, GatewayError> {
        match operation {
            BuilderConfigOperation::CreateLens {
                id,
                title,
                summary,
                default_surface,
                ui_modules,
                prominent_team_ids,
            } => {
                let saved = self
                    .create_lens(
                        CreateLensRequest {
                            id,
                            title,
                            summary,
                            default_surface,
                            ui_modules,
                            prominent_team_ids,
                        },
                        Some(space_id),
                        session_id,
                        source,
                    )
                    .await?;
                self.record_request_text(request_text, "lens", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "create_lens".into(),
                    summary: format!("Created lens {}.", saved.title),
                    entity_kind: "lens".into(),
                    entity_id: saved.id.to_string(),
                })
            }
            BuilderConfigOperation::UpdateLens {
                lens_id,
                title,
                summary,
                default_surface,
                ui_modules,
                prominent_team_ids,
            } => {
                let saved = self
                    .update_lens(
                        &lens_id,
                        UpdateLensRequest {
                            title,
                            summary,
                            default_surface,
                            ui_modules,
                            prominent_team_ids,
                        },
                        Some(space_id),
                        session_id,
                        source,
                    )
                    .await?;
                self.record_request_text(request_text, "lens", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "update_lens".into(),
                    summary: format!("Updated lens {}.", saved.title),
                    entity_kind: "lens".into(),
                    entity_id: saved.id.to_string(),
                })
            }
            BuilderConfigOperation::InstallLensPreset { preset_id } => {
                let saved = self
                    .install_lens_preset(
                        InstallLensRequest { preset_id },
                        Some(space_id),
                        session_id,
                        source,
                    )
                    .await?;
                self.record_request_text(request_text, "lens", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "install_lens_preset".into(),
                    summary: format!("Installed preset lens {}.", saved.title),
                    entity_kind: "lens".into(),
                    entity_id: saved.id.to_string(),
                })
            }
            BuilderConfigOperation::CreateTeam {
                name,
                description,
                entry_agent_id,
                members,
            } => {
                let saved = self
                    .create_team(
                        space_id,
                        CreateTeamRequest {
                            name,
                            description,
                            entry_agent_id: entry_agent_id.clone(),
                            agents: members_to_team_agents(&entry_agent_id, members),
                            tasks: Vec::new(),
                        },
                        session_id,
                        source,
                    )
                    .await?;
                self.record_request_text(request_text, "team", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "create_team".into(),
                    summary: format!("Created team {}.", saved.name),
                    entity_kind: "team".into(),
                    entity_id: saved.id.to_string(),
                })
            }
            BuilderConfigOperation::UpdateTeam {
                team_id,
                team_name,
                name,
                description,
                entry_agent_id,
                members,
            } => {
                let team = self
                    .resolve_team(space_id, team_id, team_name.as_deref())
                    .await?
                    .ok_or_else(|| {
                        GatewayError::ValidationError(
                            "The referenced team does not exist in this space.".into(),
                        )
                    })?;
                let saved = self
                    .update_team(
                        team.id,
                        UpdateTeamRequest {
                            name,
                            description,
                            entry_agent_id: entry_agent_id.clone(),
                            agents: team_update_agents(entry_agent_id, members),
                            tasks: None,
                            members: None,
                        },
                        session_id,
                        source,
                    )
                    .await?;
                self.record_request_text(request_text, "team", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "update_team".into(),
                    summary: format!("Updated team {}.", saved.name),
                    entity_kind: "team".into(),
                    entity_id: saved.id.to_string(),
                })
            }
            BuilderConfigOperation::CreateSpaceExtension {
                name,
                display_name,
                description,
                requested_tools,
                provided_ui_modules,
                provider_integrations,
                tool,
                instructions,
            } => {
                let saved = self
                    .create_space_extension(
                        space_id,
                        CreateSpaceExtensionRequest {
                            name,
                            display_name,
                            description,
                            requested_tools,
                            provided_ui_modules,
                            provider_integrations,
                            tool,
                            instructions,
                        },
                        session_id,
                        source,
                    )
                    .await?;
                self.record_request_text(
                    request_text,
                    "space_extension",
                    saved.name.clone(),
                    session_id,
                )
                .await?;
                Ok(BuilderAppliedOperation {
                    kind: "create_space_extension".into(),
                    summary: format!("Created extension {}.", saved.name),
                    entity_kind: "space_extension".into(),
                    entity_id: saved.name,
                })
            }
            BuilderConfigOperation::UpdateSpaceExtension {
                extension_name,
                display_name,
                description,
                requested_tools,
                provided_ui_modules,
                provider_integrations,
                tool,
                instructions,
            } => {
                let saved = self
                    .update_space_extension(
                        space_id,
                        &extension_name,
                        UpdateSpaceExtensionRequest {
                            display_name,
                            description,
                            requested_tools,
                            provided_ui_modules,
                            provider_integrations,
                            tool,
                            instructions,
                        },
                        session_id,
                        source,
                    )
                    .await?;
                self.record_request_text(
                    request_text,
                    "space_extension",
                    saved.name.clone(),
                    session_id,
                )
                .await?;
                Ok(BuilderAppliedOperation {
                    kind: "update_space_extension".into(),
                    summary: format!("Updated extension {}.", saved.name),
                    entity_kind: "space_extension".into(),
                    entity_id: saved.name,
                })
            }
            BuilderConfigOperation::ApproveWorkspaceExtension { extension_name } => {
                let saved = self
                    .approve_workspace_extension(space_id, &extension_name, session_id, source)
                    .await?;
                self.record_request_text(
                    request_text,
                    "space_extension",
                    saved.name.clone(),
                    session_id,
                )
                .await?;
                Ok(BuilderAppliedOperation {
                    kind: "approve_workspace_extension".into(),
                    summary: format!("Approved workspace extension {}.", saved.name),
                    entity_kind: "space_extension".into(),
                    entity_id: saved.name,
                })
            }
            BuilderConfigOperation::CreateAgent {
                agent_id,
                team_name,
                display_name,
                description,
                instructions,
                model,
            } => {
                let team = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                    .ok_or_else(|| {
                        GatewayError::ValidationError("create_agent needs a target team.".into())
                    })?;
                let before = capture_team_agent_from_storage(&team, &agent_id)
                    .map(|snapshot| {
                        serde_json::to_value(snapshot)
                            .map_err(|error| GatewayError::ValidationError(error.to_string()))
                    })
                    .transpose()?;
                let mut agents = team
                    .agents
                    .iter()
                    .map(team_agent_to_payload)
                    .collect::<Vec<_>>();
                agents.push(TeamAgentPayload {
                    id: agent_id.clone(),
                    display_name: display_name.unwrap_or_else(|| agent_id.clone()),
                    description: description.unwrap_or_default(),
                    instructions: instructions.unwrap_or_default(),
                    model,
                });
                let saved = self
                    .update_team(
                        team.id,
                        UpdateTeamRequest {
                            name: None,
                            description: None,
                            entry_agent_id: None,
                            agents: Some(agents),
                            tasks: None,
                            members: None,
                        },
                        session_id,
                        source,
                    )
                    .await?;
                let agent_snapshot = capture_team_agent_from_response(&saved, &agent_id)
                    .ok_or_else(|| {
                        GatewayError::ValidationError("created agent was not found".into())
                    })?;
                self.record_configuration_change(
                    "team_agent",
                    format!("{}:{agent_id}", saved.id),
                    "create_agent",
                    source,
                    Some(saved.space_id),
                    session_id,
                    before,
                    serde_json::to_value(agent_snapshot)
                        .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
                    None,
                )
                .await?;
                self.record_request_text(request_text, "team", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "create_agent".into(),
                    summary: format!("Created agent {} in team {}.", agent_id, saved.name),
                    entity_kind: "team_agent".into(),
                    entity_id: format!("{}:{agent_id}", saved.id),
                })
            }
            BuilderConfigOperation::UpdateAgent {
                agent_id,
                team_name,
                display_name,
                description,
                instructions,
                model,
            } => {
                let team = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                    .ok_or_else(|| {
                        GatewayError::ValidationError("update_agent needs a target team.".into())
                    })?;
                let before = capture_team_agent_from_storage(&team, &agent_id)
                    .map(|snapshot| {
                        serde_json::to_value(snapshot)
                            .map_err(|error| GatewayError::ValidationError(error.to_string()))
                    })
                    .transpose()?;
                let agents = team
                    .agents
                    .iter()
                    .map(|agent| {
                        if agent.id == agent_id {
                            TeamAgentPayload {
                                id: agent.id.clone(),
                                display_name: display_name
                                    .clone()
                                    .unwrap_or_else(|| agent.display_name.clone()),
                                description: description
                                    .clone()
                                    .unwrap_or_else(|| agent.description.clone()),
                                instructions: instructions
                                    .clone()
                                    .unwrap_or_else(|| agent.instructions.clone()),
                                model: model.clone().or_else(|| agent.model.clone()),
                            }
                        } else {
                            team_agent_to_payload(agent)
                        }
                    })
                    .collect::<Vec<_>>();
                let saved = self
                    .update_team(
                        team.id,
                        UpdateTeamRequest {
                            name: None,
                            description: None,
                            entry_agent_id: None,
                            agents: Some(agents),
                            tasks: None,
                            members: None,
                        },
                        session_id,
                        source,
                    )
                    .await?;
                let agent_snapshot = capture_team_agent_from_response(&saved, &agent_id)
                    .ok_or_else(|| {
                        GatewayError::ValidationError("updated agent was not found".into())
                    })?;
                self.record_configuration_change(
                    "team_agent",
                    format!("{}:{agent_id}", saved.id),
                    "update_agent",
                    source,
                    Some(saved.space_id),
                    session_id,
                    before,
                    serde_json::to_value(agent_snapshot)
                        .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
                    None,
                )
                .await?;
                self.record_request_text(request_text, "team", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "update_agent".into(),
                    summary: format!("Updated agent {} in team {}.", agent_id, saved.name),
                    entity_kind: "team_agent".into(),
                    entity_id: format!("{}:{agent_id}", saved.id),
                })
            }
            BuilderConfigOperation::CreateTask {
                task_name,
                team_name,
                description,
                instructions,
                model,
                output_schema,
            } => {
                let team = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                    .ok_or_else(|| {
                        GatewayError::ValidationError("create_task needs a target team.".into())
                    })?;
                let before = capture_team_task_from_storage(&team, &task_name)
                    .map(|snapshot| {
                        serde_json::to_value(snapshot)
                            .map_err(|error| GatewayError::ValidationError(error.to_string()))
                    })
                    .transpose()?;
                let mut tasks = team
                    .tasks
                    .iter()
                    .map(team_task_to_payload)
                    .collect::<Vec<_>>();
                tasks.push(TeamTaskPayload {
                    name: task_name.clone(),
                    description: description.unwrap_or_default(),
                    instructions: instructions.unwrap_or_default(),
                    model,
                    output_schema,
                    max_output_retries: None,
                    tool_allow: None,
                    tool_deny: None,
                });
                let saved = self
                    .update_team(
                        team.id,
                        UpdateTeamRequest {
                            name: None,
                            description: None,
                            entry_agent_id: None,
                            agents: None,
                            tasks: Some(tasks),
                            members: None,
                        },
                        session_id,
                        source,
                    )
                    .await?;
                let task_snapshot = capture_team_task_from_response(&saved, &task_name)
                    .ok_or_else(|| {
                        GatewayError::ValidationError("created task was not found".into())
                    })?;
                self.record_configuration_change(
                    "team_task",
                    format!("{}:{task_name}", saved.id),
                    "create_task",
                    source,
                    Some(saved.space_id),
                    session_id,
                    before,
                    serde_json::to_value(task_snapshot)
                        .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
                    None,
                )
                .await?;
                self.record_request_text(request_text, "team", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "create_task".into(),
                    summary: format!("Created task {} in team {}.", task_name, saved.name),
                    entity_kind: "team_task".into(),
                    entity_id: format!("{}:{task_name}", saved.id),
                })
            }
            BuilderConfigOperation::UpdateTask {
                task_name,
                team_name,
                description,
                instructions,
                model,
                output_schema,
            } => {
                let team = self
                    .resolve_target_team(space_id, team_name.as_deref())
                    .await?
                    .ok_or_else(|| {
                        GatewayError::ValidationError("update_task needs a target team.".into())
                    })?;
                let before = capture_team_task_from_storage(&team, &task_name)
                    .map(|snapshot| {
                        serde_json::to_value(snapshot)
                            .map_err(|error| GatewayError::ValidationError(error.to_string()))
                    })
                    .transpose()?;
                let tasks = team
                    .tasks
                    .iter()
                    .map(|task| {
                        if task.name == task_name {
                            TeamTaskPayload {
                                name: task.name.clone(),
                                description: description
                                    .clone()
                                    .unwrap_or_else(|| task.description.clone()),
                                instructions: instructions
                                    .clone()
                                    .unwrap_or_else(|| task.instructions.clone()),
                                model: model.clone().or_else(|| task.model.clone()),
                                output_schema: output_schema
                                    .clone()
                                    .or_else(|| task.output_schema.clone()),
                                max_output_retries: task.max_output_retries,
                                tool_allow: task.tool_allow.clone(),
                                tool_deny: task.tool_deny.clone(),
                            }
                        } else {
                            team_task_to_payload(task)
                        }
                    })
                    .collect::<Vec<_>>();
                let saved = self
                    .update_team(
                        team.id,
                        UpdateTeamRequest {
                            name: None,
                            description: None,
                            entry_agent_id: None,
                            agents: None,
                            tasks: Some(tasks),
                            members: None,
                        },
                        session_id,
                        source,
                    )
                    .await?;
                let task_snapshot = capture_team_task_from_response(&saved, &task_name)
                    .ok_or_else(|| {
                        GatewayError::ValidationError("updated task was not found".into())
                    })?;
                self.record_configuration_change(
                    "team_task",
                    format!("{}:{task_name}", saved.id),
                    "update_task",
                    source,
                    Some(saved.space_id),
                    session_id,
                    before,
                    serde_json::to_value(task_snapshot)
                        .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
                    None,
                )
                .await?;
                self.record_request_text(request_text, "team", saved.id.to_string(), session_id)
                    .await?;
                Ok(BuilderAppliedOperation {
                    kind: "update_task".into(),
                    summary: format!("Updated task {} in team {}.", task_name, saved.name),
                    entity_kind: "team_task".into(),
                    entity_id: format!("{}:{task_name}", saved.id),
                })
            }
        }
    }

    async fn resolve_team(
        &self,
        space_id: Uuid,
        team_id: Option<Uuid>,
        team_name: Option<&str>,
    ) -> Result<Option<swage_storage::Team>, GatewayError> {
        if let Some(team_id) = team_id {
            let team = self.state.storage.get_team(team_id).await?;
            return Ok(team.filter(|team| team.space_id == space_id));
        }

        if let Some(team_name) = team_name {
            return Ok(self
                .state
                .storage
                .get_team_by_name(space_id, team_name)
                .await?);
        }

        Ok(None)
    }

    async fn resolve_target_team(
        &self,
        space_id: Uuid,
        team_name: Option<&str>,
    ) -> Result<Option<swage_storage::Team>, GatewayError> {
        if let Some(team) = self.resolve_team(space_id, None, team_name).await? {
            return Ok(Some(team));
        }

        Ok(self
            .state
            .storage
            .list_teams_by_space(space_id)
            .await?
            .into_iter()
            .next())
    }

    async fn capture_lens_snapshot(
        &self,
        lens_id: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let lens_id = swage_storage::AppLens::new(lens_id.to_string())
            .map_err(GatewayError::ValidationError)?;
        let lens = self.state.storage.get_lens(&lens_id).await?;
        lens.map(|lens| {
            serde_json::to_value(crate::lens::LensDescriptor {
                id: lens.id,
                title: lens.title,
                summary: lens.summary,
                default_surface: lens.default_surface,
                ui_modules: lens.ui_modules,
                prominent_team_ids: lens.prominent_team_ids,
                source_kind: lens.source_kind.to_string(),
            })
            .map_err(|error| GatewayError::ValidationError(error.to_string()))
        })
        .transpose()
    }

    async fn capture_lens_preset_target(
        &self,
        preset_id: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let preset = crate::lens::preset_lens(preset_id)?;
        self.capture_lens_snapshot(preset.id.as_str()).await
    }

    async fn capture_team_snapshot(
        &self,
        team_id: Uuid,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let team = self.state.storage.get_team(team_id).await?;
        team.map(|team| {
            serde_json::to_value(team_to_version_response(team))
                .map_err(|error| GatewayError::ValidationError(error.to_string()))
        })
        .transpose()
    }

    async fn capture_space_extension_snapshot(
        &self,
        space_id: Uuid,
        extension_name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let extensions = space_extension_api::list_space_extensions(&self.state, space_id)
            .await?
            .extensions;
        extensions
            .into_iter()
            .find(|extension| {
                extension.name == extension_name && extension.source_kind == "managed"
            })
            .map(|extension| {
                serde_json::to_value(extension)
                    .map_err(|error| GatewayError::ValidationError(error.to_string()))
            })
            .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_configuration_change(
        &self,
        entity_kind: &str,
        entity_id: String,
        operation_kind: &str,
        source: &str,
        space_id: Option<Uuid>,
        session_id: Option<Uuid>,
        before_snapshot: Option<serde_json::Value>,
        snapshot: serde_json::Value,
        rollback_to_version_id: Option<Uuid>,
    ) -> Result<ConfigurationVersion, GatewayError> {
        let previous_version = self
            .state
            .storage
            .get_latest_configuration_version(entity_kind, &entity_id)
            .await?;
        let diff = diff_json_values(before_snapshot.as_ref(), &snapshot);
        let version = self
            .state
            .storage
            .create_configuration_version(NewConfigurationVersion {
                id: Uuid::new_v4(),
                space_id,
                session_id,
                entity_kind: entity_kind.to_string(),
                entity_id: entity_id.clone(),
                version_number: previous_version
                    .as_ref()
                    .map(|version| version.version_number + 1)
                    .unwrap_or(1),
                operation_kind: operation_kind.to_string(),
                source: source.to_string(),
                summary: format!("{operation_kind} via {source}"),
                snapshot,
                diff,
                previous_version_id: previous_version.as_ref().map(|version| version.id),
                rollback_to_version_id,
            })
            .await?;
        self.record_audit(
            entity_kind,
            entity_id,
            operation_kind,
            source,
            session_id,
            space_id,
            Some(version.id),
            rollback_to_version_id,
        )
        .await?;
        Ok(version)
    }

    async fn rollback_lens_version(
        &self,
        version: &ConfigurationVersion,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<ConfigurationVersion, GatewayError> {
        let snapshot: LensDescriptor = serde_json::from_value(version.snapshot.clone())
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
        let before = self.capture_lens_snapshot(snapshot.id.as_str()).await?;
        let request = UpdateLensRequest {
            title: snapshot.title.clone(),
            summary: snapshot.summary.clone(),
            default_surface: snapshot.default_surface.clone(),
            ui_modules: snapshot.ui_modules.clone(),
            prominent_team_ids: snapshot.prominent_team_ids.clone(),
        };
        let restored = if before.is_some() {
            crate::lens::update_lens(&self.state, snapshot.id.as_str(), request).await?
        } else {
            crate::lens::create_lens(
                &self.state,
                CreateLensRequest {
                    id: snapshot.id.to_string(),
                    title: snapshot.title.clone(),
                    summary: snapshot.summary.clone(),
                    default_surface: snapshot.default_surface.clone(),
                    ui_modules: snapshot.ui_modules.clone(),
                    prominent_team_ids: snapshot.prominent_team_ids.clone(),
                },
            )
            .await?
        };
        self.record_configuration_change(
            "lens",
            restored.id.to_string(),
            "rollback_configuration_version",
            source,
            version.space_id,
            session_id,
            before,
            serde_json::to_value(&restored)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            Some(version.id),
        )
        .await
    }

    async fn rollback_team_version(
        &self,
        version: &ConfigurationVersion,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<ConfigurationVersion, GatewayError> {
        let snapshot: TeamResponse = serde_json::from_value(version.snapshot.clone())
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
        let before = self.capture_team_snapshot(snapshot.id).await?;
        let restored = if before.is_some() {
            TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
                .update_team(
                    snapshot.id,
                    UpdateTeamRequest {
                        name: Some(snapshot.name.clone()),
                        description: Some(snapshot.description.clone()),
                        entry_agent_id: Some(snapshot.entry_agent_id.clone()),
                        agents: Some(snapshot.agents.clone()),
                        tasks: Some(snapshot.tasks.clone()),
                        members: None,
                    },
                )
                .await?
        } else {
            TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
                .create_team(
                    snapshot.space_id,
                    CreateTeamRequest {
                        name: snapshot.name.clone(),
                        description: snapshot.description.clone(),
                        entry_agent_id: snapshot.entry_agent_id.clone(),
                        agents: snapshot.agents.clone(),
                        tasks: snapshot.tasks.clone(),
                    },
                )
                .await?
        };
        self.record_configuration_change(
            "team",
            restored.id.to_string(),
            "rollback_configuration_version",
            source,
            Some(restored.space_id),
            session_id,
            before,
            serde_json::to_value(&restored)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            Some(version.id),
        )
        .await
    }

    async fn rollback_team_agent_version(
        &self,
        version: &ConfigurationVersion,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<ConfigurationVersion, GatewayError> {
        let snapshot: TeamAgentVersionSnapshot =
            serde_json::from_value(version.snapshot.clone())
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
        let before = self
            .capture_team_agent_snapshot(snapshot.team_id, &snapshot.agent.id)
            .await?;
        let current_team =
            TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
                .get_team(snapshot.team_id)
                .await?;
        let mut agents = current_team.agents.clone();
        if let Some(existing) = agents
            .iter_mut()
            .find(|agent| agent.id == snapshot.agent.id)
        {
            *existing = snapshot.agent.clone();
        } else {
            agents.push(snapshot.agent.clone());
        }
        let restored_team =
            TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
                .update_team(
                    snapshot.team_id,
                    UpdateTeamRequest {
                        name: None,
                        description: None,
                        entry_agent_id: None,
                        agents: Some(agents),
                        tasks: None,
                        members: None,
                    },
                )
                .await?;
        let after = capture_team_agent_from_response(&restored_team, &snapshot.agent.id)
            .ok_or_else(|| {
                GatewayError::ValidationError("restored team agent was not found".into())
            })?;
        self.record_configuration_change(
            "team_agent",
            format!("{}:{}", snapshot.team_id, snapshot.agent.id),
            "rollback_configuration_version",
            source,
            Some(restored_team.space_id),
            session_id,
            before,
            serde_json::to_value(after)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            Some(version.id),
        )
        .await
    }

    async fn rollback_team_task_version(
        &self,
        version: &ConfigurationVersion,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<ConfigurationVersion, GatewayError> {
        let snapshot: TeamTaskVersionSnapshot = serde_json::from_value(version.snapshot.clone())
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
        let before = self
            .capture_team_task_snapshot(snapshot.team_id, &snapshot.task.name)
            .await?;
        let current_team =
            TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
                .get_team(snapshot.team_id)
                .await?;
        let mut tasks = current_team.tasks.clone();
        if let Some(existing) = tasks
            .iter_mut()
            .find(|task| task.name == snapshot.task.name)
        {
            *existing = snapshot.task.clone();
        } else {
            tasks.push(snapshot.task.clone());
        }
        let restored_team =
            TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
                .update_team(
                    snapshot.team_id,
                    UpdateTeamRequest {
                        name: None,
                        description: None,
                        entry_agent_id: None,
                        agents: None,
                        tasks: Some(tasks),
                        members: None,
                    },
                )
                .await?;
        let after = capture_team_task_from_response(&restored_team, &snapshot.task.name)
            .ok_or_else(|| {
                GatewayError::ValidationError("restored team task was not found".into())
            })?;
        self.record_configuration_change(
            "team_task",
            format!("{}:{}", snapshot.team_id, snapshot.task.name),
            "rollback_configuration_version",
            source,
            Some(restored_team.space_id),
            session_id,
            before,
            serde_json::to_value(after)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            Some(version.id),
        )
        .await
    }

    async fn rollback_space_extension_version(
        &self,
        version: &ConfigurationVersion,
        session_id: Option<Uuid>,
        source: &str,
    ) -> Result<ConfigurationVersion, GatewayError> {
        let space_id = version.space_id.ok_or_else(|| {
            GatewayError::ValidationError("space extension version is missing space_id".into())
        })?;
        let snapshot: SpaceExtensionResponse = serde_json::from_value(version.snapshot.clone())
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
        let before = self
            .capture_space_extension_snapshot(space_id, &snapshot.name)
            .await?;
        let restored = if before.is_some() {
            space_extension_api::update_space_extension(
                &self.state,
                space_id,
                &snapshot.name,
                UpdateSpaceExtensionRequest {
                    display_name: snapshot.display_name.clone(),
                    description: snapshot.description.clone(),
                    requested_tools: snapshot.requested_tools.clone(),
                    provided_ui_modules: snapshot.provided_ui_modules.clone(),
                    provider_integrations: snapshot.provider_integrations.clone(),
                    tool: snapshot.tool.clone(),
                    instructions: snapshot.instructions.clone(),
                },
            )
            .await?
        } else {
            space_extension_api::create_space_extension(
                &self.state,
                space_id,
                CreateSpaceExtensionRequest {
                    name: snapshot.name.clone(),
                    display_name: snapshot.display_name.clone(),
                    description: snapshot.description.clone(),
                    requested_tools: snapshot.requested_tools.clone(),
                    provided_ui_modules: snapshot.provided_ui_modules.clone(),
                    provider_integrations: snapshot.provider_integrations.clone(),
                    tool: snapshot.tool.clone(),
                    instructions: snapshot.instructions.clone(),
                },
            )
            .await?
        };
        self.record_configuration_change(
            "space_extension",
            restored.name.clone(),
            "rollback_configuration_version",
            source,
            Some(space_id),
            session_id,
            before,
            serde_json::to_value(&restored)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
            Some(version.id),
        )
        .await
    }

    async fn capture_team_agent_snapshot(
        &self,
        team_id: Uuid,
        agent_id: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let team = TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
            .get_team(team_id)
            .await?;
        capture_team_agent_from_response(&team, agent_id)
            .map(|snapshot| {
                serde_json::to_value(snapshot)
                    .map_err(|error| GatewayError::ValidationError(error.to_string()))
            })
            .transpose()
    }

    async fn capture_team_task_snapshot(
        &self,
        team_id: Uuid,
        task_name: &str,
    ) -> Result<Option<serde_json::Value>, GatewayError> {
        let team = TeamService::new(self.state.storage.clone(), self.state.space_store.clone())
            .get_team(team_id)
            .await?;
        capture_team_task_from_response(&team, task_name)
            .map(|snapshot| {
                serde_json::to_value(snapshot)
                    .map_err(|error| GatewayError::ValidationError(error.to_string()))
            })
            .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_audit(
        &self,
        entity_kind: &str,
        entity_id: String,
        operation_kind: &str,
        source: &str,
        session_id: Option<Uuid>,
        space_id: Option<Uuid>,
        configuration_version_id: Option<Uuid>,
        rollback_to_version_id: Option<Uuid>,
    ) -> Result<(), GatewayError> {
        self.state
            .storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id,
                entity_kind: entity_kind.to_string(),
                entity_id,
                event_kind: "builder_config_mutation".into(),
                summary: format!("{operation_kind} via {source}"),
                metadata: json!({
                    "service": BUILDER_CONFIG_SERVICE_NAME,
                    "operation_kind": operation_kind,
                    "source": source,
                    "space_id": space_id,
                    "configuration_version_id": configuration_version_id,
                    "rollback_to_version_id": rollback_to_version_id,
                }),
            })
            .await?;
        Ok(())
    }

    async fn record_request_text(
        &self,
        request_text: Option<&str>,
        entity_kind: &str,
        entity_id: String,
        session_id: Option<Uuid>,
    ) -> Result<(), GatewayError> {
        let Some(request_text) = request_text else {
            return Ok(());
        };
        if request_text.trim().is_empty() {
            return Ok(());
        }
        self.state
            .storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id,
                entity_kind: entity_kind.to_string(),
                entity_id,
                event_kind: "builder_request_text".into(),
                summary: "conversational builder request".into(),
                metadata: json!({
                    "service": BUILDER_CONFIG_SERVICE_NAME,
                    "request_text": request_text,
                }),
            })
            .await?;
        Ok(())
    }
}

fn members_to_team_agents(entry_agent_id: &str, members: Vec<String>) -> Vec<TeamAgentPayload> {
    let mut agent_ids = Vec::with_capacity(members.len() + 1);
    agent_ids.push(entry_agent_id.to_string());
    for member in members {
        if !agent_ids.iter().any(|existing| existing == &member) {
            agent_ids.push(member);
        }
    }

    agent_ids
        .into_iter()
        .map(|agent_id| TeamAgentPayload {
            display_name: agent_id.clone(),
            id: agent_id,
            description: String::new(),
            instructions: String::new(),
            model: None,
        })
        .collect()
}

fn team_update_agents(
    entry_agent_id: Option<String>,
    members: Option<Vec<String>>,
) -> Option<Vec<TeamAgentPayload>> {
    match (entry_agent_id, members) {
        (Some(entry_agent_id), Some(members)) => {
            Some(members_to_team_agents(&entry_agent_id, members))
        }
        (Some(entry_agent_id), None) => Some(members_to_team_agents(&entry_agent_id, Vec::new())),
        (None, Some(members)) => {
            let fallback_entry = members
                .first()
                .cloned()
                .unwrap_or_else(|| "default".to_string());
            Some(members_to_team_agents(&fallback_entry, members))
        }
        (None, None) => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamAgentVersionSnapshot {
    team_id: Uuid,
    team_name: String,
    agent: TeamAgentPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamTaskVersionSnapshot {
    team_id: Uuid,
    team_name: String,
    task: TeamTaskPayload,
}

fn configuration_version_to_response(
    version: ConfigurationVersion,
    is_current: bool,
) -> ConfigurationVersionResponse {
    ConfigurationVersionResponse {
        id: version.id,
        space_id: version.space_id,
        session_id: version.session_id,
        entity_kind: version.entity_kind,
        entity_id: version.entity_id,
        version_number: version.version_number,
        operation_kind: version.operation_kind,
        source: version.source,
        summary: version.summary,
        snapshot: version.snapshot,
        diff: version.diff,
        previous_version_id: version.previous_version_id,
        rollback_to_version_id: version.rollback_to_version_id,
        created_at: version.created_at.to_rfc3339(),
        is_current,
    }
}

fn team_to_version_response(team: swage_storage::Team) -> TeamResponse {
    TeamResponse {
        id: team.id,
        space_id: team.space_id,
        name: team.name,
        description: team.description,
        entry_agent_id: team.entry_agent_id,
        agents: team.agents.iter().map(team_agent_to_payload).collect(),
        tasks: team.tasks.iter().map(team_task_to_payload).collect(),
        members: team.members,
        created_at: team.created_at,
        updated_at: team.updated_at,
    }
}

fn capture_team_agent_from_storage(
    team: &swage_storage::Team,
    agent_id: &str,
) -> Option<TeamAgentVersionSnapshot> {
    team.agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .map(|agent| TeamAgentVersionSnapshot {
            team_id: team.id,
            team_name: team.name.clone(),
            agent: team_agent_to_payload(agent),
        })
}

fn capture_team_agent_from_response(
    team: &TeamResponse,
    agent_id: &str,
) -> Option<TeamAgentVersionSnapshot> {
    team.agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .cloned()
        .map(|agent| TeamAgentVersionSnapshot {
            team_id: team.id,
            team_name: team.name.clone(),
            agent,
        })
}

fn capture_team_task_from_storage(
    team: &swage_storage::Team,
    task_name: &str,
) -> Option<TeamTaskVersionSnapshot> {
    team.tasks
        .iter()
        .find(|task| task.name == task_name)
        .map(|task| TeamTaskVersionSnapshot {
            team_id: team.id,
            team_name: team.name.clone(),
            task: team_task_to_payload(task),
        })
}

fn capture_team_task_from_response(
    team: &TeamResponse,
    task_name: &str,
) -> Option<TeamTaskVersionSnapshot> {
    team.tasks
        .iter()
        .find(|task| task.name == task_name)
        .cloned()
        .map(|task| TeamTaskVersionSnapshot {
            team_id: team.id,
            team_name: team.name.clone(),
            task,
        })
}

fn diff_json_values(
    before: Option<&serde_json::Value>,
    after: &serde_json::Value,
) -> serde_json::Value {
    let null = serde_json::Value::Null;
    let before = before.unwrap_or(&null);
    let mut changes = Vec::new();
    collect_json_diff(before, after, "", &mut changes);
    serde_json::Value::Array(changes)
}

fn collect_json_diff(
    before: &serde_json::Value,
    after: &serde_json::Value,
    path: &str,
    changes: &mut Vec<serde_json::Value>,
) {
    match (before, after) {
        (serde_json::Value::Object(before_map), serde_json::Value::Object(after_map)) => {
            let mut keys = before_map
                .keys()
                .chain(after_map.keys())
                .cloned()
                .collect::<Vec<_>>();
            keys.sort();
            keys.dedup();
            for key in keys {
                let next_path = format!("{path}/{key}");
                collect_json_diff(
                    before_map.get(&key).unwrap_or(&serde_json::Value::Null),
                    after_map.get(&key).unwrap_or(&serde_json::Value::Null),
                    &next_path,
                    changes,
                );
            }
        }
        _ if before == after => {}
        _ => changes.push(json!({
            "path": if path.is_empty() { "/" } else { path },
            "before": before,
            "after": after,
        })),
    }
}

fn reviewed_operation(
    operation: BuilderConfigOperation,
    diagnostics: Vec<String>,
) -> BuilderReviewedOperation {
    BuilderReviewedOperation {
        kind: operation.kind().to_string(),
        summary: operation.summary(),
        ready: diagnostics.is_empty(),
        diagnostics,
        operation,
    }
}

fn team_agent_to_payload(agent: &swage_storage::model::TeamAgentDefinition) -> TeamAgentPayload {
    TeamAgentPayload {
        id: agent.id.clone(),
        display_name: agent.display_name.clone(),
        description: agent.description.clone(),
        instructions: agent.instructions.clone(),
        model: agent.model.clone(),
    }
}

fn team_task_to_payload(task: &swage_storage::model::TeamTaskDefinition) -> TeamTaskPayload {
    TeamTaskPayload {
        name: task.name.clone(),
        description: task.description.clone(),
        instructions: task.instructions.clone(),
        model: task.model.clone(),
        output_schema: task.output_schema.clone(),
        max_output_retries: task.max_output_retries,
        tool_allow: task.tool_allow.clone(),
        tool_deny: task.tool_deny.clone(),
    }
}

impl BuilderConfigOperation {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CreateLens { .. } => "create_lens",
            Self::UpdateLens { .. } => "update_lens",
            Self::InstallLensPreset { .. } => "install_lens_preset",
            Self::CreateTeam { .. } => "create_team",
            Self::UpdateTeam { .. } => "update_team",
            Self::CreateSpaceExtension { .. } => "create_space_extension",
            Self::UpdateSpaceExtension { .. } => "update_space_extension",
            Self::ApproveWorkspaceExtension { .. } => "approve_workspace_extension",
            Self::CreateAgent { .. } => "create_agent",
            Self::UpdateAgent { .. } => "update_agent",
            Self::CreateTask { .. } => "create_task",
            Self::UpdateTask { .. } => "update_task",
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::CreateLens { title, id, .. } => format!("Create lens {title} ({id})."),
            Self::UpdateLens { lens_id, .. } => format!("Update lens {lens_id}."),
            Self::InstallLensPreset { preset_id } => {
                format!("Install lens preset {preset_id}.")
            }
            Self::CreateTeam { name, .. } => format!("Create team {name}."),
            Self::UpdateTeam {
                team_id, team_name, ..
            } => format!(
                "Update team {}.",
                team_name
                    .clone()
                    .unwrap_or_else(|| team_id.map(|value| value.to_string()).unwrap_or_default())
            ),
            Self::CreateSpaceExtension { name, .. } => {
                format!("Create space extension {name}.")
            }
            Self::UpdateSpaceExtension { extension_name, .. } => {
                format!("Update space extension {extension_name}.")
            }
            Self::ApproveWorkspaceExtension { extension_name } => {
                format!("Approve workspace extension {extension_name}.")
            }
            Self::CreateAgent { agent_id, .. } => format!("Create agent {agent_id}."),
            Self::UpdateAgent { agent_id, .. } => format!("Update agent {agent_id}."),
            Self::CreateTask { task_name, .. } => format!("Create task {task_name}."),
            Self::UpdateTask { task_name, .. } => format!("Update task {task_name}."),
        }
    }
}

fn parse_builder_output(content: &str) -> Result<BuilderPlannerOutput, GatewayError> {
    let trimmed = content.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim();
    let trimmed = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();
    serde_json::from_str(trimmed).map_err(|error| {
        GatewayError::ValidationError(format!("builder proposal was not valid JSON: {error}"))
    })
}

//! Security settings read and written per scope — the install, or one
//! topic — through the settings surface (C69).
//!
//! Every shipped security check is on by default. A scope relaxes a check by
//! setting it in its own `config.toml`: the install's at the data root, a
//! topic's in its data directory, the same files the config cascade reads.
//! What a session in a scope runs under is the cascade's result, so a topic
//! that says nothing inherits the install's choice, and the install inherits
//! the shipped default. Reading a scope returns all three views at once —
//! the effective checks, the scope's own overrides, and each check standing
//! below the shipped default with the scope that relaxed it — so the topic
//! settings page, the install's Settings and a session's view all say the
//! same thing from the same reading.
//!
//! A write replaces the scope's own security overrides whole, records the
//! change in the audit trail with the prior and new readings, and takes
//! effect for the next turn: the install's active configuration is updated
//! in place, and a topic's file is read by every turn that runs under it.
//! No write here touches tool grants: the allow and deny lists are the
//! security model, not a relaxable check.

use std::path::{Path, PathBuf};

use swage_config::RESERVED_WORKFLOW_AGENT_ID;
use swage_core::ScopeLocation;
use swage_protocol::client::access::AccessGateMode;
use swage_protocol::client::security_settings::{
    EffectiveSecurityChecks, RelaxedCheck, SaveSecuritySettingsRequest, SecurityCheck,
    SecurityOverrides, SecuritySettingsResponse, SettingsScope,
};
use swage_protocol::client::settings::RequireApprovalEntry;
use uuid::Uuid;

use crate::config_change::{ConfigChange, record_config_change};
use crate::error::ApiError;
use crate::state::AppState;

/// Read a scope's security settings: effective, its own overrides, and what
/// stands relaxed.
pub async fn read_security_settings(
    state: &AppState,
    scope: SettingsScope,
) -> Result<SecuritySettingsResponse, ApiError> {
    let resolved = ResolvedScope::resolve(state, &scope).await?;
    resolved.read(state).await
}

/// Replace a scope's security overrides, record the change, and return the
/// scope as it now reads.
pub async fn save_security_settings(
    state: &AppState,
    scope: SettingsScope,
    request: SaveSecuritySettingsRequest,
) -> Result<SecuritySettingsResponse, ApiError> {
    let resolved = ResolvedScope::resolve(state, &scope).await?;
    validate_overrides(&scope, &request.overrides)?;
    let prior = resolved.read(state).await?;

    let mut partial = read_partial(&resolved.config_path).await?;
    apply_overrides(&mut partial, &request.overrides)?;
    write_partial(&resolved.config_path, &partial).await?;

    if resolved.scope == SettingsScope::Install {
        apply_to_active_config(state, &partial);
    }

    let current = resolved.read(state).await?;
    record_config_change(
        state,
        ConfigChange {
            entity_kind: "security_settings",
            entity_id: &resolved.entity_id(),
            action: "save_security_settings",
            source: "operator_http",
            session_id: None,
            summary: &format!(
                "security settings saved for {}: {}",
                resolved.describe(),
                describe_relaxations(&current.relaxed)
            ),
            prior_state: Some(serde_json::to_value(&prior).map_err(snapshot_error)?),
            new_state: Some(serde_json::to_value(&current).map_err(snapshot_error)?),
        },
    )
    .await?;
    Ok(current)
}

/// The security settings a session runs under: its topic's scope, or the
/// install's for a session in the general scope.
pub async fn security_settings_for_session(
    state: &AppState,
    session_id: Uuid,
) -> Result<SecuritySettingsResponse, ApiError> {
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| ApiError::not_found("conversation", session_id))?;
    let topic = state
        .topic_store
        .get(session.topic_id)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| ApiError::not_found("topic", session.topic_id))?;
    let scope = if topic.is_general {
        SettingsScope::Install
    } else {
        SettingsScope::Topic { topic_id: topic.id }
    };
    read_security_settings(state, scope).await
}

/// A scope with its configuration file and location resolved.
struct ResolvedScope {
    scope: SettingsScope,
    location: ScopeLocation,
    config_path: PathBuf,
    /// The topic's name, for the change summary; absent for the install.
    topic_name: Option<String>,
}

impl ResolvedScope {
    async fn resolve(state: &AppState, scope: &SettingsScope) -> Result<Self, ApiError> {
        match scope {
            SettingsScope::Install => Ok(Self {
                scope: scope.clone(),
                location: ScopeLocation::General,
                config_path: state.config_root.join("config.toml"),
                topic_name: None,
            }),
            SettingsScope::Topic { topic_id } => {
                let topic = state
                    .topic_store
                    .get(*topic_id)
                    .await
                    .map_err(storage_error)?
                    .ok_or_else(|| ApiError::not_found("topic", *topic_id))?;
                if topic.is_general {
                    return Err(ApiError::validation(
                        "the general scope has no settings of its own; its sessions run under the install's".into(),
                    ));
                }
                let location = topic.location();
                Ok(Self {
                    scope: scope.clone(),
                    config_path: location.data_dir(&state.config_root).join("config.toml"),
                    location,
                    topic_name: Some(topic.name),
                })
            }
        }
    }

    fn entity_id(&self) -> String {
        match &self.scope {
            SettingsScope::Install => "install".to_owned(),
            SettingsScope::Topic { topic_id } => topic_id.to_string(),
        }
    }

    fn describe(&self) -> String {
        match &self.topic_name {
            None => "the install".to_owned(),
            Some(name) => format!("topic {name}"),
        }
    }

    /// The scope as it reads now: effective checks through the cascade, the
    /// scope's own file, and the relaxations each attributed to the scope
    /// whose setting caused it.
    async fn read(&self, state: &AppState) -> Result<SecuritySettingsResponse, ApiError> {
        let overrides = overrides_from_partial(&read_partial(&self.config_path).await?)?;
        let effective = effective_checks(state, &self.location)?;
        let install_overrides = match self.scope {
            SettingsScope::Install => overrides.clone(),
            SettingsScope::Topic { .. } => overrides_from_partial(
                &read_partial(&state.config_root.join("config.toml")).await?,
            )?,
        };
        let relaxed = relaxations(&effective, &self.scope, &overrides, &install_overrides);
        let dispatcher = state.active_tool_dispatcher();
        let mut available_tools: Vec<String> = dispatcher
            .registry()
            .list()
            .into_iter()
            .map(str::to_owned)
            .collect();
        available_tools.sort();
        Ok(SecuritySettingsResponse {
            available_tools,
            scope: self.scope.clone(),
            effective,
            overrides,
            relaxed,
        })
    }
}

/// The checks as the cascade resolves them for `location`, with no agent
/// overlay: the install's active configuration for the general scope, the
/// topic's file over it for a topic.
fn effective_checks(
    state: &AppState,
    location: &ScopeLocation,
) -> Result<EffectiveSecurityChecks, ApiError> {
    let config = if location.is_general() {
        state.active_config()
    } else {
        crate::turn::load_session_config_from_root(
            &state.config_root,
            state.active_config(),
            location,
            RESERVED_WORKFLOW_AGENT_ID,
        )
        .map_err(ApiError::from)?
    };
    Ok(EffectiveSecurityChecks {
        sandbox_enabled: config.sandbox.enabled,
        allow_degraded_execution: config.sandbox.allow_degraded_execution,
        require_approval: wire_entries(&config.tools.permissions.require_approval)?,
        access_gate: config.server.access,
    })
}

/// The shipped defaults every relaxation is measured against.
fn shipped_defaults() -> EffectiveSecurityChecks {
    let sandbox = swage_config::SandboxConfig::default();
    let permissions = swage_config::ToolPermissionConfig::default();
    EffectiveSecurityChecks {
        sandbox_enabled: sandbox.enabled,
        allow_degraded_execution: sandbox.allow_degraded_execution,
        require_approval: wire_entries(&permissions.require_approval)
            .expect("the shipped approval list is a valid wire list"),
        access_gate: swage_config::ServerConfig::default().access,
    }
}

/// Each check standing below the shipped default, attributed to the scope
/// whose own setting put it there: the topic when its override sets the
/// field, the install otherwise.
fn relaxations(
    effective: &EffectiveSecurityChecks,
    scope: &SettingsScope,
    scope_overrides: &SecurityOverrides,
    install_overrides: &SecurityOverrides,
) -> Vec<RelaxedCheck> {
    let defaults = shipped_defaults();
    let relaxed_by = |topic_sets: bool, install_sets: bool| -> SettingsScope {
        match scope {
            SettingsScope::Topic { .. } if topic_sets => scope.clone(),
            _ if install_sets => SettingsScope::Install,
            // Neither file sets it, yet it differs from the default: an agent
            // overlay or an older file shape; attribute to the install, the
            // scope whose operator can see every file.
            _ => SettingsScope::Install,
        }
    };
    let mut relaxed = Vec::new();
    if defaults.sandbox_enabled && !effective.sandbox_enabled {
        relaxed.push(RelaxedCheck {
            check: SecurityCheck::Sandbox,
            relaxed_by: relaxed_by(
                scope_overrides.sandbox_enabled.is_some(),
                install_overrides.sandbox_enabled.is_some(),
            ),
            detail: "The per-turn sandbox is off: shell, Python and file tools run unconfined, \
                     and every turn is recorded as degraded."
                .into(),
        });
    }
    if !defaults.allow_degraded_execution && effective.allow_degraded_execution {
        relaxed.push(RelaxedCheck {
            check: SecurityCheck::SandboxConfinement,
            relaxed_by: relaxed_by(
                scope_overrides.allow_degraded_execution.is_some(),
                install_overrides.allow_degraded_execution.is_some(),
            ),
            detail: "A turn whose sandbox cannot start runs unconfined instead of refusing.".into(),
        });
    }
    let missing: Vec<&str> = defaults
        .require_approval
        .iter()
        .map(RequireApprovalEntry::tool_name)
        .filter(|tool| {
            !effective
                .require_approval
                .iter()
                .any(|entry| entry.tool_name() == *tool)
        })
        .collect();
    if !missing.is_empty() {
        relaxed.push(RelaxedCheck {
            check: SecurityCheck::ApprovalPrompts,
            relaxed_by: relaxed_by(
                scope_overrides.require_approval.is_some(),
                install_overrides.require_approval.is_some(),
            ),
            detail: format!(
                "Attended sessions are not asked before these high-risk tools run: {}.",
                missing.join(", ")
            ),
        });
    }
    if defaults.access_gate == AccessGateMode::Token
        && effective.access_gate != AccessGateMode::Token
    {
        relaxed.push(RelaxedCheck {
            check: SecurityCheck::AccessGate,
            // The gate is install-wide: whichever scope is read, the install set it.
            relaxed_by: SettingsScope::Install,
            detail: match effective.access_gate {
                AccessGateMode::Warn => {
                    "A request from beyond loopback without the token is admitted and logged."
                        .into()
                }
                _ => "Requests from beyond loopback are admitted without a token.".into(),
            },
        });
    }
    relaxed
}

fn describe_relaxations(relaxed: &[RelaxedCheck]) -> String {
    if relaxed.is_empty() {
        return "every shipped check in force".to_owned();
    }
    let names: Vec<&str> = relaxed
        .iter()
        .map(|entry| match entry.check {
            SecurityCheck::Sandbox => "sandbox relaxed",
            SecurityCheck::SandboxConfinement => "sandbox confinement relaxed",
            SecurityCheck::ApprovalPrompts => "approval prompts relaxed",
            SecurityCheck::AccessGate => "access gate relaxed",
        })
        .collect();
    names.join(", ")
}

/// What a scope's own file says about the security checks.
fn overrides_from_partial(
    partial: &swage_config::PartialConfig,
) -> Result<SecurityOverrides, ApiError> {
    Ok(SecurityOverrides {
        sandbox_enabled: partial.sandbox.as_ref().and_then(|sandbox| sandbox.enabled),
        allow_degraded_execution: partial
            .sandbox
            .as_ref()
            .and_then(|sandbox| sandbox.allow_degraded_execution),
        require_approval: partial
            .tools
            .as_ref()
            .and_then(|tools| tools.require_approval.as_ref())
            .map(|entries| wire_entries(entries))
            .transpose()?,
        access_gate: partial.server.as_ref().and_then(|server| server.access),
    })
}

/// Write the overrides into the scope's partial, whole: an absent field
/// clears the scope's own setting.
fn apply_overrides(
    partial: &mut swage_config::PartialConfig,
    overrides: &SecurityOverrides,
) -> Result<(), ApiError> {
    let sandbox = partial.sandbox.get_or_insert_with(Default::default);
    sandbox.enabled = overrides.sandbox_enabled;
    sandbox.allow_degraded_execution = overrides.allow_degraded_execution;
    if sandbox.enabled.is_none() && sandbox.allow_degraded_execution.is_none() {
        partial.sandbox = None;
    }
    let require_approval = overrides
        .require_approval
        .as_ref()
        .map(|entries| config_entries(entries))
        .transpose()?;
    match (&mut partial.tools, require_approval) {
        (Some(tools), entries) => tools.require_approval = entries,
        (None, Some(entries)) => {
            partial.tools = Some(swage_config::merge::PartialToolConfig {
                require_approval: Some(entries),
                ..Default::default()
            });
        }
        (None, None) => {}
    }
    let server = partial.server.get_or_insert_with(Default::default);
    server.access = overrides.access_gate;
    if server.host.is_none() && server.port.is_none() && server.access.is_none() {
        partial.server = None;
    }
    Ok(())
}

/// The install's running configuration takes the saved overrides at once,
/// so the next turn runs under them without a restart.
fn apply_to_active_config(state: &AppState, partial: &swage_config::PartialConfig) {
    let mut config = state
        .runtime_config
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let defaults = swage_config::SandboxConfig::default();
    let sandbox = partial.sandbox.clone().unwrap_or_default();
    config.sandbox.enabled = sandbox.enabled.unwrap_or(defaults.enabled);
    config.sandbox.allow_degraded_execution = sandbox
        .allow_degraded_execution
        .unwrap_or(defaults.allow_degraded_execution);
    config.tools.permissions.require_approval = partial
        .tools
        .as_ref()
        .and_then(|tools| tools.require_approval.clone())
        .unwrap_or_else(|| swage_config::ToolPermissionConfig::default().require_approval);
    config.server.access = partial
        .server
        .as_ref()
        .and_then(|server| server.access)
        .unwrap_or(swage_config::ServerConfig::default().access);
}

fn validate_overrides(
    scope: &SettingsScope,
    overrides: &SecurityOverrides,
) -> Result<(), ApiError> {
    if overrides.access_gate.is_some() && *scope != SettingsScope::Install {
        return Err(ApiError::validation(
            "the access gate admits a client before any topic is chosen, so it is relaxed for the whole install only".into(),
        ));
    }
    if let Some(entries) = &overrides.require_approval
        && let Some(blank) = entries
            .iter()
            .find(|entry| entry.tool_name().trim().is_empty())
    {
        return Err(ApiError::validation(format!(
            "an approval entry names no tool: {blank:?}"
        )));
    }
    Ok(())
}

fn wire_entries(
    entries: &[swage_config::model::RequireApprovalEntry],
) -> Result<Vec<RequireApprovalEntry>, ApiError> {
    serde_json::from_value(serde_json::to_value(entries).map_err(snapshot_error)?)
        .map_err(snapshot_error)
}

fn config_entries(
    entries: &[RequireApprovalEntry],
) -> Result<Vec<swage_config::model::RequireApprovalEntry>, ApiError> {
    serde_json::from_value(serde_json::to_value(entries).map_err(snapshot_error)?)
        .map_err(snapshot_error)
}

fn storage_error(error: swage_core::StorageError) -> ApiError {
    ApiError::from(swage_core::GatewayError::from(error))
}

fn snapshot_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::validation(format!("invalid security settings: {error}"))
}

async fn read_partial(path: &Path) -> Result<swage_config::PartialConfig, ApiError> {
    if !path.exists() {
        return Ok(swage_config::PartialConfig::default());
    }
    let body = tokio::fs::read_to_string(path)
        .await
        .map_err(|error| ApiError::validation(format!("failed to read config: {error}")))?;
    toml::from_str(&body)
        .map_err(|error| ApiError::validation(format!("failed to parse config: {error}")))
}

async fn write_partial(path: &Path, partial: &swage_config::PartialConfig) -> Result<(), ApiError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            ApiError::validation(format!("failed to create config directory: {error}"))
        })?;
    }
    let body = toml::to_string_pretty(partial)
        .map_err(|error| ApiError::validation(format!("failed to serialise settings: {error}")))?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        swage_client::runtime::write_atomically(&path, body.as_bytes())
    })
    .await
    .map_err(|error| ApiError::internal(format!("config write task failed: {error}")))?
    .map_err(|error| ApiError::validation(format!("failed to write config: {error}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_storage::store::{AuditStore, TopicStore};
    use swage_storage::{NewTopic, SqliteStorage, Storage};

    use super::*;

    async fn fixture() -> (AppState, Arc<SqliteStorage>, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("config root");
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let config = swage_config::ResolvedConfig::default();
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![]))
                as Arc<dyn swage_provider::LlmProvider>,
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );
        (state, storage, root)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn general_save_cannot_overwrite_newer_security_settings() {
        let (state, _, _root) = fixture().await;
        let mut old =
            serde_json::to_value(crate::settings::load_settings(&state).await.unwrap()).unwrap();
        old["sandbox"] = serde_json::json!({"enabled": false, "allow_degraded_execution": false});
        old["tool_permissions"]["require_approval"] = serde_json::json!([]);
        save_security_settings(
            &state,
            SettingsScope::Install,
            save(SecurityOverrides {
                sandbox_enabled: Some(true),
                allow_degraded_execution: Some(true),
                require_approval: Some(vec![RequireApprovalEntry::Simple("shell".into())]),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        crate::settings::save_settings(&state, serde_json::from_value(old).unwrap())
            .await
            .unwrap();
        let reading = read_security_settings(&state, SettingsScope::Install)
            .await
            .unwrap();
        assert!(
            reading.effective.sandbox_enabled,
            "general save reverted the sandbox"
        );
        assert!(
            reading.effective.allow_degraded_execution,
            "general save reverted confinement"
        );
        assert_eq!(
            serde_json::to_value(reading.effective.require_approval).unwrap(),
            serde_json::json!(["shell"])
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unconfined_fallback_is_saved_and_read_as_effective() {
        let (state, _, _root) = fixture().await;
        save_security_settings(
            &state,
            SettingsScope::Install,
            save(SecurityOverrides {
                allow_degraded_execution: Some(true),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        let reading = read_security_settings(&state, SettingsScope::Install)
            .await
            .unwrap();
        assert_eq!(reading.overrides.allow_degraded_execution, Some(true));
        assert!(reading.effective.allow_degraded_execution);
        assert!(state.active_config().sandbox.allow_degraded_execution);
    }

    #[tokio::test]
    async fn security_reading_lists_the_active_registry_for_the_picker() {
        let (state, _, _root) = fixture().await;
        state.replace_runtime(
            state.active_config(),
            state.active_provider(),
            crate::tool::ToolDispatcher::allow_all(vec![Arc::new(
                crate::tool::file_read::FileReadTool {
                    max_output_bytes: 4096,
                },
            ) as Arc<dyn crate::tool::Tool>]),
        );
        let reading = read_security_settings(&state, SettingsScope::Install)
            .await
            .expect("reading");
        assert_eq!(reading.available_tools, vec!["file_read"]);
    }

    fn save(overrides: SecurityOverrides) -> SaveSecuritySettingsRequest {
        SaveSecuritySettingsRequest { overrides }
    }

    fn no_overrides(overrides: &SecurityOverrides) -> bool {
        overrides.sandbox_enabled.is_none()
            && overrides.allow_degraded_execution.is_none()
            && overrides.require_approval.is_none()
            && overrides.access_gate.is_none()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_install_ships_every_check_on_and_relaxing_the_sandbox_applies_at_once() {
        let (state, storage, root) = fixture().await;
        let before = read_security_settings(&state, SettingsScope::Install)
            .await
            .expect("read");
        assert!(before.effective.sandbox_enabled);
        assert!(!before.effective.allow_degraded_execution);
        assert_eq!(before.effective.require_approval.len(), 8);
        assert!(no_overrides(&before.overrides));
        assert!(before.relaxed.is_empty());

        let after = save_security_settings(
            &state,
            SettingsScope::Install,
            save(SecurityOverrides {
                sandbox_enabled: Some(false),
                ..Default::default()
            }),
        )
        .await
        .expect("save");
        assert!(!after.effective.sandbox_enabled);
        assert_eq!(after.overrides.sandbox_enabled, Some(false));
        assert_eq!(after.relaxed.len(), 1);
        assert_eq!(after.relaxed[0].check, SecurityCheck::Sandbox);
        assert_eq!(after.relaxed[0].relaxed_by, SettingsScope::Install);
        assert!(
            !state.active_config().sandbox.enabled,
            "the running gateway reads the relaxation without a restart"
        );
        let written = std::fs::read_to_string(root.path().join("config.toml")).expect("file");
        assert!(
            written.contains("[sandbox]") && written.contains("enabled = false"),
            "{written}"
        );
        let reloaded = swage_config::load_resolved_config_from(
            root.path(),
            &ScopeLocation::General,
            "default",
        )
        .expect("the saved file reloads");
        assert!(!reloaded.sandbox.enabled);

        let changes = storage
            .list_audit_events_filtered(swage_storage::AuditEventFilter {
                entity_kind: Some("security_settings".into()),
                ..Default::default()
            })
            .await
            .expect("audit read");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].entity_id, "install");
        assert_eq!(changes[0].event_kind, "configuration_change");
        assert_eq!(
            changes[0].metadata["prior_state"]["effective"]["sandbox_enabled"],
            true
        );
        assert_eq!(
            changes[0].metadata["new_state"]["effective"]["sandbox_enabled"],
            false
        );
        assert!(changes[0].summary.contains("sandbox relaxed"));

        // Clearing the override restores the shipped default.
        let restored =
            save_security_settings(&state, SettingsScope::Install, save(Default::default()))
                .await
                .expect("clear");
        assert!(restored.effective.sandbox_enabled);
        assert!(restored.relaxed.is_empty());
        assert!(state.active_config().sandbox.enabled);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_topic_inherits_the_install_and_its_own_override_is_attributed_to_it() {
        let (state, storage, root) = fixture().await;
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Orchard".into(),
                slug: "orchard".into(),
            },
        )
        .await
        .expect("topic");
        let scope = SettingsScope::Topic { topic_id: topic.id };

        // The install relaxes approvals; the topic inherits that, attributed to the install.
        save_security_settings(
            &state,
            SettingsScope::Install,
            save(SecurityOverrides {
                require_approval: Some(vec![RequireApprovalEntry::Simple("shell".into())]),
                ..Default::default()
            }),
        )
        .await
        .expect("install save");
        let inherited = read_security_settings(&state, scope.clone())
            .await
            .expect("read");
        assert!(no_overrides(&inherited.overrides));
        assert_eq!(inherited.relaxed.len(), 1);
        assert_eq!(inherited.relaxed[0].check, SecurityCheck::ApprovalPrompts);
        assert_eq!(inherited.relaxed[0].relaxed_by, SettingsScope::Install);
        assert!(inherited.relaxed[0].detail.contains("file_write"));
        assert!(!inherited.relaxed[0].detail.contains("shell"));

        // The topic turns the sandbox off for itself: attributed to the topic,
        // written to the topic's own file, and read by a turn in the topic
        // while the install stays on.
        let saved = save_security_settings(
            &state,
            scope.clone(),
            save(SecurityOverrides {
                sandbox_enabled: Some(false),
                ..Default::default()
            }),
        )
        .await
        .expect("topic save");
        assert!(!saved.effective.sandbox_enabled);
        let sandbox = saved
            .relaxed
            .iter()
            .find(|entry| entry.check == SecurityCheck::Sandbox)
            .expect("sandbox relaxation");
        assert_eq!(sandbox.relaxed_by, scope);
        assert!(
            state.active_config().sandbox.enabled,
            "the install is untouched"
        );
        let topic_file = topic.location().data_dir(root.path()).join("config.toml");
        assert!(
            std::fs::read_to_string(&topic_file)
                .expect("topic file")
                .contains("enabled = false")
        );
        let in_topic = crate::turn::load_session_config_from_root(
            root.path(),
            state.active_config(),
            &topic.location(),
            "default",
        )
        .expect("topic config");
        assert!(!in_topic.sandbox.enabled);

        let changes = storage
            .list_audit_events_filtered(swage_storage::AuditEventFilter {
                entity_kind: Some("security_settings".into()),
                entity_id: Some(topic.id.to_string()),
                ..Default::default()
            })
            .await
            .expect("audit read");
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].metadata["new_state"]["scope"]["topic_id"],
            topic.id.to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blank_approval_entry_is_refused_and_nothing_is_written() {
        let (state, _storage, root) = fixture().await;
        let error = save_security_settings(
            &state,
            SettingsScope::Install,
            save(SecurityOverrides {
                require_approval: Some(vec![RequireApprovalEntry::Simple("  ".into())]),
                ..Default::default()
            }),
        )
        .await
        .expect_err("blank entry refused");
        assert!(error.to_string().contains("names no tool"));
        assert!(
            !root.path().join("config.toml").exists(),
            "nothing was written"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_access_gate_relaxes_for_the_install_only_and_applies_at_once() {
        let (state, storage, root) = fixture().await;
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Orchard".into(),
                slug: "orchard".into(),
            },
        )
        .await
        .expect("topic");
        let refused = save_security_settings(
            &state,
            SettingsScope::Topic { topic_id: topic.id },
            save(SecurityOverrides {
                access_gate: Some(AccessGateMode::Off),
                ..Default::default()
            }),
        )
        .await
        .expect_err("a topic cannot relax the gate");
        assert!(refused.to_string().contains("whole install only"));

        let relaxed = save_security_settings(
            &state,
            SettingsScope::Install,
            save(SecurityOverrides {
                access_gate: Some(AccessGateMode::Warn),
                ..Default::default()
            }),
        )
        .await
        .expect("install save");
        assert_eq!(relaxed.effective.access_gate, AccessGateMode::Warn);
        assert_eq!(relaxed.relaxed.len(), 1);
        assert_eq!(relaxed.relaxed[0].check, SecurityCheck::AccessGate);
        assert_eq!(state.active_config().server.access, AccessGateMode::Warn);
        let written = std::fs::read_to_string(root.path().join("config.toml")).expect("file");
        assert!(
            written.contains("[server]") && written.contains("access = \"warn\""),
            "{written}"
        );
        // A topic reads the install's relaxation as the install's.
        let in_topic = read_security_settings(&state, SettingsScope::Topic { topic_id: topic.id })
            .await
            .expect("topic read");
        assert_eq!(in_topic.relaxed[0].relaxed_by, SettingsScope::Install);
    }
}

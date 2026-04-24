use crate::state::AppState;
use serde::{Deserialize, Serialize};
use swage_core::GatewayError;
use swage_storage::model::{AppLens, LensPreference, LensRecord, LensSourceKind, NewLensRecord};
use uuid::Uuid;

const ALLOWED_UI_MODULES: &[&str] = &["conversation", "operator", "builder"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensDescriptor {
    pub id: AppLens,
    pub title: String,
    pub summary: String,
    pub default_surface: String,
    pub ui_modules: Vec<String>,
    pub prominent_team_ids: Vec<String>,
    pub source_kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LensRuntimeResponse {
    pub bench_id: Uuid,
    pub active_lens: AppLens,
    pub updated_at: String,
    pub available_lenses: Vec<LensDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateLensRuntimeRequest {
    pub lens: AppLens,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateLensRequest {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub default_surface: String,
    #[serde(default)]
    pub ui_modules: Vec<String>,
    #[serde(default)]
    pub prominent_team_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UpdateLensRequest {
    pub title: String,
    pub summary: String,
    pub default_surface: String,
    #[serde(default)]
    pub ui_modules: Vec<String>,
    #[serde(default)]
    pub prominent_team_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InstallLensRequest {
    pub preset_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LensListResponse {
    pub lenses: Vec<LensDescriptor>,
}

pub(crate) fn validate_lens_payload(
    title: &str,
    summary: &str,
    default_surface: &str,
    ui_modules: &[String],
) -> Result<(), GatewayError> {
    if title.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "lens title must not be empty".to_string(),
        ));
    }
    if summary.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "lens summary must not be empty".to_string(),
        ));
    }
    if ui_modules.is_empty() {
        return Err(GatewayError::ValidationError(
            "lens ui_modules must not be empty".to_string(),
        ));
    }
    if !ui_modules.iter().any(|module| module == default_surface) {
        return Err(GatewayError::ValidationError(format!(
            "lens default_surface {default_surface} must be included in ui_modules"
        )));
    }
    for module in ui_modules {
        if !ALLOWED_UI_MODULES.contains(&module.as_str()) {
            return Err(GatewayError::ValidationError(format!(
                "unsupported lens ui module: {module}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn preset_lens(preset_id: &str) -> Result<NewLensRecord, GatewayError> {
    match preset_id {
        "research" => Ok(NewLensRecord {
            id: AppLens::new("research").map_err(GatewayError::ValidationError)?,
            title: "Research lens".to_string(),
            summary: "Keeps investigation work front-and-centre with operator visibility nearby."
                .to_string(),
            default_surface: "conversation".to_string(),
            ui_modules: vec!["conversation".to_string(), "operator".to_string()],
            prominent_team_ids: vec!["research-lead".to_string(), "reviewer".to_string()],
            source_kind: LensSourceKind::Installed,
        }),
        other => Err(GatewayError::ValidationError(format!(
            "unknown lens preset: {other}"
        ))),
    }
}

fn to_descriptor(record: LensRecord) -> LensDescriptor {
    LensDescriptor {
        id: record.id,
        title: record.title,
        summary: record.summary,
        default_surface: record.default_surface,
        ui_modules: record.ui_modules,
        prominent_team_ids: record.prominent_team_ids,
        source_kind: record.source_kind.to_string(),
    }
}

fn to_response(preference: LensPreference, lenses: Vec<LensRecord>) -> LensRuntimeResponse {
    LensRuntimeResponse {
        bench_id: preference.bench_id,
        active_lens: preference.active_lens,
        updated_at: preference.updated_at.to_rfc3339(),
        available_lenses: lenses.into_iter().map(to_descriptor).collect(),
    }
}

pub async fn list_lenses(state: &AppState) -> Result<LensListResponse, GatewayError> {
    let lenses = state
        .storage
        .list_lenses()
        .await
        .map_err(GatewayError::Storage)?;
    Ok(LensListResponse {
        lenses: lenses.into_iter().map(to_descriptor).collect(),
    })
}

pub async fn create_lens(
    state: &AppState,
    request: CreateLensRequest,
) -> Result<LensDescriptor, GatewayError> {
    validate_lens_payload(
        &request.title,
        &request.summary,
        &request.default_surface,
        &request.ui_modules,
    )?;
    let lens = state
        .storage
        .create_lens(NewLensRecord {
            id: AppLens::new(request.id).map_err(GatewayError::ValidationError)?,
            title: request.title,
            summary: request.summary,
            default_surface: request.default_surface,
            ui_modules: request.ui_modules,
            prominent_team_ids: request.prominent_team_ids,
            source_kind: LensSourceKind::Authored,
        })
        .await
        .map_err(GatewayError::Storage)?;
    Ok(to_descriptor(lens))
}

pub async fn update_lens(
    state: &AppState,
    lens_id: &str,
    request: UpdateLensRequest,
) -> Result<LensDescriptor, GatewayError> {
    validate_lens_payload(
        &request.title,
        &request.summary,
        &request.default_surface,
        &request.ui_modules,
    )?;
    let lens_id = AppLens::new(lens_id).map_err(GatewayError::ValidationError)?;
    let existing = state
        .storage
        .get_lens(&lens_id)
        .await
        .map_err(GatewayError::Storage)?
        .ok_or_else(|| GatewayError::ValidationError(format!("lens {} not found", lens_id)))?;
    if existing.source_kind == LensSourceKind::BuiltIn {
        return Err(GatewayError::ValidationError(format!(
            "built-in lens {} cannot be edited",
            lens_id
        )));
    }

    let lens = state
        .storage
        .update_lens(
            &lens_id,
            request.title,
            request.summary,
            request.default_surface,
            request.ui_modules,
            request.prominent_team_ids,
        )
        .await
        .map_err(GatewayError::Storage)?;
    Ok(to_descriptor(lens))
}

pub async fn install_lens(
    state: &AppState,
    request: InstallLensRequest,
) -> Result<LensDescriptor, GatewayError> {
    let preset = preset_lens(&request.preset_id)?;
    if let Some(existing) = state
        .storage
        .get_lens(&preset.id)
        .await
        .map_err(GatewayError::Storage)?
    {
        return Ok(to_descriptor(existing));
    }

    let lens = state
        .storage
        .create_lens(preset)
        .await
        .map_err(GatewayError::Storage)?;
    Ok(to_descriptor(lens))
}

pub async fn get_lens_runtime(
    state: &AppState,
    bench_id: Uuid,
) -> Result<LensRuntimeResponse, GatewayError> {
    state
        .bench_store
        .get(bench_id)
        .await?
        .ok_or(GatewayError::BenchNotFound { id: bench_id })?;
    let preference = state
        .storage
        .get_active_lens(bench_id)
        .await
        .map_err(GatewayError::Storage)?;
    let lenses = state
        .storage
        .list_lenses()
        .await
        .map_err(GatewayError::Storage)?;
    Ok(to_response(preference, lenses))
}

pub async fn update_lens_runtime(
    state: &AppState,
    bench_id: Uuid,
    request: UpdateLensRuntimeRequest,
) -> Result<LensRuntimeResponse, GatewayError> {
    state
        .bench_store
        .get(bench_id)
        .await?
        .ok_or(GatewayError::BenchNotFound { id: bench_id })?;
    let preference = state
        .storage
        .set_active_lens(bench_id, request.lens)
        .await
        .map_err(GatewayError::Storage)?;
    let lenses = state
        .storage
        .list_lenses()
        .await
        .map_err(GatewayError::Storage)?;
    Ok(to_response(preference, lenses))
}

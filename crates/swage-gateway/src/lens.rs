use serde::{Deserialize, Serialize};
use swage_core::GatewayError;
use swage_storage::model::{AppLens, LensPreference};
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensDescriptor {
    pub id: AppLens,
    pub title: String,
    pub summary: String,
    pub default_surface: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LensRuntimeResponse {
    pub active_lens: AppLens,
    pub updated_at: String,
    pub available_lenses: Vec<LensDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateLensRuntimeRequest {
    pub lens: AppLens,
}

fn available_lenses() -> Vec<LensDescriptor> {
    vec![
        LensDescriptor {
            id: AppLens::Assistant,
            title: "Assistant lens".to_string(),
            summary: "Conversation-first view for direct work with the active agent."
                .to_string(),
            default_surface: "conversation".to_string(),
        },
        LensDescriptor {
            id: AppLens::Builder,
            title: "Builder lens".to_string(),
            summary: "Workspace-first view that foregrounds system shaping and operator state."
                .to_string(),
            default_surface: "builder".to_string(),
        },
    ]
}

fn to_response(preference: LensPreference) -> LensRuntimeResponse {
    LensRuntimeResponse {
        active_lens: preference.active_lens,
        updated_at: preference.updated_at.to_rfc3339(),
        available_lenses: available_lenses(),
    }
}

pub async fn get_lens_runtime(state: &AppState) -> Result<LensRuntimeResponse, GatewayError> {
    let preference = state
        .storage
        .get_active_lens()
        .await
        .map_err(GatewayError::Storage)?;
    Ok(to_response(preference))
}

pub async fn update_lens_runtime(
    state: &AppState,
    request: UpdateLensRuntimeRequest,
) -> Result<LensRuntimeResponse, GatewayError> {
    let preference = state
        .storage
        .set_active_lens(request.lens)
        .await
        .map_err(GatewayError::Storage)?;
    Ok(to_response(preference))
}

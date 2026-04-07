use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use swage_core::GatewayError;

/// Wraps `GatewayError` and implements axum's `IntoResponse` so handlers can
/// return `Result<_, ApiError>` and the correct HTTP status + JSON body is sent
/// automatically.
pub struct ApiError(pub GatewayError);

impl From<GatewayError> for ApiError {
    fn from(err: GatewayError) -> Self {
        ApiError(err)
    }
}

impl ApiError {
    /// 404 Not Found for a named entity.
    pub fn not_found(entity: &str, id: uuid::Uuid) -> Self {
        ApiError(GatewayError::Storage(swage_core::StorageError::NotFound {
            entity: entity.to_string(),
            id: id.to_string(),
        }))
    }

    /// 500 Internal Server Error with a free-form message.
    pub fn internal(message: String) -> Self {
        ApiError(GatewayError::ConfigError(message))
    }

    /// 409 Conflict.
    pub fn conflict(message: String) -> Self {
        ApiError(GatewayError::ProjectConflict(message))
    }

    /// 422 Validation Error.
    pub fn validation(message: String) -> Self {
        ApiError(GatewayError::ValidationError(message))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = classify(&self.0);
        let body = json!({
            "code": code,
            "message": self.0.to_string(),
        });
        (status, Json(body)).into_response()
    }
}

/// Map a `GatewayError` variant to an HTTP status code and a short error code
/// string suitable for machine consumption.
fn classify(err: &GatewayError) -> (StatusCode, &'static str) {
    match err {
        GatewayError::ConversationNotFound { .. } => (StatusCode::NOT_FOUND, "not_found"),
        GatewayError::ProjectNotFound { .. } => (StatusCode::NOT_FOUND, "not_found"),
        GatewayError::TurnInProgress { .. } => (StatusCode::CONFLICT, "turn_in_progress"),
        GatewayError::ProjectConflict(_) => (StatusCode::CONFLICT, "conflict"),
        GatewayError::ValidationError(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_error"),
        GatewayError::Provider(_) => (StatusCode::BAD_GATEWAY, "provider_error"),
        GatewayError::Storage(swage_core::StorageError::NotFound { .. }) => {
            (StatusCode::NOT_FOUND, "not_found")
        }
        GatewayError::Storage(swage_core::StorageError::ConstraintViolation(_)) => {
            (StatusCode::CONFLICT, "conflict")
        }
        // All other variants are unexpected internal failures.
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use uuid::Uuid;

    use swage_core::ProviderError;

    use super::*;

    async fn status_and_code(err: GatewayError) -> (StatusCode, String) {
        let response = ApiError(err).into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, body["code"].as_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn conversation_not_found_maps_to_404() {
        let (status, code) =
            status_and_code(GatewayError::ConversationNotFound { id: Uuid::new_v4() }).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "not_found");
    }

    #[tokio::test]
    async fn turn_in_progress_maps_to_409() {
        let (status, code) = status_and_code(GatewayError::TurnInProgress {
            session_id: Uuid::new_v4(),
        })
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "turn_in_progress");
    }

    #[tokio::test]
    async fn provider_error_maps_to_502() {
        let (status, code) = status_and_code(GatewayError::Provider(ProviderError::StreamError(
            "overloaded".to_string(),
        )))
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(code, "provider_error");
    }

    #[tokio::test]
    async fn config_error_maps_to_500() {
        let (status, code) =
            status_and_code(GatewayError::ConfigError("bad config".to_string())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code, "internal_error");
    }
}

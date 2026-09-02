//! The gateway's side of the wire error contract.
//!
//! Every failed HTTP response on the client surface carries
//! `swage_protocol::client::error::ApiErrorBody` — one `{code, message}`
//! shape with the status projected from the code. Three things produce
//! failures and all three land here: a `GatewayError` a handler returns
//! (`From` below, with the exhaustive code mapping in `error_code`), a
//! request body the framework could not parse (the [`Json`] extractor's
//! rejection), and a refusal a handler states directly (the constructors).
//! No handler builds an error body by hand.

use axum::{
    extract::{FromRequest, OptionalFromRequest, Request, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};

use swage_core::{GatewayError, StorageError};
use swage_protocol::client::error::{ApiErrorBody, ErrorCode};

/// A failed request, ready to answer with the contract's body.
///
/// Handlers return `Result<_, ApiError>`; `?` on a `GatewayError` converts
/// through [`error_code`]. The originating `GatewayError` is kept where
/// there was one, so callers that need to branch on the domain error
/// (a retry decision, a log line) still can.
#[derive(Debug)]
pub struct ApiError {
    code: ErrorCode,
    message: String,
    cause: Option<GatewayError>,
}

impl ApiError {
    /// A failure stated directly by a handler, with no domain error behind
    /// it — a refusal the route itself decides.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cause: None,
        }
    }

    /// 404 Not Found for a named entity.
    pub fn not_found(entity: &str, id: uuid::Uuid) -> Self {
        Self::not_found_str(entity, &id.to_string())
    }

    /// 404 Not Found for a named entity with a non-UUID identifier.
    pub fn not_found_str(entity: &str, id: &str) -> Self {
        GatewayError::Storage(StorageError::NotFound {
            entity: entity.to_string(),
            id: id.to_string(),
        })
        .into()
    }

    /// 500 Internal Server Error with a free-form message.
    pub fn internal(message: String) -> Self {
        GatewayError::ConfigError(message).into()
    }

    /// 409 Conflict.
    pub fn conflict(message: String) -> Self {
        GatewayError::SpaceConflict(message).into()
    }

    /// 422 Validation Error.
    pub fn validation(message: String) -> Self {
        GatewayError::ValidationError(message).into()
    }

    /// 403 Forbidden.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// The domain error this response answers, when one produced it.
    pub fn cause(&self) -> Option<&GatewayError> {
        self.cause.as_ref()
    }

    pub fn status(&self) -> StatusCode {
        StatusCode::from_u16(self.code.status()).expect("error codes name valid statuses")
    }

    fn body(&self) -> ApiErrorBody {
        ApiErrorBody {
            code: self.code,
            message: self.message.clone(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
}

impl From<GatewayError> for ApiError {
    fn from(error: GatewayError) -> Self {
        Self {
            code: error_code(&error),
            message: error.to_string(),
            cause: Some(error),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status(), axum::Json(self.body())).into_response()
    }
}

/// The wire code for a domain error. Exhaustive on purpose: a new
/// `GatewayError` variant must choose its code here rather than fall to an
/// internal-error catch-all that hides it from clients.
pub fn error_code(error: &GatewayError) -> ErrorCode {
    match error {
        GatewayError::ConversationNotFound { .. } | GatewayError::SpaceNotFound { .. } => {
            ErrorCode::NotFound
        }
        GatewayError::TurnInProgress { .. } => ErrorCode::TurnInProgress,
        GatewayError::SpaceConflict(_) => ErrorCode::Conflict,
        GatewayError::ValidationError(_) => ErrorCode::ValidationError,
        GatewayError::Provider(_) => ErrorCode::ProviderError,
        GatewayError::SignInUnsupported(_) => ErrorCode::SignInUnsupported,
        GatewayError::SignInFailed(_) => ErrorCode::SignInFailed,
        GatewayError::ProviderRuntimeUnavailable(_) => ErrorCode::ProviderRuntimeUnavailable,
        GatewayError::Storage(storage) => match storage {
            StorageError::NotFound { .. } => ErrorCode::NotFound,
            StorageError::ConstraintViolation(_) => ErrorCode::Conflict,
            StorageError::Io(_)
            | StorageError::Serialisation { .. }
            | StorageError::Corruption { .. }
            | StorageError::CapacityExceeded { .. }
            | StorageError::ConnectionFailed(_)
            | StorageError::QueryFailed(_)
            | StorageError::MigrationFailed(_) => ErrorCode::InternalError,
        },
        GatewayError::ConfigError(_)
        | GatewayError::BindError(_)
        | GatewayError::WebSocketError(_)
        | GatewayError::ShutdownError(_) => ErrorCode::InternalError,
    }
}

/// The client surface's JSON body extractor and JSON response.
///
/// Extraction is `axum::Json` with its rejection answered as an
/// [`ApiError`], so a body the framework cannot read fails in the
/// contract's shape like every other failure. As a response it is
/// `axum::Json` unchanged. Handlers on the client surface take and return
/// this `Json`, never `axum::Json` directly.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        <axum::Json<T> as FromRequest<S>>::from_request(request, state)
            .await
            .map(|axum::Json(value)| Self(value))
            .map_err(rejection_error)
    }
}

impl<T, S> OptionalFromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Option<Self>, Self::Rejection> {
        <axum::Json<T> as OptionalFromRequest<S>>::from_request(request, state)
            .await
            .map(|value| value.map(|axum::Json(value)| Self(value)))
            .map_err(rejection_error)
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// A body the framework refused, in the contract's shape. The rejection's
/// own text is kept as the message: for a validation failure it names the
/// offending field, which is what a client needs.
fn rejection_error(rejection: JsonRejection) -> ApiError {
    let code = match &rejection {
        JsonRejection::JsonDataError(_) => ErrorCode::ValidationError,
        JsonRejection::JsonSyntaxError(_) | JsonRejection::BytesRejection(_) => {
            ErrorCode::MalformedBody
        }
        JsonRejection::MissingJsonContentType(_) => ErrorCode::UnsupportedMediaType,
        _ => ErrorCode::MalformedBody,
    };
    ApiError::new(code, rejection.body_text())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use axum::{Router, body::to_bytes, http::Request, routing::post};
    use tower::ServiceExt;
    use uuid::Uuid;

    use swage_core::ProviderError;

    use super::*;

    async fn read_error(response: Response) -> (StatusCode, ApiErrorBody) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: ApiErrorBody = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("every failure carries the contract body: {error}"));
        (status, body)
    }

    async fn status_and_code(error: GatewayError) -> (StatusCode, ErrorCode) {
        let (status, body) = read_error(ApiError::from(error).into_response()).await;
        (status, body.code)
    }

    #[tokio::test]
    async fn conversation_not_found_maps_to_404() {
        let (status, code) =
            status_and_code(GatewayError::ConversationNotFound { id: Uuid::new_v4() }).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn turn_in_progress_maps_to_409() {
        let (status, code) = status_and_code(GatewayError::TurnInProgress {
            session_id: Uuid::new_v4(),
        })
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, ErrorCode::TurnInProgress);
    }

    #[tokio::test]
    async fn provider_error_maps_to_502() {
        let (status, code) = status_and_code(GatewayError::Provider(ProviderError::StreamError(
            "overloaded".to_string(),
        )))
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(code, ErrorCode::ProviderError);
    }

    #[tokio::test]
    async fn config_error_maps_to_500() {
        let (status, code) =
            status_and_code(GatewayError::ConfigError("bad config".to_string())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code, ErrorCode::InternalError);
    }

    #[tokio::test]
    async fn provider_sign_in_errors_have_stable_codes() {
        let cases = [
            (
                GatewayError::SignInUnsupported("native provider".to_string()),
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::SignInUnsupported,
            ),
            (
                GatewayError::SignInFailed("login rejected".to_string()),
                StatusCode::BAD_GATEWAY,
                ErrorCode::SignInFailed,
            ),
            (
                GatewayError::ProviderRuntimeUnavailable("spawn failed".to_string()),
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::ProviderRuntimeUnavailable,
            ),
        ];

        for (error, expected_status, expected_code) in cases {
            let (status, code) = status_and_code(error).await;
            assert_eq!(status, expected_status);
            assert_eq!(code, expected_code);
        }
    }

    #[tokio::test]
    async fn a_domain_error_keeps_its_cause_and_message() {
        let error = ApiError::from(GatewayError::ValidationError("name is empty".into()));
        assert_eq!(error.code(), ErrorCode::ValidationError);
        assert_eq!(error.message(), "validation error: name is empty");
        assert!(matches!(
            error.cause(),
            Some(GatewayError::ValidationError(message)) if message == "name is empty"
        ));
    }

    #[tokio::test]
    async fn a_stated_refusal_answers_in_the_contract_shape() {
        let (status, body) =
            read_error(ApiError::forbidden("shutdown is local only").into_response()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.code, ErrorCode::Forbidden);
        assert_eq!(body.message, "shutdown is local only");
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Probe {
        #[allow(dead_code)]
        name: String,
    }

    fn probe_router() -> Router {
        Router::new().route(
            "/probe",
            post(|Json(_): Json<Probe>| async { StatusCode::OK }),
        )
    }

    async fn probe(content_type: &str, body: &'static str) -> Response {
        probe_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/probe")
                    .header("content-type", content_type)
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_body_the_framework_cannot_parse_answers_in_the_contract_shape() {
        let (status, body) = read_error(probe("application/json", "{not json").await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.code, ErrorCode::MalformedBody);
    }

    #[tokio::test]
    async fn a_body_outside_its_contract_names_the_offending_field() {
        let (status, body) =
            read_error(probe("application/json", r#"{"name":"x","extra":1}"#).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body.code, ErrorCode::ValidationError);
        assert!(
            body.message.contains("extra"),
            "the message names the field: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn a_body_without_a_json_content_type_is_refused_in_the_contract_shape() {
        let (status, body) = read_error(probe("text/plain", r#"{"name":"x"}"#).await).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(body.code, ErrorCode::UnsupportedMediaType);
    }

    #[tokio::test]
    async fn a_well_formed_body_reaches_the_handler() {
        let response = probe("application/json", r#"{"name":"x"}"#).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

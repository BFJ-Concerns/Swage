//! The one error shape every HTTP response on the client surface carries.
//!
//! A failed request answers `{"code", "message"}` whatever produced the
//! failure — a gateway-originated error, a body the framework could not
//! parse, a refused connection upgrade. `code` is the machine-readable
//! contract a client branches on; `message` is for people. The HTTP status
//! is a projection of the code, never an independent fact, so a client that
//! reads only the body loses nothing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Machine-readable classification of a failed request.
///
/// Each code names one HTTP status (`status`). Codes are stable wire
/// identifiers: add a code for a new class of failure rather than
/// widening the meaning of an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request body was not valid JSON, or could not be read.
    MalformedBody,
    /// The request did not declare `Content-Type: application/json`.
    UnsupportedMediaType,
    /// The body parsed but failed validation — a missing or unknown field,
    /// a value outside its contract.
    ValidationError,
    /// The caller may not perform this operation from where it is.
    Forbidden,
    /// The addressed entity does not exist.
    NotFound,
    /// The operation collides with existing state (a duplicate name, an
    /// immutable record).
    Conflict,
    /// The conversation already has a turn in flight; retry once it settles.
    TurnInProgress,
    /// The conversation already has a live connection.
    AlreadyConnected,
    /// An upstream model provider failed.
    ProviderError,
    /// The selected provider offers no sign-in flow of the requested kind.
    SignInUnsupported,
    /// The provider refused or could not complete a sign-in.
    SignInFailed,
    /// The provider runtime could not be started or reached.
    ProviderRuntimeUnavailable,
    /// An unexpected failure inside the gateway.
    InternalError,
}

impl ErrorCode {
    /// The HTTP status this code is answered with.
    pub const fn status(self) -> u16 {
        match self {
            Self::MalformedBody => 400,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::Conflict | Self::TurnInProgress | Self::AlreadyConnected => 409,
            Self::UnsupportedMediaType => 415,
            Self::ValidationError | Self::SignInUnsupported => 422,
            Self::InternalError => 500,
            Self::ProviderError | Self::SignInFailed => 502,
            Self::ProviderRuntimeUnavailable => 503,
        }
    }
}

/// The body of every failed client-surface HTTP response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ApiErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialise_as_snake_case_identifiers() {
        let body = ApiErrorBody {
            code: ErrorCode::TurnInProgress,
            message: "turn already in progress".into(),
        };
        let wire = serde_json::to_value(&body).expect("serialise error body");
        assert_eq!(
            wire,
            serde_json::json!({"code": "turn_in_progress", "message": "turn already in progress"})
        );
        let parsed: ApiErrorBody = serde_json::from_value(wire).expect("parse error body");
        assert_eq!(parsed, body);
    }

    #[test]
    fn every_code_names_an_error_status() {
        let codes = [
            ErrorCode::MalformedBody,
            ErrorCode::UnsupportedMediaType,
            ErrorCode::ValidationError,
            ErrorCode::Forbidden,
            ErrorCode::NotFound,
            ErrorCode::Conflict,
            ErrorCode::TurnInProgress,
            ErrorCode::AlreadyConnected,
            ErrorCode::ProviderError,
            ErrorCode::SignInUnsupported,
            ErrorCode::SignInFailed,
            ErrorCode::ProviderRuntimeUnavailable,
            ErrorCode::InternalError,
        ];
        for code in codes {
            assert!(
                (400..600).contains(&code.status()),
                "{code:?} must answer with an error status"
            );
        }
    }

    #[test]
    fn an_unknown_code_is_rejected_not_defaulted() {
        let error = serde_json::from_value::<ApiErrorBody>(
            serde_json::json!({"code": "teapot", "message": "short and stout"}),
        )
        .expect_err("a code outside the contract must not parse");
        assert!(error.to_string().contains("teapot"));
    }
}

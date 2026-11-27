//! Well-known error codes for the `Error` event.
pub const PROVIDER_ERROR: &str = "provider_error";
pub const TURN_IN_PROGRESS: &str = "turn_in_progress";
pub const INVALID_CONFIG: &str = "invalid_config";
pub const INTERNAL_ERROR: &str = "internal_error";
pub const SKILL_NOT_FOUND: &str = "skill_not_found";
/// A request this client sent on the socket failed — an unreadable frame, a
/// message that could not be queued behind the running turn, a skill or
/// approval request. The error ends no turn: one that is running carries on.
pub const REQUEST_FAILED: &str = "request_failed";

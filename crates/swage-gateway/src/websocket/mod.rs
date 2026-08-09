mod connection;
mod dispatch;
pub mod error_codes;
mod protocol;

#[allow(unused_imports)]
pub(crate) use connection::check_upgrade_preconditions;
pub use connection::ws_handler;
pub use protocol::{ClientEvent, SchemaValidationError, ServerEvent};

#[cfg(test)]
use crate::state::AppState;
#[cfg(test)]
use axum::http::StatusCode;
#[cfg(test)]
use dispatch::TEST_SKILL_ROOTS;
use dispatch::dispatch_client_message;
#[cfg(test)]
use tokio::sync::{Mutex, mpsc};
#[cfg(test)]
use uuid::Uuid;
#[cfg(test)]
mod tests;

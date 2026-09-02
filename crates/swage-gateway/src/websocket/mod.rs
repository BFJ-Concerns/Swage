mod connection;
mod dispatch;
pub mod error_codes;
mod protocol;
mod work_surface;

#[allow(unused_imports)]
pub(crate) use connection::check_upgrade_preconditions;
pub use connection::ws_handler;
pub use protocol::{ClientEvent, SchemaValidationError, ServerEvent};
pub use work_surface::work_surface_ws_handler;

#[cfg(test)]
use crate::state::AppState;
#[cfg(test)]
use axum::http::StatusCode;
pub(crate) use dispatch::discover_available_for_session;
use dispatch::dispatch_client_message;
#[cfg(test)]
use tokio::sync::{Mutex, mpsc};
#[cfg(test)]
use uuid::Uuid;
#[cfg(test)]
mod tests;

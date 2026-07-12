//! Codex App Server transport and lifecycle support.
//!
//! This crate deliberately contains no Swage permission or storage policy. It
//! speaks the app-server protocol; the gateway decides what requests mean.

mod client;
mod protocol;
mod supervisor;

pub use client::{AppServerClient, ClientError, ServerEvent, ServerRequest};
pub use protocol::*;
pub use supervisor::{AppServerConfig, AppServerHandle, AppServerSupervisor, RuntimeEvent};

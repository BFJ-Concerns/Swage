//! IPC protocol for host-agent communication.
//!
//! Newline-delimited JSON messages over Unix domain sockets.

pub mod agent;
pub mod host;
pub(crate) mod in_flight;
pub mod message;
pub mod transport;

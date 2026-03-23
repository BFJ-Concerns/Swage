//! Typed gateway client library for the Swage platform.
//!
//! Provides HTTP and WebSocket access to a running Swage gateway, shared by
//! the CLI (gateway lifecycle management) and TUI (conversation interface).
//!
//! # Modules
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`runtime`] | Runtime metadata (PID, address, start time) — file I/O and discovery |
//! | [`error`] | Typed client errors with address context and underlying cause |
//! | [`http`] | HTTP client covering the gateway REST API |
//! | [`websocket`] | WebSocket client for the streaming event protocol |

pub mod error;
pub mod http;
pub mod runtime;
pub mod websocket;

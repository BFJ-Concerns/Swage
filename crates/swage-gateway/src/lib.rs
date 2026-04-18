//! Swage gateway — HTTP and WebSocket server for the Swage platform.
//!
//! The gateway is the primary runtime surface of Swage. It exposes a REST API
//! for conversation and project management, a WebSocket interface for streaming
//! agent turns in real time, and an embedded Svelte SPA served from the same
//! process.
//!
//! ## Modules
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`handlers`] | HTTP route handlers for conversations, projects, and health |
//! | [`websocket`] | WebSocket upgrade and bidirectional message protocol |
//! | [`turn`] | Agent turn orchestration: provider streaming, tool dispatch, storage |
//! | [`tool`] | Tool dispatch pipeline with permission checks and rate limiting |
//! | [`service`] | Request/response DTOs shared by handlers and the websocket layer |
//! | [`state`] | [`state::AppState`] — shared runtime state cloned into every handler |
//! | [`server`] | Router construction and request-pipeline middleware |
//! | [`lifecycle`] | [`lifecycle::ActivityTracker`] — atomic last-activity timestamp |
//! | [`shutdown`] | Idle monitor, graceful shutdown, and orphaned turn recovery |
//! | [`context`] | Provider and tool context assembly |
//! | [`assets`] | Embedded frontend SPA served at the fallback route |
//! | [`error`] | [`error::AppError`] — axum-compatible error type |
//!
//! ## Structured Logging
//!
//! The gateway emits structured JSON logs to stderr via `tracing`. Two
//! correlation identifiers propagate through the log stream:
//!
//! - `request_id` — a UUID assigned by [`server::build_router`] middleware to
//!   every inbound HTTP request; returned to clients as `X-Request-Id`.
//! - `turn_id` — the database turn UUID recorded on the tracing span at the
//!   start of each agent turn in [`turn::process_turn`].
//!
//! ## On-Demand Lifecycle
//!
//! When `idle_timeout_secs` is non-zero the gateway shuts itself down after
//! the configured period of inactivity. Activity is tracked by
//! [`lifecycle::ActivityTracker`] and recorded by the activity middleware on
//! every non-health HTTP request and on every streaming token in a turn.
//! [`shutdown::idle_monitor`] polls the tracker and, when the idle threshold is
//! exceeded, either shuts down immediately (no active turns) or enters a grace
//! period equal to `idle_timeout_secs` before proceeding.
//!
//! ## Graceful Shutdown
//!
//! [`shutdown::graceful_shutdown`] waits for SIGTERM or SIGINT. Once either
//! resolves, axum drains in-flight connections and
//! [`shutdown::shutdown_active_turns`] cancels any remaining agent turns.
//! [`shutdown::recover_orphaned_turns`] runs at startup to mark turns left
//! in `Processing` state by a previous unclean exit as `Interrupted`.

pub mod assets;
pub mod attachment;
pub mod audit;
pub mod automated_turn;
pub mod capabilities;
pub mod channel_gateway;
pub mod cli;
pub mod context;
pub mod error;
pub mod execution_host;
pub mod handlers;
pub mod heartbeat;
pub mod lens;
pub mod lifecycle;
pub mod material;
pub mod onboarding;
pub mod operator;
pub mod presence;
pub mod project_extension;
pub mod schedule;
pub mod secret;
pub mod server;
pub mod service;
pub mod shared_reference;
pub mod shutdown;
pub mod skill;
pub mod startup;
pub mod state;
pub mod subscription;
pub mod supervision;
pub mod thread_selection;
pub mod tool;
pub mod tool_executor;
pub mod topology;
pub mod turn;
pub mod voice;
pub mod websocket;

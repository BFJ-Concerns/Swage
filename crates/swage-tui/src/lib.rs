//! Terminal user interface for the Swage AI assistant platform.
//!
//! Provides a conversation interface in the terminal that connects to a running
//! Swage gateway. Uses ratatui + crossterm for rendering and crossterm's async
//! event stream with tokio for non-blocking input/output.
//!
//! # Architecture
//!
//! The TUI is a standard gateway client — it uses the same REST and WebSocket
//! endpoints as the web UI, with no TUI-specific server-side code.
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`app`] | Application struct, lifecycle, and main event dispatch |
//! | [`event`] | Event loop multiplexing terminal input, gateway events, and render ticks |
//! | [`gateway`] | Connection manager with reconnection backoff |
//! | [`state`] | Conversation state model (messages, streaming, tools, context) |
//! | [`keybindings`] | Keyboard input mapping to application actions |
//! | [`ui`] | Layout calculation and widget rendering |

pub mod app;
pub mod event;
pub mod gateway;
pub mod keybindings;
pub mod state;
pub mod ui;

/// Launch the terminal user interface and connect to the Swage gateway.
///
/// Creates a default space context, constructs `App`, and delegates to the
/// full terminal lifecycle in `App::run`. The terminal is restored on both
/// clean exit and panic.
pub async fn run(gateway_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let default_space = state::SpaceInfo {
        id: uuid::Uuid::nil(),
        name: "Default".to_string(),
    };
    let mut app = app::App::new(gateway_url, default_space);
    app.run().await
}

//! Tool dispatch abstraction — the bridge between Python bindings and Rust tools.
//!
//! Defines the `ToolDispatcher` trait that Python binding functions call when
//! agent code invokes `swage.shell()`, `swage.read_file()`, etc. This decouples
//! the Python crate from the gateway's `ToolRegistry` — the gateway provides a
//! concrete implementation that bridges to async tool execution internally.

use serde_json::Value;

// ---------------------------------------------------------------------------
// Dispatch result
// ---------------------------------------------------------------------------

/// The outcome of dispatching a single tool call from Python.
///
/// Mirrors the shape of `ToolResult` in swage-core but without the `call_id`
/// field — the caller already knows which call it made.
#[derive(Debug, Clone)]
pub struct DispatchResult {
    /// Tool output on success, or an error description on failure.
    pub content: String,
    /// `true` when the tool reported failure (the content describes the error).
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Dispatcher trait
// ---------------------------------------------------------------------------

/// Synchronous bridge from Python binding functions to Rust tool execution.
///
/// The trait is deliberately synchronous: Python code calling `swage.shell()`
/// blocks until the tool returns. The gateway's concrete implementation handles
/// the async↔sync bridge internally (e.g. via `tokio::Handle::block_on` on a
/// blocking thread).
///
/// Must be object-safe so it can be stored as `Arc<dyn ToolDispatcher>` and
/// installed as a thread-local during `execute_code`.
pub trait ToolDispatcher: Send + Sync {
    /// Execute a tool by name with the given JSON arguments.
    ///
    /// Returns the tool's output (or error description) synchronously.
    fn dispatch(&self, tool_name: &str, arguments: Value) -> DispatchResult;
}

// ---------------------------------------------------------------------------
// Event reporting
// ---------------------------------------------------------------------------

/// Lifecycle events emitted during sub-tool execution.
///
/// The gateway uses these to push WebSocket updates for nested tool calls
/// without the Python crate knowing about WebSocket internals.
#[derive(Debug, Clone)]
pub enum DispatchEvent {
    /// A tool call is about to execute.
    Started {
        /// Unique identifier for this sub-tool call.
        call_id: String,
        /// The tool being invoked (e.g. "shell", "read_file").
        tool_name: String,
        /// Arguments passed to the tool.
        arguments: Value,
    },
    /// A tool call has finished executing.
    Completed {
        /// Matches the `call_id` from the corresponding `Started` event.
        call_id: String,
        /// The tool that was invoked.
        tool_name: String,
        /// The tool's result.
        result: DispatchResult,
        /// Wall-clock execution time in milliseconds.
        duration_ms: u64,
    },
}

/// Trait for receiving sub-tool activity events.
///
/// Object-safe so the gateway can provide any implementation (channel sender,
/// WebSocket broadcaster, test collector) without the Python crate depending
/// on gateway types.
pub trait DispatchEventSender: Send + Sync {
    /// Deliver one event. Implementations should be non-blocking — if the
    /// receiver is slow or full, events may be dropped rather than stalling
    /// the Python execution thread.
    fn send_event(&self, event: DispatchEvent);
}

/// A no-op event sender that silently discards all events.
///
/// Used when the caller doesn't need sub-tool activity reporting
/// (e.g. in unit tests or when no WebSocket clients are connected).
#[derive(Debug, Clone, Copy)]
pub struct NoOpEventSender;

impl DispatchEventSender for NoOpEventSender {
    fn send_event(&self, _event: DispatchEvent) {
        // Intentionally empty — events are discarded.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // -- Mock dispatcher for testing trait object safety -----------------------

    /// A trivial dispatcher that echoes the tool name and arguments back.
    struct EchoDispatcher;

    impl ToolDispatcher for EchoDispatcher {
        fn dispatch(&self, tool_name: &str, arguments: serde_json::Value) -> DispatchResult {
            DispatchResult {
                content: format!("{tool_name}: {arguments}"),
                is_error: false,
            }
        }
    }

    /// A dispatcher that always returns an error.
    struct ErrorDispatcher;

    impl ToolDispatcher for ErrorDispatcher {
        fn dispatch(&self, tool_name: &str, _arguments: serde_json::Value) -> DispatchResult {
            DispatchResult {
                content: format!("ERROR: {tool_name} not found"),
                is_error: true,
            }
        }
    }

    // -- Trait object safety ---------------------------------------------------

    #[test]
    fn trait_is_object_safe() {
        // Must compile as dyn ToolDispatcher — proves object safety.
        let dispatcher: Box<dyn ToolDispatcher> = Box::new(EchoDispatcher);
        let result = dispatcher.dispatch("shell", serde_json::json!({"command": "ls"}));
        assert!(!result.is_error);
        assert!(result.content.contains("shell"));
    }

    #[test]
    fn trait_is_send_and_sync() {
        // Must be usable behind Arc for thread-safe sharing.
        let dispatcher: Arc<dyn ToolDispatcher> = Arc::new(EchoDispatcher);
        let result = dispatcher.dispatch("read_file", serde_json::json!({"path": "/tmp"}));
        assert!(result.content.contains("read_file"));
    }

    // -- DispatchResult -------------------------------------------------------

    #[test]
    fn dispatch_result_captures_success() {
        let result = DispatchResult {
            content: "file contents here".into(),
            is_error: false,
        };
        assert_eq!(result.content, "file contents here");
        assert!(!result.is_error);
    }

    #[test]
    fn dispatch_result_captures_error() {
        let result = DispatchResult {
            content: "command not found".into(),
            is_error: true,
        };
        assert!(result.is_error);
    }

    // -- Error dispatcher path ------------------------------------------------

    #[test]
    fn error_dispatcher_returns_is_error_true() {
        let dispatcher: Box<dyn ToolDispatcher> = Box::new(ErrorDispatcher);
        let result = dispatcher.dispatch("unknown_tool", serde_json::json!({}));
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    // -- DispatchEvent variants -----------------------------------------------

    #[test]
    fn dispatch_event_started_contains_metadata() {
        let event = DispatchEvent::Started {
            call_id: "call_123".into(),
            tool_name: "shell".into(),
            arguments: serde_json::json!({"command": "echo hi"}),
        };
        if let DispatchEvent::Started { call_id, tool_name, .. } = &event {
            assert_eq!(call_id, "call_123");
            assert_eq!(tool_name, "shell");
        } else {
            panic!("expected Started variant");
        }
    }

    #[test]
    fn dispatch_event_completed_contains_result_and_duration() {
        let event = DispatchEvent::Completed {
            call_id: "call_456".into(),
            tool_name: "read_file".into(),
            result: DispatchResult {
                content: "contents".into(),
                is_error: false,
            },
            duration_ms: 42,
        };
        if let DispatchEvent::Completed { call_id, duration_ms, result, .. } = &event {
            assert_eq!(call_id, "call_456");
            assert_eq!(*duration_ms, 42);
            assert!(!result.is_error);
        } else {
            panic!("expected Completed variant");
        }
    }

    // -- DispatchEventSender --------------------------------------------------

    #[test]
    fn event_sender_is_object_safe() {
        // Must compile as dyn DispatchEventSender — proves object safety.
        use std::sync::Mutex;

        struct CollectingSender {
            events: Mutex<Vec<DispatchEvent>>,
        }

        impl DispatchEventSender for CollectingSender {
            fn send_event(&self, event: DispatchEvent) {
                self.events.lock().unwrap().push(event);
            }
        }

        let sender: Box<dyn DispatchEventSender> = Box::new(CollectingSender {
            events: Mutex::new(Vec::new()),
        });
        sender.send_event(DispatchEvent::Started {
            call_id: "c1".into(),
            tool_name: "shell".into(),
            arguments: serde_json::json!({}),
        });
        // If this compiles and runs, the trait is object-safe.
    }
}

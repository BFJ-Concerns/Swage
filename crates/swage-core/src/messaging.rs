//! Inter-agent message types for the Swage messaging subsystem.
//!
//! Defines the closed message-kind enum, delivery status lifecycle, and
//! context transfer mode — shared across agent, storage, and gateway crates.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown {type_name}: {value}")]
pub struct MessagingParseError {
    type_name: &'static str,
    value: String,
}

impl MessagingParseError {
    fn new(type_name: &'static str, value: impl Into<String>) -> Self {
        Self {
            type_name,
            value: value.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// MessageKind
// ---------------------------------------------------------------------------

/// The intent of an inter-agent message.
///
/// Stored as snake_case text (e.g. "delegate", "cancel") wherever persistence
/// or wire serialisation is needed. The variant set is closed — new message
/// kinds require a migration if added later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Ask a sub-agent to carry out a task and return a result.
    Delegate,
    /// The result of a completed delegation, returned to the parent agent.
    Result,
    /// A one-way notification that does not expect a reply.
    Notify,
    /// Instruct the target agent to abandon its current task.
    Cancel,
    /// Request an orderly agent shutdown.
    Shutdown,
}

impl std::fmt::Display for MessageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MessageKind::Delegate => "delegate",
            MessageKind::Result => "result",
            MessageKind::Notify => "notify",
            MessageKind::Cancel => "cancel",
            MessageKind::Shutdown => "shutdown",
        };
        write!(f, "{s}")
    }
}

impl FromStr for MessageKind {
    type Err = MessagingParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "delegate" => Ok(MessageKind::Delegate),
            "result" => Ok(MessageKind::Result),
            "notify" => Ok(MessageKind::Notify),
            "cancel" => Ok(MessageKind::Cancel),
            "shutdown" => Ok(MessageKind::Shutdown),
            other => Err(MessagingParseError::new("message kind", other)),
        }
    }
}

// ---------------------------------------------------------------------------
// MessageStatus
// ---------------------------------------------------------------------------

/// Delivery lifecycle state of an inter-agent message.
///
/// Transitions: Pending → Delivered (agent received) | Failed (delivery error).
/// Stored as snake_case text in SQLite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    /// Message created and queued; the target agent has not yet received it.
    Pending,
    /// Message reached the target agent's mailbox successfully.
    Delivered,
    /// Delivery failed (target not found, timeout, or routing error).
    Failed,
}

impl std::fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MessageStatus::Pending => "pending",
            MessageStatus::Delivered => "delivered",
            MessageStatus::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl FromStr for MessageStatus {
    type Err = MessagingParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(MessageStatus::Pending),
            "delivered" => Ok(MessageStatus::Delivered),
            "failed" => Ok(MessageStatus::Failed),
            other => Err(MessagingParseError::new("message status", other)),
        }
    }
}

// ---------------------------------------------------------------------------
// ContextTransferMode
// ---------------------------------------------------------------------------

/// Controls how much of the delegating agent's context is forwarded to a
/// sub-agent when a `Delegate` message is sent.
///
/// The mode lives in `CommunicationConfig` / `ContextTransferConfig` (with
/// optional per-target overrides) and is resolved at send time by
/// `context_transfer::resolve_policy`. The Delegate message payload carries
/// the *result* of that policy (the transferred context messages), not the
/// mode itself.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextTransferMode {
    /// No context forwarded — the sub-agent starts with only the task prompt.
    #[default]
    None,
    /// The delegating agent explicitly selects the context elements to forward.
    Explicit,
    /// A concise summary of the parent context is generated and forwarded.
    Summary,
}

impl std::fmt::Display for ContextTransferMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ContextTransferMode::None => "none",
            ContextTransferMode::Explicit => "explicit",
            ContextTransferMode::Summary => "summary",
        };
        write!(f, "{s}")
    }
}

impl FromStr for ContextTransferMode {
    type Err = MessagingParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(ContextTransferMode::None),
            "explicit" => Ok(ContextTransferMode::Explicit),
            "summary" => Ok(ContextTransferMode::Summary),
            other => Err(MessagingParseError::new("context transfer mode", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- MessageKind round-trips ---

    #[test]
    fn message_kind_delegate_displays_as_delegate() {
        assert_eq!(MessageKind::Delegate.to_string(), "delegate");
    }

    #[test]
    fn message_kind_display_from_str_round_trip() {
        let kinds = [
            MessageKind::Delegate,
            MessageKind::Result,
            MessageKind::Notify,
            MessageKind::Cancel,
            MessageKind::Shutdown,
        ];
        for kind in &kinds {
            let s = kind.to_string();
            let parsed: MessageKind = s.parse().expect("round-trip should succeed");
            assert_eq!(kind, &parsed, "round-trip failed for {s}");
        }
    }

    #[test]
    fn message_kind_from_str_unknown_errors() {
        assert!("bogus".parse::<MessageKind>().is_err());
    }

    #[test]
    fn message_kind_serialises_as_snake_case() {
        let json = serde_json::to_string(&MessageKind::Shutdown).expect("serialise");
        assert_eq!(json, "\"shutdown\"");
    }

    // --- MessageStatus round-trips ---

    #[test]
    fn message_status_pending_displays_as_pending() {
        assert_eq!(MessageStatus::Pending.to_string(), "pending");
    }

    #[test]
    fn message_status_display_from_str_round_trip() {
        let statuses = [
            MessageStatus::Pending,
            MessageStatus::Delivered,
            MessageStatus::Failed,
        ];
        for status in &statuses {
            let s = status.to_string();
            let parsed: MessageStatus = s.parse().expect("round-trip should succeed");
            assert_eq!(status, &parsed, "round-trip failed for {s}");
        }
    }

    #[test]
    fn message_status_from_str_unknown_errors() {
        assert!("bogus".parse::<MessageStatus>().is_err());
    }

    #[test]
    fn message_status_serialises_as_snake_case() {
        let json = serde_json::to_string(&MessageStatus::Delivered).expect("serialise");
        assert_eq!(json, "\"delivered\"");
    }

    // --- ContextTransferMode round-trips ---

    #[test]
    fn context_transfer_mode_none_displays_as_none() {
        assert_eq!(ContextTransferMode::None.to_string(), "none");
    }

    #[test]
    fn context_transfer_mode_display_from_str_round_trip() {
        let modes = [
            ContextTransferMode::None,
            ContextTransferMode::Explicit,
            ContextTransferMode::Summary,
        ];
        for mode in &modes {
            let s = mode.to_string();
            let parsed: ContextTransferMode = s.parse().expect("round-trip should succeed");
            assert_eq!(mode, &parsed, "round-trip failed for {s}");
        }
    }

    #[test]
    fn context_transfer_mode_from_str_unknown_errors() {
        assert!("bogus".parse::<ContextTransferMode>().is_err());
    }

    #[test]
    fn context_transfer_mode_serialises_as_snake_case() {
        let json = serde_json::to_string(&ContextTransferMode::Explicit).expect("serialise");
        assert_eq!(json, "\"explicit\"");
    }
}

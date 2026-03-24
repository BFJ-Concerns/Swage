//! Inter-agent message types for the Swage messaging subsystem.
//!
//! Defines the closed message-kind enum, delivery status lifecycle, and
//! context transfer mode — shared across agent, storage, and gateway crates.

// TODO Inter-agent message type enums
// tags: forge:inter-agent-communication
// Define MessageKind (Delegate, Result, Notify, Cancel, Shutdown),
// MessageStatus (Pending, Delivered, Failed) with Display/FromStr round-trips,
// and ContextTransferMode (None, Explicit, Summary) with serde support.
// All three enums need: Debug, Clone, PartialEq, Serialize, Deserialize,
// Display, and FromStr. Follow the pattern of TurnState and ApprovalState
// in swage-storage/src/model.rs.
// Success: all three enums compile, round-trip through Display/FromStr,
// and serialise correctly. Re-exported from swage-core lib.rs.
// ---

//! Delivery expiry processing for inter-agent messages.
//!
//! Durable messages have a configurable TTL. If the target agent is not
//! instantiated within the TTL, the message transitions to Failed with
//! "delivery timeout". Checked on gateway startup and periodically.

// TODO Delivery expiry checker
// tags: forge:inter-agent-communication
// Define expire_stale_messages(storage, default_ttl: Duration) → u64:
//   Delegates to AgentMessageStore.expire_stale_messages(default_ttl).
//   Returns the count of expired messages.
// Define a periodic expiry task (tokio interval) that runs on a configurable
// cadence (e.g. every 60 seconds) and calls expire_stale_messages.
// Wire into gateway startup (run once on start, then periodically).
// Default TTL: 24 hours (configurable via built-in constant for now,
// config surface deferred to Phase 05).
// Success: a pending message whose creation time exceeds the TTL transitions
// to Failed with "delivery timeout". Expiry is checked on startup and
// periodically during operation.
// ---

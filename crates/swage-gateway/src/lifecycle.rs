//! Gateway lifecycle management.
//!
//! Tracks the last time any significant activity was observed — HTTP requests,
//! WebSocket messages, and active tool executions. Presence and operator
//! surfaces use this to report recent gateway activity.
//!
//! Activity is defined narrowly: data must actually flow through a connection.
//! An open but idle WebSocket does not count; the first message through it does.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Tracks the timestamp of the last significant gateway activity.
///
/// Timestamps are stored as Unix epoch seconds in an `AtomicU64`. `Relaxed`
/// ordering is intentional — we need only approximate recency, not
/// happens-before guarantees. The tracker is `Send + Sync` and cheap to
/// wrap in an `Arc` for sharing across handler tasks.
pub struct ActivityTracker {
    last_activity_secs: AtomicU64,
}

impl ActivityTracker {
    /// Creates a new tracker, recording the current time as the baseline activity.
    pub fn new() -> Self {
        Self {
            last_activity_secs: AtomicU64::new(current_epoch_secs()),
        }
    }

    /// Records the current time as the most recent activity.
    ///
    /// Cheap: a single atomic store with `Relaxed` ordering.
    pub fn record_activity(&self) {
        self.last_activity_secs
            .store(current_epoch_secs(), Ordering::Relaxed);
    }

    /// Returns the number of whole seconds elapsed since the last recorded activity.
    ///
    /// Uses saturating subtraction to stay at zero when the wall clock runs
    /// backwards (e.g. NTP correction shortly after boot).
    pub fn seconds_since_last_activity(&self) -> u64 {
        let last = self.last_activity_secs.load(Ordering::Relaxed);
        current_epoch_secs().saturating_sub(last)
    }
}

impl Default for ActivityTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Current Unix epoch time in whole seconds.
fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    #[test]
    fn new_records_current_time() {
        let tracker = ActivityTracker::new();
        // A freshly created tracker should show near-zero idle time.
        assert!(
            tracker.seconds_since_last_activity() < 5,
            "expected near-zero idle time after construction; got {} s",
            tracker.seconds_since_last_activity()
        );
    }

    #[test]
    fn record_activity_resets_elapsed_time() {
        let tracker = ActivityTracker::new();

        // Backdate the stored timestamp to simulate 60 seconds of inactivity.
        tracker
            .last_activity_secs
            .store(current_epoch_secs().saturating_sub(60), Ordering::Relaxed);

        assert!(
            tracker.seconds_since_last_activity() >= 59,
            "elapsed should reflect the backdated timestamp; got {} s",
            tracker.seconds_since_last_activity()
        );

        // record_activity should reset elapsed to near-zero.
        tracker.record_activity();
        assert!(
            tracker.seconds_since_last_activity() < 5,
            "elapsed should be near-zero after recording activity; got {} s",
            tracker.seconds_since_last_activity()
        );
    }

    #[test]
    fn seconds_since_last_activity_does_not_decrease() {
        let tracker = ActivityTracker::new();
        let first = tracker.seconds_since_last_activity();
        let second = tracker.seconds_since_last_activity();
        // May be equal (sub-second precision) but must never go backwards.
        assert!(
            second >= first,
            "elapsed time must not decrease ({first} → {second})"
        );
    }

    #[test]
    fn default_impl_matches_new() {
        // Default::default() should behave identically to ActivityTracker::new().
        let tracker: ActivityTracker = Default::default();
        assert!(tracker.seconds_since_last_activity() < 5);
    }
}

//! Safety / leak detection — re-exported from `swage-safety`.
//!
//! [`LeakDetector`] scans text for credential patterns and redacts
//! matches before they leave the agent process.

pub use swage_safety::*;

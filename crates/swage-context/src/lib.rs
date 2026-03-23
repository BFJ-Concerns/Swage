//! Context assembly engine for the Swage platform.
//!
//! Accepts typed content elements, applies token budgets and compaction,
//! and returns assembled context with a structured report. The assembly
//! engine is shared between the gateway and agent runtimes.

pub mod assembly;
pub mod compaction;
pub mod element;
pub mod history;
pub mod memory;
pub mod prompts;
pub mod report;
pub mod tokeniser;

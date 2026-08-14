//! Context assembly engine for the Swage platform.
//!
//! Accepts typed content elements, applies token budgets and compaction,
//! and returns assembled context with a structured report. The assembly
//! engine is shared between the gateway and agent runtimes.

pub mod assembly;
pub mod compaction;
pub mod delta_summarisation;
pub mod element;
pub mod history;
pub mod js_assembly;
pub mod memory;
pub mod orientation;
pub mod prompts;
pub mod report;
pub mod selective_eviction;
pub mod skills;
pub mod structured_extraction;
pub mod tokeniser;
pub mod working_norms;

//! Convenience prelude — most-needed types for basic agent setup.
//!
//! A single `use swage::prelude::*` brings in the types required to construct
//! and run a minimal agent. For the high-level builder, see [`crate::SwageBuilder`]
//! (added in the builder module).
//!
//! # Included
//!
//! | Type | Source |
//! |------|--------|
//! | [`Agent`] | `swage::agent` |
//! | [`AgentBuilder`] | `swage::agent` |
//! | [`ChatMessage`] | `swage::core` |
//! | [`Role`] | `swage::core` |
//! | [`LlmProvider`] | `swage::provider` |
//! | [`Tool`] | `swage::tool::tool` |

pub use crate::agent::{Agent, AgentBuilder};
pub use crate::builder::SwageBuilder;
pub use crate::core::{ChatMessage, Role};
pub use crate::provider::LlmProvider;
pub use crate::tool::tool::Tool;

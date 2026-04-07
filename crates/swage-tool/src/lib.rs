//! Tool system for the Swage framework.
//!
//! Provides the tool execution framework: trait definition, registry with
//! protected namespaces, permission enforcement, rate limiting, JSON schema
//! validation, parameter redaction, and leak detection integration.
//!
//! All tool invocations flow through the dispatch pipeline:
//! registry lookup → permission check → rate limit → schema validation →
//! execute → leak detection → assemble ToolOutput.

pub mod context;
pub mod delegate;
pub mod dispatch;
pub mod output;
pub mod params;
pub mod permission;
pub mod rate_limit;
pub mod registry;
pub mod tool;
pub mod validation;

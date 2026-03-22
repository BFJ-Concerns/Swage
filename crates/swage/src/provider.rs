//! LLM provider traits and implementations — re-exported from `swage-provider`.
//!
//! The [`LlmProvider`] trait is the primary abstraction. Concrete
//! implementations live in the `anthropic`, `openai`, and `google`
//! submodules. Reliability decorators (retry, circuit breaker, failover)
//! can be composed via [`builder::ProviderChainBuilder`].

pub use swage_provider::*;

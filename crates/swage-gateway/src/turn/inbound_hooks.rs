//! The inbound hooks a turn's user content passes through before it is
//! recorded.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: dispatching `BeforeInbound` over the raw content and
//! returning the canonical content with the hooks that modified it, or the
//! refusal a blocking hook raised. Recording the inbound audit and failing
//! the turn on a refusal stay the coordinator's.

use super::{Arc, GatewayError, HookEvent, HookOutcome, HookRegistry};

#[derive(Debug, Clone)]
pub(crate) struct BeforeInboundDispatchResult {
    pub canonical_content: String,
    pub modified_by_hooks: Vec<String>,
}

fn map_hook_block(reason: String) -> GatewayError {
    GatewayError::ConfigError(format!(
        "BeforeInbound hook blocked inbound content: {reason}"
    ))
}

pub(crate) async fn dispatch_before_inbound(
    hook_registry: Option<&Arc<HookRegistry>>,
    raw_content: &str,
) -> Result<BeforeInboundDispatchResult, GatewayError> {
    let Some(hook_registry) = hook_registry else {
        return Ok(BeforeInboundDispatchResult {
            canonical_content: raw_content.to_string(),
            modified_by_hooks: Vec::new(),
        });
    };

    let traced = hook_registry
        .dispatch_traced(HookEvent::BeforeInbound {
            content: raw_content.to_string(),
        })
        .await;

    match traced.outcome {
        HookOutcome::Continue => Ok(BeforeInboundDispatchResult {
            canonical_content: raw_content.to_string(),
            modified_by_hooks: traced.modified_by_hooks,
        }),
        HookOutcome::ContinueModified(modified) => match *modified {
            HookEvent::BeforeInbound { content } => Ok(BeforeInboundDispatchResult {
                canonical_content: content,
                modified_by_hooks: traced.modified_by_hooks,
            }),
            _ => Ok(BeforeInboundDispatchResult {
                canonical_content: raw_content.to_string(),
                modified_by_hooks: traced.modified_by_hooks,
            }),
        },
        HookOutcome::Block(reason) => Err(map_hook_block(reason)),
        HookOutcome::RequestApproval { .. } => Err(GatewayError::ConfigError(
            "BeforeInbound hooks cannot request approval".to_string(),
        )),
    }
}

#![allow(clippy::unwrap_used)]

use serde_json::json;
use swage_core::{ChatMessage, CompletionRequest, Role};
use swage_provider::{LlmProvider, ProviderConfig, google::GoogleProvider};

/// The gateway-facing provider payload must preserve the shared projection's
/// local-validation guidance when a provider cannot carry JSON Schema unions.
#[test]
fn gateway_rung_provider_payload_carries_anyof_and_allof_guidance() {
    let provider = GoogleProvider::from_config(ProviderConfig::new(
        "test-key",
        "gemini-test",
        "http://provider.invalid",
    ));
    let request = CompletionRequest {
        effort: None,
        messages: vec![ChatMessage::new(Role::User, "use the lookup tool")],
        model: None,
        max_tokens: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![json!({
            "name": "lookup",
            "description": "Look up a source.",
            "parameters": {
                "type": "object",
                "anyOf": [
                    {"required": ["url"]},
                    {"required": ["query"]}
                ],
                "allOf": [
                    {"required": ["source"]},
                    {"properties": {"source": {"type": "string"}}}
                ]
            }
        })],
    };

    let payload = provider.provider_request_snapshot(&request, true);
    let parameters = &payload["body"]["tools"][0]["functionDeclarations"][0]["parameters"];
    assert!(parameters.get("anyOf").is_none());
    assert!(parameters.get("allOf").is_none());
    let guidance = parameters["description"].as_str().unwrap();
    assert!(guidance.contains("anyOf (at least one of: requires `url`; or requires `query`)"));
    assert!(guidance.contains(
        "allOf (all of: requires `source`; and an additional locally validated alternative)"
    ));
}

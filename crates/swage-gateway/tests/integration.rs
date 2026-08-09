#[path = "integration/support.rs"]
mod support;

#[path = "integration/agent_task_authoring_integration.rs"]
mod agent_task_authoring_integration;
#[path = "integration/attachment_ingestion_integration.rs"]
mod attachment_ingestion_integration;
#[path = "integration/browser_journey_integration.rs"]
mod browser_journey_integration;
#[path = "integration/cli_run_integration.rs"]
mod cli_run_integration;
#[path = "integration/credential_governance_integration.rs"]
mod credential_governance_integration;
#[path = "integration/device_integration.rs"]
mod device_integration;
#[path = "integration/external_channel_gateway_integration.rs"]
mod external_channel_gateway_integration;
#[path = "integration/gateway_cli_integration.rs"]
mod gateway_cli_integration;
#[path = "integration/gateway_observability_integration.rs"]
mod gateway_observability_integration;
#[path = "integration/integration.rs"]
mod integration;
#[path = "integration/inter_agent_messaging_integration.rs"]
mod inter_agent_messaging_integration;
#[path = "integration/material_management_integration.rs"]
mod material_management_integration;
#[path = "integration/onboarding_recovery_integration.rs"]
mod onboarding_recovery_integration;
#[path = "integration/operator_audit_usage_integration.rs"]
mod operator_audit_usage_integration;
#[path = "integration/operator_control_integration.rs"]
mod operator_control_integration;
#[path = "integration/output_schema_validation_integration.rs"]
mod output_schema_validation_integration;
#[path = "integration/presence_liveness_integration.rs"]
mod presence_liveness_integration;
#[path = "integration/ptc_integration.rs"]
mod ptc_integration;
#[path = "integration/run_supervision_integration.rs"]
mod run_supervision_integration;
#[path = "integration/sandbox_entrypoint_integration.rs"]
mod sandbox_entrypoint_integration;
#[path = "integration/sandbox_observability_integration.rs"]
mod sandbox_observability_integration;
#[path = "integration/schedule_integration.rs"]
mod schedule_integration;

#[path = "integration/schedule_tools_integration.rs"]
mod schedule_tools_integration;
#[path = "integration/skills_endpoint_integration.rs"]
mod skills_endpoint_integration;
#[path = "integration/skills_integration.rs"]
mod skills_integration;
#[path = "integration/space_integration.rs"]
mod space_integration;
#[path = "integration/tool_approval_integration.rs"]
mod tool_approval_integration;
#[path = "integration/tui_cli_integration.rs"]
mod tui_cli_integration;
#[path = "integration/unified_turn_loop_integration.rs"]
mod unified_turn_loop_integration;
#[path = "integration/web_interface_integration.rs"]
mod web_interface_integration;

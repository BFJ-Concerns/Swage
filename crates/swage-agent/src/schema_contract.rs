//! Output schema contract override for inter-agent delegation.
//!
//! When a Delegate message includes an output schema, it overrides the target
//! agent's own [output] config for that delegation. The delegator defines the
//! contract, not the delegate.

// TODO Delegate output schema override
// tags: forge:inter-agent-communication
// Implement resolve_delegation_schema(delegate_payload, target_config) → OutputConfig:
//   1. If the Delegate payload includes an "output_schema" field:
//      - Use it as the output schema, overriding target's [output].schema
//      - Use the Delegate's max_retries if specified, else target's default
//   2. If no schema in the Delegate payload:
//      - Fall back to the target agent's own [output] config
//   3. If neither specifies a schema:
//      - No validation occurs (existing Phase 02 behaviour)
// The returned OutputConfig is used when constructing the target Agent via
// AgentBuilder — it's set as agent_config.output_schema.
// The Phase 02 validation infrastructure handles the actual validation and
// retry loop — this module only resolves which schema to use.
// Success: Delegate-specified schema overrides target's own config,
// missing delegate schema falls back to target config, no schema = no validation.
// ---

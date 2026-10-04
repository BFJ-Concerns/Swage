//! Choosing and running the context assembly a turn is served by.
//!
//! Placed here because it has one responsibility a reader can hold without
//! the coordinator: which assembly and which working-norms scaffold serve
//! this turn — each an experiment location whose decision lands in the audit
//! trail — and running the chosen assembly over the built elements. The
//! ordinary turn and the delegated turn select through this one seam.

use super::{
    AppState, ContextElement, GatewayError, Path, ScopeLocation, TiktokenTokeniser, Uuid,
    configuration_hierarchy, is_default_context_assembly,
};

pub(super) struct ContextChoices {
    pub(super) assembly_name: String,
    pub(super) working_norms_scaffold: swage_context::working_norms::WorkingNormsScaffold,
}

const WORKING_NORMS_AUTHORED_V1: &str = "authored-v1";
pub(super) const WORKING_NORMS_FAMILY_FALLBACK: &str = "family-fallback";

pub(super) async fn select_context_assembly(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    session_id: Uuid,
    turn_id: Uuid,
    call_override: Option<&str>,
) -> Result<ContextChoices, GatewayError> {
    if let Some(call_override) = call_override {
        swage_core::validate_context_assembly_name(call_override)
            .map_err(GatewayError::ConfigError)?;
    }
    // The assembly name is an experiment location: when variants are
    // registered for it, its first use draws which serves this session
    // (honouring the kill switch and forced pins), and that decision —
    // including "experimentation is off" — lands in the audit trail. The
    // report and every model-facing surface stay function-named; the audit
    // record is the only place the variant id appears.
    let configured_assembly = call_override
        .unwrap_or(&config.context.assembly)
        .trim()
        .to_owned();
    let assembly_variants = config
        .context
        .alternatives
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let decision = crate::experimentation::select_session_variant(
        state,
        &config.experimentation,
        crate::experimentation::LOCATION_CONTEXT_ASSEMBLY,
        &assembly_variants,
        &configured_assembly,
        session_id,
        Some(turn_id),
    )
    .await
    .map_err(|error| GatewayError::ConfigError(error.to_string()))?;

    // The working-norms text is its own experiment location, drawn beside
    // the assembly choice. The exact-model native scaffold is the default;
    // the established family text remains a registered alternative.
    let norms_variants = [WORKING_NORMS_FAMILY_FALLBACK];
    let norms_decision = crate::experimentation::select_session_variant(
        state,
        &config.experimentation,
        crate::experimentation::LOCATION_WORKING_NORMS,
        &norms_variants,
        WORKING_NORMS_AUTHORED_V1,
        session_id,
        Some(turn_id),
    )
    .await
    .map_err(|error| GatewayError::ConfigError(error.to_string()))?;
    let working_norms_scaffold = match norms_decision.variant.as_str() {
        WORKING_NORMS_FAMILY_FALLBACK => {
            swage_context::working_norms::WorkingNormsScaffold::FamilyFallback
        }
        _ => swage_context::working_norms::WorkingNormsScaffold::Native,
    };

    Ok(ContextChoices {
        assembly_name: decision.variant,
        working_norms_scaffold,
    })
}

pub(super) fn execute_context_assembly(
    assembly_name: &str,
    config: &swage_config::ResolvedConfig,
    elements: &[Box<dyn ContextElement>],
    tokeniser: &TiktokenTokeniser,
    swage_root: Option<&Path>,
    scope: &ScopeLocation,
    agent_id: &str,
    variables: &std::collections::BTreeMap<String, String>,
) -> swage_context::report::AssemblyOutput {
    let assembly_name = assembly_name.trim();
    if is_default_context_assembly(assembly_name) {
        return swage_context::assembly::default_assembly_with_variables(
            elements,
            &config.context,
            tokeniser,
            variables,
        );
    }

    let search_levels = swage_root
        .map(|root| configuration_hierarchy(root, scope, Some(agent_id)))
        .unwrap_or_default();
    let level_refs: Vec<(swage_config::HierarchyLevel, &Path)> = search_levels
        .iter()
        .map(|(level, path)| (level.clone(), path.as_path()))
        .collect();
    let source_path = swage_context::js_assembly::resolve_js_assembly(assembly_name, &level_refs);
    swage_context::js_assembly::assemble_named(
        assembly_name,
        source_path.as_deref(),
        elements,
        &config.context,
        tokeniser,
        variables,
    )
}

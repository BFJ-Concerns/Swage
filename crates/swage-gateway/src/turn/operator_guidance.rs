use super::{HierarchyLevel, Path, PromptRule, TrustedWorkspacePolicy};

/// Operator-authored guidance rules for a turn: the standard agent-guidance
/// file (`AGENTS.md`) read from the Swage config root, then from each
/// trusted, readable workspace root — global before workspace, so workspace
/// guidance refines the operator's baseline rather than the reverse.
///
/// An untrusted attachment contributes nothing: workspace content enters the
/// model's instructions only once the operator has trusted the attachment.
/// Absent files contribute nothing and are not an error; an unreadable file
/// is logged and skipped — guidance never fails a turn.
pub(crate) fn operator_guidance_rules(
    swage_root: Option<&Path>,
    workspace_policy: &TrustedWorkspacePolicy,
) -> Vec<PromptRule> {
    let mut rules = Vec::new();
    if let Some(root) = swage_root {
        push_guidance_rule(&mut rules, root, HierarchyLevel::Global);
    }
    for root in trusted_readable_roots(workspace_policy) {
        // A workspace is attached to the space, so its guidance carries the
        // Space level in the prompts element's provenance summary.
        push_guidance_rule(&mut rules, root, HierarchyLevel::Space);
    }
    rules
}

/// Workspace roots whose guidance may enter context: the active workspace
/// first when trusted and readable, then the trusted readable attachment
/// grants, deduplicated by path.
fn trusted_readable_roots(policy: &TrustedWorkspacePolicy) -> Vec<&Path> {
    let mut roots: Vec<&Path> = Vec::new();
    if let Some(active) = &policy.active_workspace
        && active.is_trusted
        && active.can_read
    {
        roots.push(active.path.as_path());
    }
    for attachment in &policy.attachments {
        if attachment.is_trusted
            && attachment.can_read
            && !roots.contains(&attachment.path.as_path())
        {
            roots.push(attachment.path.as_path());
        }
    }
    roots
}

fn push_guidance_rule(rules: &mut Vec<PromptRule>, root: &Path, source_level: HierarchyLevel) {
    match swage_config::agent_guidance::read_agent_guidance(root) {
        Ok(Some(content)) if !content.trim().is_empty() => rules.push(PromptRule {
            priority: 0,
            content,
            source_level,
            file_path: root.join(swage_config::agent_guidance::AGENT_GUIDANCE_FILE),
        }),
        Ok(_) => {}
        Err(problem) => {
            tracing::warn!(
                path = %problem.file_path.display(),
                "failed to read agent guidance file; continuing without it"
            );
        }
    }
}

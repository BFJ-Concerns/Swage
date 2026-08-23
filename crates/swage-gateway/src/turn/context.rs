use super::{
    ActiveSkill, AppState, ChatMessage, ContextElement, HierarchyLevel, HistoryElement,
    MemoryElement, Path, PathBuf, PromptRule, PromptsElement, SkillManager, SkillsElement, Uuid,
};
use swage_context::briefing::BriefingElement;
use swage_context::named_element::discover_named_elements;
use swage_context::orientation::{
    AvailableSkill, ChannelOrientation, OrientationElement, SessionEnvironment,
};
use swage_context::working_norms::{WorkingNormsElement, WorkingNormsScaffold};

#[cfg(test)]
tokio::task_local! {
    static SPACE_MEMORY_LOAD_COUNT: std::cell::Cell<usize>;
}

/// Build the context assembly elements for a turn.
///
/// Constructs typed elements from the resolved turn inputs:
/// - `OrientationElement` — baseline identity, place, and capability; always
///   produces content, so an empty configuration still orients the session
/// - `PromptsElement` — from the merged instruction string, followed by the
///   operator-guidance rules (the standard agent-guidance files, resolved by
///   `operator_guidance_rules` from the config root and trusted workspaces)
/// - `SkillsElement`  — from the active skill set
/// - `MemoryElement`  — only for a named assembly, from pre-loaded space
///   memory content (may be empty)
/// - `HistoryElement` — from the thread's message history
///
/// The returned vec is suitable for passing to `swage_context::assembly::assemble()`.
///
/// When instructions are empty the `PromptsElement` produces no messages;
/// the orientation baseline is what keeps the assembled context non-empty.
/// When `memory` is present with empty content the `MemoryElement` produces no
/// messages but remains available for the named assembly to reference. The
/// default assembly passes `None` so it does not construct an element it would
/// discard.
pub(crate) fn build_assembly_elements(
    instructions: &str,
    operator_guidance: Vec<PromptRule>,
    active_skills: Vec<ActiveSkill>,
    history: Vec<ChatMessage>,
    memory: Option<(String, String)>,
    environment: SessionEnvironment,
    available_skills: Vec<AvailableSkill>,
    model_id: &str,
    working_norms_scaffold: WorkingNormsScaffold,
    hierarchy: &[(HierarchyLevel, PathBuf)],
    requested_elements: Option<&[String]>,
) -> Vec<Box<dyn ContextElement>> {
    // Construct a single PromptRule from the resolved instructions string.
    // The config resolver has already merged hierarchy levels and assembled
    // the text — we wrap it as a single Global-level rule so the PromptsElement
    // produces the expected system message.
    let mut rules = if instructions.is_empty() {
        vec![]
    } else {
        vec![PromptRule {
            priority: 0,
            content: instructions.to_string(),
            source_level: HierarchyLevel::Global,
            file_path: PathBuf::from("resolved-instructions"),
        }]
    };
    // Operator guidance extends the resolved baseline rather than replacing
    // it: the rules arrive pre-ordered (global config root before trusted
    // workspace roots) and append after the resolved instructions.
    rules.extend(operator_guidance);

    let mut elements: Vec<Box<dyn ContextElement>> = vec![
        Box::new(OrientationElement::new(environment, available_skills)),
        // Models arrive over the proxy bare: the norms their own harness
        // would have supplied come from the assembly, keyed by the coming
        // turn's model family.
        Box::new(WorkingNormsElement::for_model(
            model_id,
            working_norms_scaffold,
        )),
        Box::new(PromptsElement::new(rules)),
        Box::new(SkillsElement::new(active_skills)),
    ];
    let hierarchy_refs = hierarchy
        .iter()
        .map(|(level, path)| (level.clone(), path.as_path()))
        .collect::<Vec<_>>();
    let named_elements = match requested_elements {
        Some(names) => names
            .iter()
            .filter_map(|name| match swage_context::named_element::resolve_named_element(
                name,
                &hierarchy_refs,
            ) {
                Ok(Some(resolved)) => Some(resolved),
                Ok(None) => {
                    tracing::warn!(element = %name, "requested context element was not found");
                    None
                }
                Err(error) => {
                    tracing::warn!(element = %name, %error, "requested context element could not be loaded");
                    None
                }
            })
            .collect(),
        None => discover_named_elements(&hierarchy_refs).unwrap_or_else(|error| {
            tracing::warn!(%error, "named context elements could not be loaded");
            Vec::new()
        }),
    };
    elements.extend(
        named_elements
            .into_iter()
            .map(|resolved| Box::new(BriefingElement::new(resolved)) as Box<dyn ContextElement>),
    );
    if let Some((content, space_name)) = memory {
        elements.push(Box::new(MemoryElement::new(content, space_name)));
    }
    elements.push(Box::new(HistoryElement::new(history)));
    elements
}

/// Load memory only when the already-selected assembly can use it.
///
/// The loader argument keeps the disposition directly testable: default turns
/// must not invoke the filesystem seam at all.
pub(crate) async fn load_memory_for_assembly<F, Fut>(
    assembly_name: &str,
    loader: F,
) -> Option<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = String>,
{
    if is_default_context_assembly(assembly_name) {
        None
    } else {
        Some(loader().await)
    }
}

pub(crate) fn is_default_context_assembly(assembly_name: &str) -> bool {
    let assembly_name = assembly_name.trim();
    assembly_name.is_empty() || assembly_name == "default"
}

/// Build the session-environment half of orientation from what the turn
/// already resolved: the space row, the sandbox workspace root, and the
/// session's channel origin when it speaks through one.
pub(crate) fn build_session_environment(
    space: &swage_storage::Space,
    working_directory: Option<String>,
    session: &swage_storage::Session,
) -> SessionEnvironment {
    let session_metadata = session.metadata.as_ref();
    // A channel session's origin was stamped at session creation; surface the
    // integration by name so the model knows whose voice it uses. Constraints
    // are stated per integration — Telegram is the only channel today.
    let channel = session_metadata
        .and_then(|metadata| metadata.get("channel_origin"))
        .and_then(|origin| origin.get("integration"))
        .and_then(|integration| integration.as_str())
        .map(|integration| {
            let constraints = if integration.eq_ignore_ascii_case("telegram") {
                "Replies arrive as phone messages: keep them short, prefer \
                 plain text, and avoid markdown tables and long code blocks."
            } else {
                "Prefer short plain-text replies unless the operator asks \
                 for more."
            };
            ChannelOrientation {
                channel_name: integration.to_string(),
                constraints: constraints.to_string(),
            }
        });

    SessionEnvironment {
        space_name: if space.is_default {
            String::new()
        } else {
            space.name.clone()
        },
        space_description: space.description.clone(),
        working_directory,
        // The session row's creation time, not the turn's clock: the block
        // renders identically on every turn of the session, keeping the
        // assembled prefix stable.
        started_at: session.created_at,
        channel,
    }
}

/// List the skills a session could activate — the catalogue for the
/// orientation element, distinct from the active set whose instructions the
/// skills element carries.
pub(crate) fn available_skills_for_agent(
    space_slug: &str,
    agent_id: &str,
    swage_root: Option<&Path>,
) -> Vec<AvailableSkill> {
    let Some(root) = swage_root else {
        return vec![];
    };
    let hierarchy = configuration_hierarchy(root, space_slug, agent_id);
    let space_dir = &hierarchy[1].1;
    let agent_dir_opt = hierarchy[2].1.is_dir().then_some(hierarchy[2].1.as_path());
    let (available, _warnings) = SkillManager::discover_available(root, space_dir, agent_dir_opt);
    available
        .into_iter()
        .map(|skill| AvailableSkill {
            name: skill.name,
            trigger: skill.description,
        })
        .collect()
}

/// Construct the shared global → space → agent config hierarchy.
pub(crate) fn configuration_hierarchy(
    root: &Path,
    space_slug: &str,
    agent_id: &str,
) -> Vec<(HierarchyLevel, PathBuf)> {
    let space = root.join("spaces").join(space_slug);
    let agent = space.join("agents").join(agent_id);
    vec![
        (HierarchyLevel::Global, root.to_path_buf()),
        (HierarchyLevel::Space, space),
        (HierarchyLevel::Agent, agent),
    ]
}

/// Load space memory content by reading all `.md` files from the space's
/// memory directory (`{swage_root}/spaces/{space_slug}/memory/`).
///
/// Files are read in alphabetical filename order and joined with a blank line
/// between entries. Returns an empty string when the directory is absent or
/// contains no `.md` files, so callers receive a valid `String` in all cases.
pub(crate) async fn load_space_memory(swage_root: &Path, space_slug: &str) -> String {
    #[cfg(test)]
    SPACE_MEMORY_LOAD_COUNT
        .try_with(|count| count.set(count.get() + 1))
        .ok();

    let memory_dir = swage_root.join("spaces").join(space_slug).join("memory");

    let mut entries = match tokio::fs::read_dir(&memory_dir).await {
        Ok(rd) => rd,
        // Missing directory is the common case for new spaces.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
        Err(error) => {
            tracing::warn!(
                path = %memory_dir.display(),
                %error,
                "failed to read space memory directory"
            );
            return String::new();
        }
    };

    // Collect .md paths and sort alphabetically so loading order is stable.
    let mut paths: Vec<PathBuf> = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                    paths.push(path);
                }
            }
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    path = %memory_dir.display(),
                    %error,
                    "failed to enumerate space memory directory"
                );
                return String::new();
            }
        }
    }
    paths.sort();

    let mut parts = Vec::new();
    for path in paths {
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                if !content.trim().is_empty() {
                    parts.push(content);
                }
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to read space memory file");
            }
        }
    }

    parts.join("\n\n")
}

#[cfg(test)]
pub(crate) async fn observe_space_memory_loads<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    SPACE_MEMORY_LOAD_COUNT
        .scope(std::cell::Cell::new(0), async move {
            let output = future.await;
            let count = SPACE_MEMORY_LOAD_COUNT.with(std::cell::Cell::get);
            (output, count)
        })
        .await
}

pub(crate) fn resolve_swage_root(state: &AppState) -> &Path {
    &state.config_root
}

pub(crate) async fn load_active_skills_for_agent(
    state: &AppState,
    session_id: Uuid,
    space_slug: &str,
    agent_id: &str,
    swage_root: Option<&Path>,
    include_user_activated: bool,
    declared_skills: &[String],
) -> Vec<ActiveSkill> {
    let Some(root) = swage_root else {
        return vec![];
    };
    let hierarchy = configuration_hierarchy(root, space_slug, agent_id);
    let space_dir = &hierarchy[1].1;
    let agent_dir_opt = hierarchy[2].1.is_dir().then_some(hierarchy[2].1.as_path());
    let (available, _warnings) = SkillManager::discover_available(root, space_dir, agent_dir_opt);

    // Agent-level skills are automatic for the current agent context. Store
    // them idempotently so the normal activation ordering and filtering path is
    // shared by foreground and delegated turns.
    if let Err(e) =
        SkillManager::auto_activate_agent_skills(session_id, &available, state.storage.as_ref())
            .await
    {
        tracing::warn!(
            error = ?e,
            "failed to auto-activate agent-level skills; proceeding"
        );
    }

    let mut active =
        match SkillManager::build_active_skills(session_id, &available, state.storage.as_ref())
            .await
        {
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "failed to load active skills; proceeding without skills"
                );
                vec![]
            }
            Ok(infos) => infos
                .into_iter()
                .filter(|info| include_user_activated || info.is_agent_level)
                .map(|info| ActiveSkill {
                    name: info.name,
                    instructions: info.instructions,
                    source_directory: info.source_directory,
                    level: info.level,
                    is_agent_level: info.is_agent_level,
                    activation_order: info.activation_order as u32,
                })
                .collect(),
        };

    // Definition declarations are resolved configuration for this turn, not
    // persistent user activations. Add their instructions directly so the
    // current definition applies equally to foreground and delegated turns.
    for skill_name in declared_skills {
        if active.iter().any(|skill| skill.name == *skill_name) {
            continue;
        }
        let Some(skill) = available.iter().find(|skill| skill.name == *skill_name) else {
            tracing::warn!(
                agent_id,
                skill_name,
                "agent definition declares an unavailable skill; ignoring it"
            );
            continue;
        };
        active.push(ActiveSkill {
            name: skill.name.clone(),
            instructions: skill.instructions.clone(),
            source_directory: skill
                .source_path
                .parent()
                .expect("skill source has a parent directory")
                .to_path_buf(),
            level: skill.level.clone(),
            is_agent_level: skill.level == HierarchyLevel::Agent,
            activation_order: active.len() as u32,
        });
    }

    active
}

#[cfg(test)]
mod shipped_skill_tests {
    use super::*;

    #[test]
    fn fresh_orientation_catalogue_contains_all_shipped_skills() {
        let root = tempfile::tempdir().expect("temporary root");
        let available = available_skills_for_agent("default", "gateway", Some(root.path()));
        assert_eq!(available.len(), 5);
        assert_eq!(
            available
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            [
                "assembly",
                "commission",
                "maestro",
                "project-docs",
                "workflow-authoring"
            ]
        );
    }
}

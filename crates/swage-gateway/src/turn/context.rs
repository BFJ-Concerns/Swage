use super::{
    ActiveSkill, AppState, ChatMessage, ContextElement, HierarchyLevel, HistoryElement,
    MemoryElement, Path, PathBuf, PromptRule, PromptsElement, SkillManager, SkillsElement, Uuid,
};

pub(super) fn delegated_target_instructions(
    target_instructions: &str,
    available_skills: &[swage_config::skill::SkillDefinition],
) -> String {
    let agent_skills = available_skills
        .iter()
        .enumerate()
        .filter(|(_, skill)| skill.level == HierarchyLevel::Agent)
        .map(|(order, skill)| ActiveSkill {
            name: skill.name.clone(),
            instructions: skill.instructions.clone(),
            level: skill.level.clone(),
            is_agent_level: true,
            activation_order: order as u32,
        })
        .collect::<Vec<_>>();
    let skill_context = SkillsElement::new(agent_skills)
        .content()
        .into_iter()
        .map(|message| message.text_content())
        .collect::<Vec<_>>()
        .join("\n\n");

    match (target_instructions.trim(), skill_context.trim()) {
        ("", "") => String::new(),
        (instructions, "") => instructions.to_string(),
        ("", skills) => format!("Active Skills\n\n{skills}"),
        (instructions, skills) => format!("{instructions}\n\nActive Skills\n\n{skills}"),
    }
}

pub(crate) fn delegated_target_instructions_for_bench(
    target_instructions: &str,
    bench_slug: &str,
    target_agent_id: &str,
) -> String {
    resolve_swage_root()
        .map(|root| {
            let bench_dir = root.join("benches").join(bench_slug);
            let agent_dir = bench_dir.join("agents").join(target_agent_id);
            let agent_dir_opt = if agent_dir.is_dir() {
                Some(agent_dir.as_path())
            } else {
                None
            };
            let (available, _warnings) =
                SkillManager::discover_available(&root, &bench_dir, agent_dir_opt);
            delegated_target_instructions(target_instructions, &available)
        })
        .unwrap_or_else(|| target_instructions.to_string())
}

/// Build the context assembly elements for a turn.
///
/// Constructs typed elements from the resolved turn inputs:
/// - `PromptsElement` — from the merged instruction string
/// - `SkillsElement`  — from the active skill set
/// - `MemoryElement`  — from pre-loaded bench memory content (may be empty)
/// - `HistoryElement` — from the thread's message history
///
/// The returned vec is suitable for passing to `swage_context::assembly::assemble()`.
///
/// When instructions are empty the `PromptsElement` produces no messages.
/// When `memory_content` is empty the `MemoryElement` produces no messages
/// but is still present in the vec so custom assemblies can reference it.
pub(crate) fn build_assembly_elements(
    instructions: &str,
    active_skills: Vec<ActiveSkill>,
    history: Vec<ChatMessage>,
    memory_content: String,
    bench_name: String,
) -> Vec<Box<dyn ContextElement>> {
    // Construct a single PromptRule from the resolved instructions string.
    // The config resolver has already merged hierarchy levels and assembled
    // the text — we wrap it as a single Global-level rule so the PromptsElement
    // produces the expected system message.
    let rules = if instructions.is_empty() {
        vec![]
    } else {
        vec![PromptRule {
            priority: 0,
            content: instructions.to_string(),
            source_level: HierarchyLevel::Global,
            file_path: PathBuf::from("resolved-instructions"),
        }]
    };

    vec![
        Box::new(PromptsElement::new(rules)),
        Box::new(SkillsElement::new(active_skills)),
        Box::new(MemoryElement::new(memory_content, bench_name)),
        Box::new(HistoryElement::new(history)),
    ]
}

/// Load bench memory content by reading all `.md` files from the bench's
/// memory directory (`{swage_root}/benches/{bench_slug}/memory/`).
///
/// Files are read in alphabetical filename order and joined with a blank line
/// between entries. Returns an empty string when the directory is absent or
/// contains no `.md` files, so callers receive a valid `String` in all cases.
pub(crate) async fn load_bench_memory(swage_root: &Path, bench_slug: &str) -> String {
    let memory_dir = swage_root.join("benches").join(bench_slug).join("memory");

    let mut entries = match tokio::fs::read_dir(&memory_dir).await {
        Ok(rd) => rd,
        // Missing directory is the common case for new benches.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
        Err(error) => {
            tracing::warn!(
                path = %memory_dir.display(),
                %error,
                "failed to read bench memory directory"
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
                    "failed to enumerate bench memory directory"
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
                tracing::warn!(path = %path.display(), %error, "failed to read bench memory file");
            }
        }
    }

    parts.join("\n\n")
}

pub(crate) fn resolve_swage_root() -> Option<PathBuf> {
    match swage_config::discovery::resolve_root() {
        Ok(root) => Some(root),
        Err(error) => {
            tracing::warn!(
                error = %error.kind,
                "failed to resolve swage root; proceeding without skills, memory, or custom assembly"
            );
            None
        }
    }
}

pub(crate) async fn load_active_skills_for_agent(
    state: &AppState,
    session_id: Uuid,
    bench_slug: &str,
    agent_id: &str,
    swage_root: Option<&Path>,
    include_user_activated: bool,
) -> Vec<ActiveSkill> {
    let Some(root) = swage_root else {
        return vec![];
    };
    let bench_dir = root.join("benches").join(bench_slug);
    let agent_dir = bench_dir.join("agents").join(agent_id);
    let agent_dir_opt = if agent_dir.is_dir() {
        Some(agent_dir.as_path())
    } else {
        None
    };
    let (available, _warnings) = SkillManager::discover_available(root, &bench_dir, agent_dir_opt);

    // Agent-level skills are automatic for the current agent context. Store
    // them idempotently so the normal activation ordering and filtering path is
    // shared by foreground, delegated, and topology turns.
    if let Err(e) =
        SkillManager::auto_activate_agent_skills(session_id, &available, state.storage.as_ref())
            .await
    {
        tracing::warn!(
            error = ?e,
            "failed to auto-activate agent-level skills; proceeding"
        );
    }

    match SkillManager::build_active_skills(session_id, &available, state.storage.as_ref()).await {
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
                level: info.level,
                is_agent_level: info.is_agent_level,
                activation_order: info.activation_order as u32,
            })
            .collect(),
    }
}

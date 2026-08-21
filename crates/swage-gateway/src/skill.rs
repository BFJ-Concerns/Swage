//! Skill management — discovery, activation, deactivation, and slash command
//! parsing for the gateway.
//!
//! The `SkillManager` coordinates between the config-level skill discovery
//! (swage-config) and the storage-backed activation state (swage-storage),
//! providing the gateway with a single entry point for all skill operations.

// ---------------------------------------------------------------------------
// Slash command parser
// ---------------------------------------------------------------------------

/// Result of parsing a user message for a slash-command prefix.
#[derive(Debug, PartialEq)]
pub enum SlashCommandResult {
    /// Input does not look like a slash command — pass it through unchanged.
    NotACommand,
    /// Input starts with `/<skill-name>`.
    ///
    /// `remaining_text` holds any text that follows the skill name, stripped of
    /// leading whitespace. It is `None` when the slash command is bare (no
    /// trailing text), and `Some(text)` only when there is non-empty text after
    /// the skill name — the caller should forward it as a user message.
    SkillActivation {
        skill_name: String,
        remaining_text: Option<String>,
    },
}

/// Parse a user message for a leading slash-command prefix.
///
/// A valid slash command must:
/// - Start with `/` as the very first character
/// - Be followed by a name matching `[a-z0-9-]+` (lowercase, digits, hyphens)
///
/// If the candidate name does not match — e.g. it contains uppercase letters,
/// forward slashes (file paths), or is empty — the input is treated as a plain
/// message and `NotACommand` is returned.
///
/// This function performs **syntax parsing only**. It does not check whether
/// the named skill actually exists; that resolution is the caller's responsibility.
pub fn parse_slash_command(input: &str) -> SlashCommandResult {
    // Must start with `/`.
    let after_slash = match input.strip_prefix('/') {
        Some(rest) => rest,
        None => return SlashCommandResult::NotACommand,
    };

    // Split at the first space to separate the candidate name from any trailing text.
    let (candidate, tail) = match after_slash.find(' ') {
        Some(pos) => (&after_slash[..pos], &after_slash[pos + 1..]),
        None => (after_slash, ""),
    };

    // Reject empty candidates (bare `/` or `/ spaces`).
    if candidate.is_empty() {
        return SlashCommandResult::NotACommand;
    }

    // Validate: only lowercase ASCII letters, digits, and hyphens are allowed.
    // Anything else (uppercase, `/`, `.`, etc.) is treated as a non-command.
    if !candidate
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        return SlashCommandResult::NotACommand;
    }

    let remaining_text = if tail.is_empty() {
        None
    } else {
        Some(tail.to_string())
    };

    SlashCommandResult::SkillActivation {
        skill_name: candidate.to_string(),
        remaining_text,
    }
}

use std::{collections::HashSet, path::Path};

use swage_config::model::HierarchyLevel;
use swage_config::skill::SkillDefinition;
use swage_context::tokeniser::Tokeniser;
use swage_storage::SkillActivationStore;
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Information about an active skill, suitable for constructing a `SkillsElement`.
///
/// Contains all data the assembly engine needs to render skill instructions
/// into context, including the ordering information required to sort agent-level
/// skills ahead of user-activated ones.
#[derive(Debug, Clone)]
pub struct ActiveSkillInfo {
    /// Canonical skill name.
    pub name: String,
    /// Full markdown instruction body from the skill definition.
    pub instructions: String,
    /// Directory against which relative paths in the instructions resolve.
    pub source_directory: std::path::PathBuf,
    /// Hierarchy level where this skill was discovered.
    pub level: HierarchyLevel,
    /// Convenience flag: true when `level` is `HierarchyLevel::Agent`.
    pub is_agent_level: bool,
    /// Zero-based position in the activation sequence (earlier = lower index).
    pub activation_order: usize,
}

/// Result of a skill activation attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum SkillActivationResult {
    /// The skill was not previously active and has been stored.
    Activated {
        /// Canonical skill name.
        name: String,
        /// Display name (falls back to the canonical name if not set in frontmatter).
        display_name: String,
        /// Short description from the skill's frontmatter.
        description: String,
        /// Token count for the skill's instruction body.
        token_count: usize,
    },
    /// The skill was already active for this session — no change was made.
    AlreadyActive,
}

/// Errors produced by skill management operations.
#[derive(Debug)]
pub enum SkillError {
    /// The requested skill name was not found in the discovered set.
    NotFound(String),
    /// Agent-level skills are automatically active and cannot be manually deactivated.
    AgentLevelCannotDeactivate(String),
    /// A storage operation failed.
    Storage(swage_core::StorageError),
}

impl From<swage_core::StorageError> for SkillError {
    fn from(err: swage_core::StorageError) -> Self {
        SkillError::Storage(err)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SkillManager
// ─────────────────────────────────────────────────────────────────────────────

/// Central coordination type for skill operations in the gateway.
///
/// Stateless utility — all operations receive their dependencies as arguments.
/// This makes the type trivially testable and avoids shared mutable state.
pub struct SkillManager;

impl SkillManager {
    /// Discover skills available to a conversation.
    ///
    /// Builds a three-level hierarchy from `root` (Global), `space_dir`
    /// (Space), and optionally `agent_dir` (Agent), then delegates to
    /// [`swage_config::skill::discover_skills`] for scanning and override
    /// resolution.
    pub fn discover_available(
        root: &Path,
        space_dir: &Path,
        agent_dir: Option<&Path>,
    ) -> (
        Vec<swage_config::skill::SkillDefinition>,
        Vec<swage_config::skill::SkillWarning>,
    ) {
        let mut levels: Vec<(HierarchyLevel, &Path)> = vec![
            (HierarchyLevel::Global, root),
            (HierarchyLevel::Space, space_dir),
        ];
        if let Some(agent) = agent_dir {
            levels.push((HierarchyLevel::Agent, agent));
        }
        let (mut authored, mut warnings) = swage_config::skill::discover_skills(&levels);
        let authored_names = authored
            .iter()
            .map(|skill| skill.name.clone())
            .collect::<HashSet<_>>();
        for shipped in crate::shipped_skills::SHIPPED_SKILLS {
            if authored_names.contains(shipped.name) {
                continue;
            }
            let source_path =
                crate::shipped_skill_projection::skill_dir(root, shipped.name).join("SKILL.md");
            match swage_config::skill::parse_skill_source(
                shipped.source,
                &source_path,
                shipped.name,
                HierarchyLevel::Global,
            ) {
                Ok(mut definition) => {
                    definition.shipped = true;
                    authored.push(definition);
                }
                Err(warning) => warnings.push(warning),
            }
        }
        authored.sort_by(|left, right| {
            let specificity = |level: &HierarchyLevel| match level {
                HierarchyLevel::Agent => 0,
                HierarchyLevel::Space => 1,
                HierarchyLevel::Global => 2,
            };
            specificity(&left.level)
                .cmp(&specificity(&right.level))
                .then_with(|| left.name.cmp(&right.name))
        });
        (authored, warnings)
    }

    /// Activate a skill for a session.
    ///
    /// Returns [`SkillActivationResult::NotFound`] if the skill name is not in
    /// `available`, [`SkillActivationResult::AlreadyActive`] if already stored,
    /// or [`SkillActivationResult::Activated`] on success with skill metadata
    /// and the token count of the instruction body.
    pub async fn activate(
        session_id: Uuid,
        skill_name: &str,
        available: &[SkillDefinition],
        storage: &dyn SkillActivationStore,
        tokeniser: &dyn Tokeniser,
    ) -> Result<SkillActivationResult, SkillError> {
        let skill = available
            .iter()
            .find(|s| s.name == skill_name)
            .ok_or_else(|| SkillError::NotFound(skill_name.to_string()))?;

        // Count tokens and clone all metadata synchronously — before the first
        // await — so we don't hold &dyn Tokeniser across await points (Tokeniser
        // doesn't require Sync, so the reference would make the future !Send).
        let token_count = tokeniser.count_text(&skill.instructions);
        let display_name = skill
            .display_name
            .clone()
            .unwrap_or_else(|| skill.name.clone());
        let name = skill.name.clone();
        let description = skill.description.clone();

        // Check before storing to give a clean AlreadyActive signal without
        // relying on the INSERT OR IGNORE idempotency as the primary feedback path.
        let active = storage.list_active_skills(session_id).await?;
        if active.iter().any(|r| r.skill_name == skill_name) {
            return Ok(SkillActivationResult::AlreadyActive);
        }

        storage.activate_skill(session_id, skill_name).await?;

        Ok(SkillActivationResult::Activated {
            name,
            display_name,
            description,
            token_count,
        })
    }

    /// Deactivate a skill for a session.
    ///
    /// Agent-level skills are always active and cannot be removed — attempting
    /// to deactivate one returns [`SkillError::AgentLevelCannotDeactivate`].
    /// For other skills, removes the activation record from storage (a no-op
    /// if the skill is not currently active).
    pub async fn deactivate(
        session_id: Uuid,
        skill_name: &str,
        available: &[SkillDefinition],
        storage: &dyn SkillActivationStore,
    ) -> Result<(), SkillError> {
        if let Some(skill) = available.iter().find(|s| s.name == skill_name)
            && skill.level == HierarchyLevel::Agent
        {
            return Err(SkillError::AgentLevelCannotDeactivate(
                skill_name.to_string(),
            ));
        }

        storage.deactivate_skill(session_id, skill_name).await?;
        Ok(())
    }

    /// Activate all agent-level skills for a new conversation.
    ///
    /// Called during session initialisation. The underlying storage uses INSERT
    /// OR IGNORE, so this is safe to call multiple times without duplicating
    /// records.
    pub async fn auto_activate_agent_skills(
        session_id: Uuid,
        available: &[SkillDefinition],
        storage: &dyn SkillActivationStore,
    ) -> Result<(), SkillError> {
        for skill in available
            .iter()
            .filter(|s| s.level == HierarchyLevel::Agent)
        {
            storage.activate_skill(session_id, &skill.name).await?;
        }
        Ok(())
    }

    /// Build the active skill set for constructing a `SkillsElement`.
    ///
    /// Retrieves activation records from storage (ordered by `activated_at`
    /// ascending), matches each against the available definitions, and returns
    /// [`ActiveSkillInfo`] values sorted with agent-level skills first, then by
    /// activation order within each tier.
    pub async fn build_active_skills(
        session_id: Uuid,
        available: &[SkillDefinition],
        storage: &dyn SkillActivationStore,
    ) -> Result<Vec<ActiveSkillInfo>, SkillError> {
        let records = storage.list_active_skills(session_id).await?;

        let mut infos: Vec<ActiveSkillInfo> = records
            .iter()
            .enumerate()
            .filter_map(|(order, record)| {
                available
                    .iter()
                    .find(|s| s.name == record.skill_name)
                    .map(|def| {
                        let is_agent = def.level == HierarchyLevel::Agent;
                        ActiveSkillInfo {
                            name: def.name.clone(),
                            instructions: def.instructions.clone(),
                            source_directory: def
                                .source_path
                                .parent()
                                .expect("skill source has a parent directory")
                                .to_path_buf(),
                            level: def.level.clone(),
                            is_agent_level: is_agent,
                            activation_order: order,
                        }
                    })
            })
            .collect();

        // Agent-level first (true > false), then by activation order ascending.
        infos.sort_by(|a, b| {
            b.is_agent_level
                .cmp(&a.is_agent_level)
                .then_with(|| a.activation_order.cmp(&b.activation_order))
        });

        Ok(infos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use swage_config::model::HierarchyLevel as CfgLevel;
    use swage_config::skill::SkillDefinition;
    use swage_context::tokeniser::TiktokenTokeniser;
    use swage_storage::{NewSession, SessionStore, SpaceStore, SqliteStorage};
    use tempfile::TempDir;

    // ── Test helpers ─────────────────────────────────────────────────────────

    /// Create an in-memory storage instance with a session, returning `(storage, session_id)`.
    async fn make_storage_with_session() -> (SqliteStorage, uuid::Uuid) {
        let storage = SqliteStorage::in_memory().expect("in-memory storage");
        let space = storage
            .get_or_create_default()
            .await
            .expect("default space");
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "test-agent".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("create session");
        (storage, session.id)
    }

    /// Build a minimal SkillDefinition for testing.
    fn make_skill(name: &str, level: CfgLevel) -> SkillDefinition {
        SkillDefinition {
            name: name.to_string(),
            display_name: Some(format!("{name}-display")),
            description: format!("{name} description"),
            instructions: format!("Instructions for {name}."),
            tools: vec![],
            level,
            source_path: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            shipped: false,
        }
    }

    /// Write a minimal SKILL.md inside `base/skills/<name>/`.
    fn write_skill_md(base: &std::path::Path, name: &str, description: &str) {
        let dir = base.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\ndescription: {description}\n---\n"),
        )
        .unwrap();
    }

    // ── SkillManager::activate tests ─────────────────────────────────────────

    /// Activating a new skill stores it and returns Activated with metadata.
    #[tokio::test(flavor = "multi_thread")]
    async fn activate_new_skill_returns_activated_with_token_count() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![make_skill("my-skill", CfgLevel::Space)];
        let tok = TiktokenTokeniser::new("gpt-4o");

        let result = SkillManager::activate(session_id, "my-skill", &available, &storage, &tok)
            .await
            .unwrap();

        match result {
            SkillActivationResult::Activated {
                name,
                display_name,
                description,
                token_count,
            } => {
                assert_eq!(name, "my-skill");
                assert_eq!(display_name, "my-skill-display");
                assert_eq!(description, "my-skill description");
                assert!(token_count > 0, "instructions should have non-zero tokens");
            }
            SkillActivationResult::AlreadyActive => panic!("expected Activated, got AlreadyActive"),
        }
    }

    /// Activating an already-active skill returns AlreadyActive without storing again.
    #[tokio::test(flavor = "multi_thread")]
    async fn activate_already_active_skill_returns_already_active() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![make_skill("my-skill", CfgLevel::Space)];
        let tok = TiktokenTokeniser::new("gpt-4o");

        // First activation
        SkillManager::activate(session_id, "my-skill", &available, &storage, &tok)
            .await
            .unwrap();

        // Second activation of the same skill
        let result = SkillManager::activate(session_id, "my-skill", &available, &storage, &tok)
            .await
            .unwrap();

        assert_eq!(result, SkillActivationResult::AlreadyActive);
    }

    /// Activating an unknown skill name returns NotFound.
    #[tokio::test(flavor = "multi_thread")]
    async fn activate_unknown_skill_returns_not_found() {
        let (storage, session_id) = make_storage_with_session().await;
        let available: Vec<SkillDefinition> = vec![];
        let tok = TiktokenTokeniser::new("gpt-4o");

        let result = SkillManager::activate(session_id, "ghost", &available, &storage, &tok).await;

        assert!(matches!(result, Err(SkillError::NotFound(name)) if name == "ghost"));
    }

    /// When a skill has no display_name, the canonical name is used as display_name.
    #[tokio::test(flavor = "multi_thread")]
    async fn activate_skill_without_display_name_uses_canonical_name() {
        let (storage, session_id) = make_storage_with_session().await;
        let mut skill = make_skill("bare", CfgLevel::Global);
        skill.display_name = None;
        let available = vec![skill];
        let tok = TiktokenTokeniser::new("gpt-4o");

        let result = SkillManager::activate(session_id, "bare", &available, &storage, &tok)
            .await
            .unwrap();

        if let SkillActivationResult::Activated { display_name, .. } = result {
            assert_eq!(
                display_name, "bare",
                "canonical name used when display_name is None"
            );
        } else {
            panic!("expected Activated");
        }
    }

    // ── SkillManager::deactivate tests ───────────────────────────────────────

    /// Deactivating an agent-level skill returns AgentLevelCannotDeactivate.
    #[tokio::test(flavor = "multi_thread")]
    async fn deactivate_agent_level_skill_returns_error() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![make_skill("agent-tool", CfgLevel::Agent)];

        let result = SkillManager::deactivate(session_id, "agent-tool", &available, &storage).await;

        assert!(
            matches!(result, Err(SkillError::AgentLevelCannotDeactivate(name)) if name == "agent-tool")
        );
    }

    /// Deactivating a user-activated skill removes it from storage.
    #[tokio::test(flavor = "multi_thread")]
    async fn deactivate_user_skill_removes_from_storage() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![make_skill("user-skill", CfgLevel::Space)];
        let tok = TiktokenTokeniser::new("gpt-4o");

        // Activate first
        SkillManager::activate(session_id, "user-skill", &available, &storage, &tok)
            .await
            .unwrap();

        // Deactivate
        SkillManager::deactivate(session_id, "user-skill", &available, &storage)
            .await
            .unwrap();

        let active = storage.list_active_skills(session_id).await.unwrap();
        assert!(active.is_empty(), "skill should have been removed");
    }

    // ── SkillManager::auto_activate_agent_skills tests ───────────────────────

    /// auto_activate_agent_skills stores all agent-level skills.
    #[tokio::test(flavor = "multi_thread")]
    async fn auto_activate_agent_skills_stores_all_agent_level() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![
            make_skill("agent-a", CfgLevel::Agent),
            make_skill("agent-b", CfgLevel::Agent),
            make_skill("user-only", CfgLevel::Space),
        ];

        SkillManager::auto_activate_agent_skills(session_id, &available, &storage)
            .await
            .unwrap();

        let active = storage.list_active_skills(session_id).await.unwrap();
        let active_names: Vec<&str> = active.iter().map(|r| r.skill_name.as_str()).collect();

        assert!(
            active_names.contains(&"agent-a"),
            "agent-a should be active"
        );
        assert!(
            active_names.contains(&"agent-b"),
            "agent-b should be active"
        );
        assert!(
            !active_names.contains(&"user-only"),
            "space skill should not be auto-activated"
        );
    }

    /// auto_activate_agent_skills is idempotent — calling twice doesn't error or duplicate.
    #[tokio::test(flavor = "multi_thread")]
    async fn auto_activate_agent_skills_is_idempotent() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![make_skill("agent-x", CfgLevel::Agent)];

        SkillManager::auto_activate_agent_skills(session_id, &available, &storage)
            .await
            .unwrap();
        SkillManager::auto_activate_agent_skills(session_id, &available, &storage)
            .await
            .unwrap();

        let active = storage.list_active_skills(session_id).await.unwrap();
        assert_eq!(active.len(), 1, "idempotent — no duplicates");
    }

    // ── SkillManager::build_active_skills tests ──────────────────────────────

    /// build_active_skills returns agent-level skills before user-activated ones.
    #[tokio::test(flavor = "multi_thread")]
    async fn build_active_skills_agent_level_sorts_first() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![
            make_skill("user-skill", CfgLevel::Space),
            make_skill("agent-skill", CfgLevel::Agent),
        ];
        let tok = TiktokenTokeniser::new("gpt-4o");

        // Activate user skill first, then auto-activate agent skills
        SkillManager::activate(session_id, "user-skill", &available, &storage, &tok)
            .await
            .unwrap();
        SkillManager::auto_activate_agent_skills(session_id, &available, &storage)
            .await
            .unwrap();

        let infos = SkillManager::build_active_skills(session_id, &available, &storage)
            .await
            .unwrap();

        assert_eq!(infos.len(), 2);
        assert!(infos[0].is_agent_level, "agent skill should sort first");
        assert_eq!(infos[0].name, "agent-skill");
        assert!(!infos[1].is_agent_level);
        assert_eq!(infos[1].name, "user-skill");
    }

    /// build_active_skills returns empty vec when no skills are active.
    #[tokio::test(flavor = "multi_thread")]
    async fn build_active_skills_empty_when_none_active() {
        let (storage, session_id) = make_storage_with_session().await;
        let available = vec![make_skill("dormant", CfgLevel::Space)];

        let infos = SkillManager::build_active_skills(session_id, &available, &storage)
            .await
            .unwrap();

        assert!(infos.is_empty());
    }

    // ── SkillManager::discover_available tests ───────────────────────────────

    /// discover_available finds skills across the hierarchy levels.
    #[test]
    fn discover_available_finds_skills_across_levels() {
        let root_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        write_skill_md(root_dir.path(), "global-skill", "Global.");
        write_skill_md(space_dir.path(), "space-skill", "Space.");
        write_skill_md(agent_dir.path(), "agent-skill", "Agent.");

        let (skills, warnings) = SkillManager::discover_available(
            root_dir.path(),
            space_dir.path(),
            Some(agent_dir.path()),
        );

        for name in ["global-skill", "space-skill", "agent-skill"] {
            assert!(skills.iter().any(|skill| skill.name == name));
        }
        assert_eq!(skills.iter().filter(|skill| skill.shipped).count(), 5);
        assert!(warnings.is_empty());
    }

    /// discover_available without agent_dir omits the agent level.
    #[test]
    fn discover_available_without_agent_dir_omits_agent_level() {
        let root_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();

        write_skill_md(root_dir.path(), "global-skill", "Global.");
        write_skill_md(space_dir.path(), "space-skill", "Space.");

        let (skills, _) = SkillManager::discover_available(root_dir.path(), space_dir.path(), None);

        assert_eq!(skills.len(), 7);
        assert!(skills.iter().all(|s| s.level != CfgLevel::Agent));
    }

    #[test]
    fn authored_skills_shadow_shipped_definitions_at_every_level() {
        for level in [CfgLevel::Global, CfgLevel::Space, CfgLevel::Agent] {
            let root = TempDir::new().unwrap();
            let space = TempDir::new().unwrap();
            let agent = TempDir::new().unwrap();
            let authored_base = match level {
                CfgLevel::Global => root.path(),
                CfgLevel::Space => space.path(),
                CfgLevel::Agent => agent.path(),
            };
            write_skill_md(authored_base, "commission", "Authored commission.");

            let (skills, warnings) =
                SkillManager::discover_available(root.path(), space.path(), Some(agent.path()));
            assert!(warnings.is_empty());
            let matching = skills
                .iter()
                .filter(|skill| skill.name == "commission")
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1);
            assert_eq!(matching[0].level, level);
            assert!(!matching[0].shipped);
            assert_eq!(matching[0].description, "Authored commission.");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projected_mutation_cannot_change_shipped_activation_body() {
        let root = TempDir::new().unwrap();
        let space = root.path().join("spaces/default");
        crate::shipped_skill_projection::regenerate(root.path())
            .await
            .expect("render projection");
        let projected =
            crate::shipped_skill_projection::skill_dir(root.path(), "commission").join("SKILL.md");
        std::fs::write(projected, "---\ndescription: tampered\n---\nTAMPERED BODY").unwrap();

        let (available, warnings) = SkillManager::discover_available(root.path(), &space, None);
        assert!(warnings.is_empty());
        let commission = available
            .iter()
            .find(|skill| skill.name == "commission")
            .expect("compiled commission skill");
        assert!(commission.shipped);
        assert_ne!(commission.description, "tampered");
        assert!(!commission.instructions.contains("TAMPERED BODY"));

        let (storage, session_id) = make_storage_with_session().await;
        let tokeniser = TiktokenTokeniser::new("gpt-4o");
        SkillManager::activate(session_id, "commission", &available, &storage, &tokeniser)
            .await
            .expect("activate shipped skill");
        let active = SkillManager::build_active_skills(session_id, &available, &storage)
            .await
            .expect("load active skill");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].instructions, commission.instructions);
        assert!(!active[0].instructions.contains("TAMPERED BODY"));
    }

    // -----------------------------------------------------------------------
    // parse_slash_command [796p]
    // -----------------------------------------------------------------------

    #[test]
    fn plain_skill_name_returns_activation_with_no_remaining() {
        let result = parse_slash_command("/code-review");
        assert!(
            matches!(result, SlashCommandResult::SkillActivation { skill_name, remaining_text: None } if skill_name == "code-review"),
            "expected SkillActivation for /code-review"
        );
    }

    #[test]
    fn skill_name_with_remaining_text() {
        let result = parse_slash_command("/code-review fix the bug");
        match result {
            SlashCommandResult::SkillActivation {
                skill_name,
                remaining_text,
            } => {
                assert_eq!(skill_name, "code-review");
                assert_eq!(remaining_text, Some("fix the bug".to_string()));
            }
            SlashCommandResult::NotACommand => panic!("expected SkillActivation, got NotACommand"),
        }
    }

    #[test]
    fn path_like_input_returns_not_a_command() {
        // Contains `/` after the initial slash — looks like a file path.
        let result = parse_slash_command("/path/to/file");
        assert!(matches!(result, SlashCommandResult::NotACommand));
    }

    #[test]
    fn input_without_leading_slash_returns_not_a_command() {
        let result = parse_slash_command("not a command");
        assert!(matches!(result, SlashCommandResult::NotACommand));
    }

    #[test]
    fn uppercase_skill_name_returns_not_a_command() {
        let result = parse_slash_command("/Code-Review");
        assert!(matches!(result, SlashCommandResult::NotACommand));
    }

    #[test]
    fn slash_with_space_first_returns_not_a_command() {
        // Empty candidate name (space immediately after slash).
        let result = parse_slash_command("/ spaces");
        assert!(matches!(result, SlashCommandResult::NotACommand));
    }

    #[test]
    fn digits_only_skill_name_is_valid() {
        let result = parse_slash_command("/123");
        assert!(
            matches!(result, SlashCommandResult::SkillActivation { skill_name, remaining_text: None } if skill_name == "123"),
            "expected SkillActivation for /123"
        );
    }

    #[test]
    fn empty_string_returns_not_a_command() {
        let result = parse_slash_command("");
        assert!(matches!(result, SlashCommandResult::NotACommand));
    }

    #[test]
    fn bare_slash_returns_not_a_command() {
        // Just "/" with no name after it.
        let result = parse_slash_command("/");
        assert!(matches!(result, SlashCommandResult::NotACommand));
    }
}

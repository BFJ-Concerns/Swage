use crate::RESERVED_WORKFLOW_AGENT_ID;
use crate::discovery;
use crate::error::{ConfigError, ConfigProblem, ConfigProblemKind};
use crate::instructions;
use crate::merge::{self, PartialConfig};
use crate::model::{HierarchyLevel, ResolvedConfig};
use crate::validation;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use swage_core::ScopeLocation;

/// Construct the active configuration hierarchy for a scope and an optional
/// agent: Global → Topic → Agent for a topic, Global → Agent for the general
/// scope, which has no scope-level layer (its agent overlays still live
/// beneath its own directory).
///
/// The reserved workflow identity is dispatch-only, so it intentionally never
/// addresses an authored agent directory.
pub fn configuration_hierarchy(
    root: &Path,
    scope: &ScopeLocation,
    agent: Option<&str>,
) -> Vec<(HierarchyLevel, PathBuf)> {
    let mut hierarchy = vec![(HierarchyLevel::Global, root.to_path_buf())];
    if !scope.is_general() {
        hierarchy.push((HierarchyLevel::Topic, scope.data_dir(root)));
    }
    if let Some(agent) = agent.filter(|agent| *agent != RESERVED_WORKFLOW_AGENT_ID) {
        hierarchy.push((HierarchyLevel::Agent, scope.agent_dir(root, agent)));
    }
    hierarchy
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Try to read and parse a config file. Returns `Some(partial)` on success,
/// `None` if the file doesn't exist (silently skipped), and appends any
/// validation problems to `problems`.
fn try_load_file(path: &Path, problems: &mut Vec<ConfigProblem>) -> Option<PartialConfig> {
    if !path.exists() {
        return None;
    }

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            problems.push(ConfigProblem {
                file_path: path.to_path_buf(),
                field_path: None,
                kind: ConfigProblemKind::DiscoveryError {
                    message: format!("failed to read config file: {e}"),
                },
            });
            return None;
        }
    };

    match validation::parse_and_validate(&content, path) {
        Ok(partial) => Some(partial),
        Err(errs) => {
            problems.extend(errs);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fully resolve configuration for a given scope and agent.
///
/// Resolves the Swage root via `$SWAGE_HOME` / `~/.swage/`, then delegates
/// to [`load_resolved_config_from`].
pub fn load_resolved_config(
    scope: &ScopeLocation,
    agent: &str,
) -> Result<ResolvedConfig, ConfigError> {
    let root = discovery::resolve_root().map_err(|p| ConfigError::new(vec![p]))?;
    load_resolved_config_from(&root, scope, agent)
}

/// Fully resolve configuration given an explicit root directory.
pub fn load_resolved_config_from(
    root: &Path,
    scope: &ScopeLocation,
    agent: &str,
) -> Result<ResolvedConfig, ConfigError> {
    // Build paths for each active hierarchy level.
    let hierarchy = configuration_hierarchy(root, scope, Some(agent));

    let mut all_problems: Vec<ConfigProblem> = Vec::new();

    // Active merge levels: Global → Topic → Agent.
    let mut config_chain: Vec<PartialConfig> = Vec::new();
    for (_, directory) in &hierarchy {
        let path = directory.join("config.toml");
        if let Some(partial) = try_load_file(&path, &mut all_problems) {
            config_chain.push(partial);
        }
    }

    // Fail fast if any file had errors — report everything collected.
    if !all_problems.is_empty() {
        return Err(ConfigError::new(all_problems));
    }

    // Merge and resolve to typed config.
    let merged = merge::merge_chain(config_chain);
    let mut resolved = merge::resolve(merged);
    let reference_problems = validation::validate_resolved_catalog_references(&resolved, root);
    if !reference_problems.is_empty() {
        return Err(ConfigError::new(reference_problems));
    }

    // Assemble instructions from the same active levels.
    let levels = hierarchy
        .iter()
        .map(|(level, directory)| (level.clone(), directory.join("instructions")))
        .collect::<Vec<_>>();
    let level_refs = levels
        .iter()
        .map(|(level, path)| (level.clone(), path.as_path()))
        .collect::<Vec<_>>();

    match instructions::assemble_instructions(&level_refs) {
        Ok(text) => resolved.instructions = text,
        Err(problem) => return Err(ConfigError::new(vec![problem])),
    }

    Ok(resolved)
}

/// Apply a programmatic overlay (e.g. a task definition) to a resolved config.
///
/// Used by task execution to overlay task-specific settings (instructions,
/// output schema, model, tool permissions) onto an agent's resolved config.
/// Overlay fields that are `None` leave the base unchanged.
pub fn apply_overlay(base: ResolvedConfig, overlay: PartialConfig) -> ResolvedConfig {
    // Convert the resolved base to a partial, merge with the overlay,
    // then resolve again. This reuses the merge semantics (replacement
    // for permission lists, map-merge for pricing, etc.).
    let base_partial = PartialConfig::from(base.clone());
    let merged = merge::merge_partial(base_partial, overlay);
    let mut resolved = merge::resolve(merged);

    // Preserve instructions from the base — task overlays don't carry
    // file-based instructions. If the overlay needs to prepend/append
    // instructions, the caller should set resolved.instructions directly.
    if resolved.instructions.is_empty() {
        resolved.instructions = base.instructions;
    }

    resolved
}

/// Validate one or more config files without loading a full resolved config.
///
/// Reads each file and runs schema validation, aggregating all problems.
/// Useful for pre-flight checks and tooling.
pub fn validate_files(paths: &[&Path]) -> Vec<ConfigProblem> {
    let mut all_problems = Vec::new();
    for &path in paths {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                all_problems.push(ConfigProblem {
                    file_path: path.to_path_buf(),
                    field_path: None,
                    kind: ConfigProblemKind::DiscoveryError {
                        message: format!("failed to read file: {e}"),
                    },
                });
                continue;
            }
        };
        all_problems.extend(validation::validate_toml_string(&content, path));
    }
    all_problems
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        name: &'static str,
        value: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(name);
            // Safety: tests that mutate process environment hold ENV_LOCK.
            unsafe { std::env::set_var(name, value) };
            Self {
                name,
                value: previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // Safety: tests that mutate process environment hold ENV_LOCK.
            unsafe {
                if let Some(value) = &self.value {
                    std::env::set_var(self.name, value);
                } else {
                    std::env::remove_var(self.name);
                }
            }
        }
    }

    fn build_fixture(
        root: &Path,
        global_toml: Option<&str>,
        topic: &str,
        topic_toml: Option<&str>,
        agent: &str,
        agent_toml: Option<&str>,
    ) {
        if let Some(content) = global_toml {
            fs::write(root.join("config.toml"), content).unwrap();
        }
        let topic_dir = root.join("topics").join(topic);
        let agent_dir = topic_dir.join("agents").join(agent);
        fs::create_dir_all(&agent_dir).unwrap();
        if let Some(content) = topic_toml {
            fs::write(topic_dir.join("config.toml"), content).unwrap();
        }
        if let Some(content) = agent_toml {
            fs::write(agent_dir.join("config.toml"), content).unwrap();
        }
    }

    #[test]
    fn the_general_scope_has_no_scope_layer_but_keeps_its_agent_overlay() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("config.toml"),
            "[server]\nhost = \"10.0.0.1\"\nport = 3000\n",
        )
        .unwrap();
        let agent_dir = tmp.path().join("general").join("agents").join("bot");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(agent_dir.join("config.toml"), "[server]\nport = 4000\n").unwrap();

        let hierarchy = configuration_hierarchy(tmp.path(), &ScopeLocation::General, Some("bot"));
        assert_eq!(
            hierarchy
                .iter()
                .map(|(level, _)| level.clone())
                .collect::<Vec<_>>(),
            vec![HierarchyLevel::Global, HierarchyLevel::Agent]
        );
        assert_eq!(hierarchy[1].1, agent_dir);

        let cfg = load_resolved_config_from(tmp.path(), &ScopeLocation::General, "bot")
            .expect("general scope resolves");
        assert_eq!(cfg.server.host, "10.0.0.1");
        assert_eq!(
            cfg.server.port, 4000,
            "the general scope's agent overlay applies"
        );
    }

    #[test]
    fn three_level_merge_applies_overrides() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nhost = \"10.0.0.1\"\nport = 3000\n"),
            "proj",
            Some("[models]\ndefault_model_id = \"claude-opus\"\n"),
            "bot",
            Some("[server]\nport = 4000\n"),
        );

        let cfg = load_resolved_config_from(tmp.path(), &ScopeLocation::topic("proj"), "bot")
            .expect("should succeed");
        assert_eq!(cfg.server.host, "10.0.0.1", "global host should survive");
        assert_eq!(cfg.server.port, 4000, "agent overrides global port");
        assert_eq!(
            cfg.default_model_id(),
            "claude-opus",
            "topic model should apply"
        );
    }

    #[test]
    fn missing_config_files_silently_skipped() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nport = 5000\n"),
            "bare-topic",
            None,
            "default",
            None,
        );

        let cfg =
            load_resolved_config_from(tmp.path(), &ScopeLocation::topic("bare-topic"), "default")
                .expect("should succeed even with missing intermediate configs");
        assert_eq!(cfg.server.port, 5000);
    }

    #[test]
    fn load_resolved_config_reads_root_from_environment() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nport = 5042\n"),
            "env-topic",
            Some("[models]\ndefault_model_id = \"env-model\"\n"),
            "env-agent",
            Some("[server]\nhost = \"127.0.0.42\"\n"),
        );
        let _env_guard = EnvVarGuard::set("SWAGE_HOME", tmp.path());

        let cfg = load_resolved_config(&ScopeLocation::topic("env-topic"), "env-agent")
            .expect("env-derived root should resolve config");

        assert_eq!(cfg.server.host, "127.0.0.42");
        assert_eq!(cfg.server.port, 5042);
        assert_eq!(cfg.default_model_id(), "env-model");
    }

    #[test]
    fn search_config_rejects_unused_fields_at_load() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            r#"[tools.search]
provider_type = "searxng"
base_url = "http://localhost:8080"
api_key = "x"
"#,
        )
        .unwrap();
        let error = load_resolved_config_from(tmp.path(), &ScopeLocation::General, "default")
            .expect_err("unused search fields must be refused at load");
        assert!(
            error.to_string().contains("api_key"),
            "field must be named: {error}"
        );
    }

    #[test]
    fn validation_errors_returned_as_config_error() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("this is not toml ==="),
            "proj",
            None,
            "bot",
            None,
        );

        let result = load_resolved_config_from(tmp.path(), &ScopeLocation::topic("proj"), "bot");
        assert!(
            result.is_err(),
            "malformed global config should produce Err"
        );
    }

    #[test]
    fn resolved_catalog_reference_errors_returned_as_config_error() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[models]\ndefault = \"missing\"\n"),
            "proj",
            None,
            "bot",
            None,
        );

        let error = load_resolved_config_from(tmp.path(), &ScopeLocation::topic("proj"), "bot")
            .expect_err("missing default model entry should produce Err");

        assert!(
            error
                .problems
                .iter()
                .any(
                    |problem| problem.field_path.as_deref() == Some("models.default")
                        && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
                ),
            "missing resolved default model should be reported, got: {error:?}"
        );
    }

    #[test]
    fn validate_files_reports_errors() {
        let tmp = TempDir::new().unwrap();
        let bad_file = tmp.path().join("config.toml");
        fs::write(&bad_file, "[server]\nport = \"not a number\"\n").unwrap();

        let problems = validate_files(&[bad_file.as_path()]);
        assert!(!problems.is_empty(), "should report validation problems");
    }

    // --- Task overlay ---

    #[test]
    fn apply_overlay_overrides_provider_model() {
        let base = ResolvedConfig::default();
        let overlay = PartialConfig {
            models: Some(merge::PartialModelCatalogConfig {
                default_model_id: Some("task-model".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let result = apply_overlay(base, overlay);
        assert_eq!(result.default_model_id(), "task-model");
        // Other fields should keep defaults
        assert_eq!(result.server.port, 3000);
    }

    #[test]
    fn apply_overlay_preserves_instructions() {
        let base = ResolvedConfig {
            instructions: "You are a helpful assistant.".to_string(),
            ..Default::default()
        };

        let overlay = PartialConfig {
            models: Some(merge::PartialModelCatalogConfig {
                default_model_id: Some("fast-model".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let result = apply_overlay(base, overlay);
        assert_eq!(result.instructions, "You are a helpful assistant.");
        assert_eq!(result.default_model_id(), "fast-model");
    }

    #[test]
    fn apply_overlay_empty_overlay_preserves_base() {
        let mut base = ResolvedConfig::default();
        base.server.port = 9999;
        base.set_default_model_id("my-model".to_string());

        let result = apply_overlay(base.clone(), PartialConfig::default());
        assert_eq!(result.server.port, 9999);
        assert_eq!(result.default_model_id(), "my-model");
    }
}

use crate::discovery;
use crate::error::{ConfigError, ConfigProblem, ConfigProblemKind};
use crate::instructions;
use crate::merge::{self, PartialConfig};
use crate::model::{HierarchyLevel, ResolvedConfig};
use crate::validation;
use std::fs;
use std::path::Path;

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

/// Fully resolve configuration for a given bench and agent.
///
/// Resolves the Swage root via `$SWAGE_HOME` / `~/.swage/`, then delegates
/// to [`load_resolved_config_from`].
pub fn load_resolved_config(bench: &str, agent: &str) -> Result<ResolvedConfig, ConfigError> {
    let root = discovery::resolve_root().map_err(|p| ConfigError::new(vec![p]))?;
    load_resolved_config_from(&root, bench, agent)
}

/// Fully resolve configuration for a given bench, team, and agent.
///
/// Same as [`load_resolved_config`] but inserts the team level between
/// bench and agent in the merge chain: Global → Bench → Team → Agent.
pub fn load_resolved_config_for_team(
    bench: &str,
    team: &str,
    agent: &str,
) -> Result<ResolvedConfig, ConfigError> {
    let root = discovery::resolve_root().map_err(|p| ConfigError::new(vec![p]))?;
    load_resolved_config_from_with_team(&root, bench, Some(team), agent)
}

/// Fully resolve configuration given an explicit root directory.
///
/// Shorthand for [`load_resolved_config_from_with_team`] with no team level.
pub fn load_resolved_config_from(
    root: &Path,
    bench: &str,
    agent: &str,
) -> Result<ResolvedConfig, ConfigError> {
    load_resolved_config_from_with_team(root, bench, None, agent)
}

/// Fully resolve configuration given an explicit root directory, with an
/// optional team level in the cascade.
///
/// Pipeline:
/// 1. Build hierarchy paths (Global → Bench → [Team] → Agent).
/// 2. Load and validate each config.toml that exists.
/// 3. If any validation errors were found, return them all as `ConfigError`.
/// 4. Merge the active levels via `merge_chain` + `resolve` to produce the
///    typed `ResolvedConfig`.
/// 5. Assemble instructions from the same active levels.
/// 6. Return the complete `ResolvedConfig`.
///
/// Missing config files at any level are silently skipped — that level
/// simply inherits values from above.
pub fn load_resolved_config_from_with_team(
    root: &Path,
    bench: &str,
    team: Option<&str>,
    agent: &str,
) -> Result<ResolvedConfig, ConfigError> {
    // Build paths for each hierarchy level.
    let global_config = root.join("config.toml");
    let bench_dir = root.join("benches").join(bench);
    let bench_config = bench_dir.join("config.toml");
    let agent_dir = bench_dir.join("agents").join(agent);
    let agent_config = agent_dir.join("config.toml");

    let global_instrs = root.join("instructions");
    let bench_instrs = bench_dir.join("instructions");
    let agent_instrs = agent_dir.join("instructions");

    let mut all_problems: Vec<ConfigProblem> = Vec::new();

    // Active merge levels: Global → Bench → [Team] → Agent.
    let mut config_chain: Vec<PartialConfig> = Vec::new();
    for path in &[&global_config, &bench_config] {
        if let Some(partial) = try_load_file(path, &mut all_problems) {
            config_chain.push(partial);
        }
    }

    // Team level sits between bench and agent — only present when specified.
    let team_dir = team.map(|t| bench_dir.join("teams").join(t));
    if let Some(ref td) = team_dir {
        let team_config = td.join("config.toml");
        if let Some(partial) = try_load_file(&team_config, &mut all_problems) {
            config_chain.push(partial);
        }
    }

    if let Some(partial) = try_load_file(&agent_config, &mut all_problems) {
        config_chain.push(partial);
    }

    // Fail fast if any file had errors — report everything collected.
    if !all_problems.is_empty() {
        return Err(ConfigError::new(all_problems));
    }

    // Merge and resolve to typed config.
    let merged = merge::merge_chain(config_chain);
    let mut resolved = merge::resolve(merged);

    // Assemble instructions from the same active levels.
    // Build the team instructions path up front so it outlives the levels vec.
    let team_instrs = team_dir.as_ref().map(|td| td.join("instructions"));
    let mut levels: Vec<(HierarchyLevel, &Path)> = vec![
        (HierarchyLevel::Global, global_instrs.as_path()),
        (HierarchyLevel::Bench, bench_instrs.as_path()),
    ];
    if let Some(ref ti) = team_instrs {
        levels.push((HierarchyLevel::Team, ti.as_path()));
    }
    levels.push((HierarchyLevel::Agent, agent_instrs.as_path()));

    match instructions::assemble_instructions(&levels) {
        Ok(text) => resolved.instructions = text,
        Err(problem) => return Err(ConfigError::new(vec![problem])),
    }

    Ok(resolved)
}

/// Apply a programmatic overlay (e.g. a task definition) to a resolved config.
///
/// Used by the topology engine to overlay task-specific settings (instructions,
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
    use tempfile::TempDir;

    fn build_fixture(
        root: &Path,
        global_toml: Option<&str>,
        bench: &str,
        bench_toml: Option<&str>,
        agent: &str,
        agent_toml: Option<&str>,
    ) {
        if let Some(content) = global_toml {
            fs::write(root.join("config.toml"), content).unwrap();
        }
        fs::create_dir_all(root.join("lenses/default")).unwrap();
        let bench_dir = root.join("benches").join(bench);
        let agent_dir = bench_dir.join("agents").join(agent);
        fs::create_dir_all(&agent_dir).unwrap();
        fs::create_dir_all(bench_dir.join("teams")).unwrap();
        if let Some(content) = bench_toml {
            fs::write(bench_dir.join("config.toml"), content).unwrap();
        }
        if let Some(content) = agent_toml {
            fs::write(agent_dir.join("config.toml"), content).unwrap();
        }
    }

    #[test]
    fn three_level_merge_applies_overrides() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nhost = \"10.0.0.1\"\nport = 3000\n"),
            "proj",
            Some("[provider]\nmodel = \"claude-opus\"\n"),
            "bot",
            Some("[server]\nport = 4000\n"),
        );

        let cfg = load_resolved_config_from(tmp.path(), "proj", "bot").expect("should succeed");
        assert_eq!(cfg.server.host, "10.0.0.1", "global host should survive");
        assert_eq!(cfg.server.port, 4000, "agent overrides global port");
        assert_eq!(
            cfg.provider.model, "claude-opus",
            "bench model should apply"
        );
    }

    #[test]
    fn missing_config_files_silently_skipped() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nport = 5000\n"),
            "bare-bench",
            None,
            "default",
            None,
        );

        let cfg = load_resolved_config_from(tmp.path(), "bare-bench", "default")
            .expect("should succeed even with missing intermediate configs");
        assert_eq!(cfg.server.port, 5000);
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

        let result = load_resolved_config_from(tmp.path(), "proj", "bot");
        assert!(
            result.is_err(),
            "malformed global config should produce Err"
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

    // --- Team config cascade ---

    #[test]
    fn team_config_merges_between_bench_and_agent() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nhost = \"10.0.0.1\"\nport = 3000\n"),
            "proj",
            Some("[provider]\nmodel = \"claude-opus\"\n"),
            "bot",
            Some("[server]\nport = 4000\n"),
        );
        // Add team config that overrides provider model.
        let team_dir = tmp.path().join("benches/proj/teams/review-team");
        fs::create_dir_all(&team_dir).unwrap();
        fs::write(
            team_dir.join("config.toml"),
            "[provider]\nmodel = \"claude-haiku\"\n",
        )
        .unwrap();

        let cfg =
            load_resolved_config_from_with_team(tmp.path(), "proj", Some("review-team"), "bot")
                .expect("should succeed");

        // Agent overrides port (agent > team > bench > global)
        assert_eq!(cfg.server.port, 4000, "agent should win on port");
        // Team overrides bench model, agent doesn't set model so team wins
        assert_eq!(
            cfg.provider.model, "claude-haiku",
            "team should override bench model"
        );
        // Global host survives (no override)
        assert_eq!(cfg.server.host, "10.0.0.1", "global host should survive");
    }

    #[test]
    fn agent_overrides_team_config() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            None,
            "proj",
            None,
            "bot",
            Some("[provider]\nmodel = \"agent-model\"\n"),
        );
        let team_dir = tmp.path().join("benches/proj/teams/myteam");
        fs::create_dir_all(&team_dir).unwrap();
        fs::write(
            team_dir.join("config.toml"),
            "[provider]\nmodel = \"team-model\"\n",
        )
        .unwrap();

        let cfg = load_resolved_config_from_with_team(tmp.path(), "proj", Some("myteam"), "bot")
            .expect("should succeed");

        assert_eq!(
            cfg.provider.model, "agent-model",
            "agent should override team"
        );
    }

    #[test]
    fn missing_team_config_silently_skipped() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nport = 5000\n"),
            "proj",
            None,
            "default",
            None,
        );

        // Team directory exists but has no config.toml.
        let team_dir = tmp.path().join("benches/proj/teams/empty-team");
        fs::create_dir_all(&team_dir).unwrap();

        let cfg =
            load_resolved_config_from_with_team(tmp.path(), "proj", Some("empty-team"), "default")
                .expect("should succeed");
        assert_eq!(cfg.server.port, 5000, "global port should survive");
    }

    #[test]
    fn no_team_behaves_like_original() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nhost = \"1.2.3.4\"\nport = 9000\n"),
            "proj",
            None,
            "bot",
            None,
        );

        let without_team =
            load_resolved_config_from(tmp.path(), "proj", "bot").expect("should succeed");
        let with_none_team = load_resolved_config_from_with_team(tmp.path(), "proj", None, "bot")
            .expect("should succeed");

        assert_eq!(
            without_team, with_none_team,
            "None team should produce same result as no-team function"
        );
    }

    // --- Task overlay ---

    #[test]
    fn apply_overlay_overrides_provider_model() {
        let base = ResolvedConfig::default();
        let overlay = PartialConfig {
            provider: Some(merge::PartialProviderConfig {
                model: Some("task-model".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let result = apply_overlay(base, overlay);
        assert_eq!(result.provider.model, "task-model");
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
            provider: Some(merge::PartialProviderConfig {
                model: Some("fast-model".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let result = apply_overlay(base, overlay);
        assert_eq!(result.instructions, "You are a helpful assistant.");
        assert_eq!(result.provider.model, "fast-model");
    }

    #[test]
    fn apply_overlay_empty_overlay_preserves_base() {
        let mut base = ResolvedConfig::default();
        base.server.port = 9999;
        base.provider.model = "my-model".to_string();

        let result = apply_overlay(base.clone(), PartialConfig::default());
        assert_eq!(result.server.port, 9999);
        assert_eq!(result.provider.model, "my-model");
    }
}

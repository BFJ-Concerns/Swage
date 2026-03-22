use std::fs;
use std::path::Path;
use crate::error::{ConfigError, ConfigProblem, ConfigProblemKind};
use crate::instructions;
use crate::merge::{self, PartialConfig};
use crate::model::{HierarchyLevel, ResolvedConfig};
use crate::validation;
use crate::discovery;

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
        Err(errs)   => { problems.extend(errs); None }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fully resolve configuration for a given project and agent.
///
/// Resolves the Swage root via `$SWAGE_HOME` / `~/.swage/`, then delegates
/// to [`load_resolved_config_from`].
pub fn load_resolved_config(project: &str, agent: &str) -> Result<ResolvedConfig, ConfigError> {
    let root = discovery::resolve_root().map_err(|p| ConfigError::new(vec![p]))?;
    load_resolved_config_from(&root, project, agent)
}

/// Fully resolve configuration given an explicit root directory.
///
/// Pipeline:
/// 1. Build hierarchy paths (Global → Lens → Project → Agent).
/// 2. Load and validate each config.toml that exists. Lens config is validated
///    but its values are **not** merged (0.1 constraint).
/// 3. If any validation errors were found, return them all as `ConfigError`.
/// 4. Merge the active levels (Global, Project, Agent) via `merge_chain` +
///    `resolve` to produce the typed `ResolvedConfig`.
/// 5. Assemble instructions from the same active levels.
/// 6. Return the complete `ResolvedConfig`.
///
/// Missing config files at any level are silently skipped — that level
/// simply inherits values from above.
pub fn load_resolved_config_from(
    root: &Path,
    project: &str,
    agent: &str,
) -> Result<ResolvedConfig, ConfigError> {
    // Build paths for each hierarchy level.
    let global_config  = root.join("config.toml");
    let lens_config    = root.join("lenses").join("default").join("config.toml");
    let project_dir    = root.join("projects").join(project);
    let project_config = project_dir.join("config.toml");
    let agent_dir      = project_dir.join("agents").join(agent);
    let agent_config   = agent_dir.join("config.toml");

    let global_instrs  = root.join("instructions");
    let project_instrs = project_dir.join("instructions");
    let agent_instrs   = agent_dir.join("instructions");

    let mut all_problems: Vec<ConfigProblem> = Vec::new();

    // Lens: validate but discard — its values are not part of the merge chain.
    let _ = try_load_file(&lens_config, &mut all_problems);

    // Active merge levels: Global → Project → Agent.
    let mut merge_chain: Vec<PartialConfig> = Vec::new();
    for path in &[&global_config, &project_config, &agent_config] {
        if let Some(partial) = try_load_file(path, &mut all_problems) {
            merge_chain.push(partial);
        }
    }

    // Fail fast if any file had errors — report everything collected.
    if !all_problems.is_empty() {
        return Err(ConfigError::new(all_problems));
    }

    // Merge and resolve to typed config.
    let merged = merge::merge_chain(merge_chain);
    let mut resolved = merge::resolve(merged);

    // Assemble instructions from the same active levels.
    let levels: Vec<(HierarchyLevel, &Path)> = vec![
        (HierarchyLevel::Global,  global_instrs.as_path()),
        (HierarchyLevel::Project, project_instrs.as_path()),
        (HierarchyLevel::Agent,   agent_instrs.as_path()),
    ];
    match instructions::assemble_instructions(&levels) {
        Ok(text)    => resolved.instructions = text,
        Err(problem) => return Err(ConfigError::new(vec![problem])),
    }

    Ok(resolved)
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
        project: &str,
        project_toml: Option<&str>,
        agent: &str,
        agent_toml: Option<&str>,
    ) {
        if let Some(content) = global_toml {
            fs::write(root.join("config.toml"), content).unwrap();
        }
        fs::create_dir_all(root.join("lenses/default")).unwrap();
        let project_dir = root.join("projects").join(project);
        let agent_dir   = project_dir.join("agents").join(agent);
        fs::create_dir_all(&agent_dir).unwrap();
        fs::create_dir_all(project_dir.join("teams")).unwrap();
        if let Some(content) = project_toml {
            fs::write(project_dir.join("config.toml"), content).unwrap();
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

        let cfg = load_resolved_config_from(tmp.path(), "proj", "bot")
            .expect("should succeed");
        assert_eq!(cfg.server.host, "10.0.0.1", "global host should survive");
        assert_eq!(cfg.server.port, 4000,        "agent overrides global port");
        assert_eq!(cfg.provider.model, "claude-opus", "project model should apply");
    }

    #[test]
    fn missing_config_files_silently_skipped() {
        let tmp = TempDir::new().unwrap();
        build_fixture(
            tmp.path(),
            Some("[server]\nport = 5000\n"),
            "bare-project",
            None,
            "default",
            None,
        );

        let cfg = load_resolved_config_from(tmp.path(), "bare-project", "default")
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
        assert!(result.is_err(), "malformed global config should produce Err");
    }

    #[test]
    fn validate_files_reports_errors() {
        let tmp = TempDir::new().unwrap();
        let bad_file = tmp.path().join("config.toml");
        fs::write(&bad_file, "[server]\nport = \"not a number\"\n").unwrap();

        let problems = validate_files(&[bad_file.as_path()]);
        assert!(!problems.is_empty(), "should report validation problems");
    }
}

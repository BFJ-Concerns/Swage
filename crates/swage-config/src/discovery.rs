use std::fs;
use std::path::{Path, PathBuf};
use crate::error::{ConfigProblem, ConfigProblemKind};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A project discovered by scanning the Swage directory skeleton.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredProject {
    /// Directory name used as the project identifier.
    pub name: String,
    /// Absolute path to the project directory.
    pub path: PathBuf,
    /// Names of agents discovered under the project's `agents/` sub-directory.
    pub agents: Vec<String>,
}

// ---------------------------------------------------------------------------
// Root resolution
// ---------------------------------------------------------------------------

/// Returns the effective Swage root directory.
///
/// Resolution order:
/// 1. `$SWAGE_HOME` environment variable, if set.
/// 2. `~/.swage/` (via `$HOME`).
///
/// Returns `DiscoveryError` if the resolved path does not exist on disk,
/// with a guidance message telling the user how to create it.
pub fn resolve_root() -> Result<PathBuf, ConfigProblem> {
    let root = if let Ok(home_override) = std::env::var("SWAGE_HOME") {
        PathBuf::from(home_override)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        PathBuf::from(home).join(".swage")
    };

    if !root.exists() {
        return Err(ConfigProblem {
            file_path: root.clone(),
            field_path: None,
            kind: ConfigProblemKind::DiscoveryError {
                message: format!(
                    "Swage root directory '{}' does not exist — \
                     create it to get started (e.g. mkdir -p '{}')",
                    root.display(),
                    root.display(),
                ),
            },
        });
    }

    Ok(root)
}

// ---------------------------------------------------------------------------
// Directory scanning helpers
// ---------------------------------------------------------------------------

/// Collect immediate subdirectory names from `dir`, returning them sorted.
/// Returns an empty list if `dir` doesn't exist.
fn subdirectory_names(dir: &Path) -> Vec<String> {
    if !dir.exists() {
        return vec![];
    }

    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();

    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Scan `{project_dir}/agents/` for subdirectories.
/// Returns agent names (directory names), sorted alphabetically.
/// Returns an empty list if the `agents/` directory does not exist.
pub fn discover_agents(project_dir: &Path) -> Vec<String> {
    subdirectory_names(&project_dir.join("agents"))
}

/// Scan `{root}/projects/` for subdirectories.
/// Each subdirectory is treated as a project regardless of whether it has
/// a `config.toml` — a project without one simply inherits global config.
/// Returns an empty list if the `projects/` directory does not exist.
pub fn discover_projects(root: &Path) -> Vec<DiscoveredProject> {
    let projects_dir = root.join("projects");
    let names = subdirectory_names(&projects_dir);

    names
        .into_iter()
        .map(|name| {
            let path = projects_dir.join(&name);
            let agents = discover_agents(&path);
            DiscoveredProject { name, path, agents }
        })
        .collect()
}

/// Full discovery: returns all projects and their agents under `root`.
///
/// Convenience wrapper around `discover_projects` that also populates each
/// project's agent list.
pub fn discover(root: &Path) -> Vec<DiscoveredProject> {
    discover_projects(root)
}

/// Returns `true` if `{root}/lenses/default/` exists on disk.
pub fn has_default_lens(root: &Path) -> bool {
    root.join("lenses").join("default").exists()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // --- resolve_root ---

    #[test]
    fn resolve_root_uses_swage_home_env_var() {
        let tmp = TempDir::new().unwrap();
        // Safety: test-only env mutation; set_var is unsafe in edition 2024
        unsafe { std::env::set_var("SWAGE_HOME", tmp.path()) };
        let result = resolve_root();
        unsafe { std::env::remove_var("SWAGE_HOME") };
        assert_eq!(result.unwrap(), tmp.path());
    }

    #[test]
    fn resolve_root_errors_when_root_missing() {
        unsafe { std::env::set_var("SWAGE_HOME", "/no/such/swage/root") };
        let result = resolve_root();
        unsafe { std::env::remove_var("SWAGE_HOME") };
        assert!(result.is_err(), "missing root should produce an error");
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ConfigProblemKind::DiscoveryError { .. }));
    }

    // --- discover_projects ---

    #[test]
    fn discover_projects_finds_subdirectories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("projects/alpha")).unwrap();
        fs::create_dir_all(tmp.path().join("projects/beta")).unwrap();

        let projects = discover_projects(tmp.path());
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "should discover alpha");
        assert!(names.contains(&"beta"),  "should discover beta");
    }

    #[test]
    fn discover_projects_returns_empty_when_projects_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let projects = discover_projects(tmp.path());
        assert!(projects.is_empty());
    }

    #[test]
    fn discover_projects_includes_path() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("projects/myproject")).unwrap();

        let projects = discover_projects(tmp.path());
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, tmp.path().join("projects/myproject"));
    }

    // --- discover_agents ---

    #[test]
    fn discover_agents_finds_agent_directories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("agents/coder")).unwrap();
        fs::create_dir_all(tmp.path().join("agents/reviewer")).unwrap();

        let agents = discover_agents(tmp.path());
        assert!(agents.contains(&"coder".to_string()),    "should find coder");
        assert!(agents.contains(&"reviewer".to_string()), "should find reviewer");
    }

    #[test]
    fn discover_agents_returns_empty_when_agents_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let agents = discover_agents(tmp.path());
        assert!(agents.is_empty());
    }

    // --- discover ---

    #[test]
    fn discover_combines_projects_and_agents() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("projects/alpha/agents/assistant")).unwrap();
        fs::create_dir_all(tmp.path().join("projects/alpha/agents/reviewer")).unwrap();
        fs::create_dir_all(tmp.path().join("projects/beta")).unwrap();

        let projects = discover(tmp.path());
        let alpha = projects.iter().find(|p| p.name == "alpha")
            .expect("alpha should be discovered");
        assert!(alpha.agents.contains(&"assistant".to_string()));
        assert!(alpha.agents.contains(&"reviewer".to_string()));

        let beta = projects.iter().find(|p| p.name == "beta")
            .expect("beta should be discovered");
        assert!(beta.agents.is_empty(), "beta has no agents");
    }

    #[test]
    fn discover_project_without_config_toml_still_found() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("projects/bare")).unwrap();

        let projects = discover(tmp.path());
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "bare");
    }

    // --- has_default_lens ---

    #[test]
    fn has_default_lens_true_when_present() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("lenses/default")).unwrap();
        assert!(has_default_lens(tmp.path()));
    }

    #[test]
    fn has_default_lens_false_when_absent() {
        let tmp = TempDir::new().unwrap();
        assert!(!has_default_lens(tmp.path()));
    }
}

use crate::RESERVED_WORKFLOW_AGENT_ID;
use crate::error::{ConfigProblem, ConfigProblemKind};
use std::fs;
use std::path::{Path, PathBuf};

/// A space discovered by scanning the Swage directory skeleton.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredSpace {
    /// Directory name used as the space identifier.
    pub name: String,
    /// Absolute path to the space directory.
    pub path: PathBuf,
    /// Names of agents discovered under the space's `agents/` sub-directory.
    pub agents: Vec<String>,
}

/// Returns the effective Swage root directory.
pub fn effective_root_path() -> PathBuf {
    if let Ok(home_override) = std::env::var("SWAGE_HOME") {
        PathBuf::from(home_override)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        PathBuf::from(home).join(".swage")
    }
}

/// Returns the effective Swage root directory, creating it when needed.
pub fn resolve_root() -> Result<PathBuf, ConfigProblem> {
    let root = effective_root_path();
    if let Err(error) = fs::create_dir_all(&root) {
        return Err(ConfigProblem {
            file_path: root.clone(),
            field_path: None,
            kind: ConfigProblemKind::DiscoveryError {
                message: format!(
                    "failed to create Swage root directory '{}': {error}",
                    root.display()
                ),
            },
        });
    }
    if !root.is_dir() {
        return Err(ConfigProblem {
            file_path: root.clone(),
            field_path: None,
            kind: ConfigProblemKind::DiscoveryError {
                message: format!(
                    "Swage root path '{}' exists but is not a directory",
                    root.display()
                ),
            },
        });
    }
    Ok(root)
}

fn subdirectory_names(dir: &Path) -> Vec<String> {
    if !dir.exists() {
        return vec![];
    }
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Scan `{space_dir}/agents/` for subdirectories.
pub fn discover_agents(space_dir: &Path) -> Vec<String> {
    subdirectory_names(&space_dir.join("agents"))
        .into_iter()
        .filter(|name| name != RESERVED_WORKFLOW_AGENT_ID)
        .collect()
}

/// Scan `{root}/spaces/` for subdirectories.
pub fn discover_spaces(root: &Path) -> Vec<DiscoveredSpace> {
    subdirectory_names(&root.join("spaces"))
        .into_iter()
        .map(|name| {
            let path = root.join("spaces").join(&name);
            DiscoveredSpace {
                agents: discover_agents(&path),
                name,
                path,
            }
        })
        .collect()
}

/// Full discovery: returns all spaces and their agents under `root`.
pub fn discover(root: &Path) -> Vec<DiscoveredSpace> {
    discover_spaces(root)
}

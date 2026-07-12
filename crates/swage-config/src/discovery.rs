use crate::error::{ConfigProblem, ConfigProblemKind};
use crate::model::HierarchyLevel;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A Python assembly file discovered under an `assemblies/` directory.
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblyFile {
    /// Assembly name — the filename with the `.py` extension stripped.
    pub name: String,
    /// Absolute path to the `.py` file.
    pub path: PathBuf,
}

/// An assembly resolved to a specific hierarchy level.
///
/// Carries the name, file path, and which level of the config hierarchy
/// supplied the file (provenance).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAssembly {
    /// Assembly name (filename minus `.py`).
    pub name: String,
    /// Absolute path to the `.py` file.
    pub path: PathBuf,
    /// Which config hierarchy level this assembly came from.
    pub level: HierarchyLevel,
}

/// A space discovered by scanning the Swage directory skeleton.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredSpace {
    /// Directory name used as the space identifier.
    pub name: String,
    /// Absolute path to the space directory.
    pub path: PathBuf,
    /// Names of agents discovered under the space's `agents/` sub-directory.
    pub agents: Vec<String>,
    /// Names of teams discovered under the space's `teams/` sub-directory.
    pub teams: Vec<String>,
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
/// This is a pure path calculation; use [`resolve_root`] when the caller needs
/// the directory to exist.
pub fn effective_root_path() -> PathBuf {
    if let Ok(home_override) = std::env::var("SWAGE_HOME") {
        PathBuf::from(home_override)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        PathBuf::from(home).join(".swage")
    }
}

/// Returns the effective Swage root directory, creating it when needed.
///
/// Returns `DiscoveryError` only when the root cannot be created or the path is
/// not a directory.
pub fn resolve_root() -> Result<PathBuf, ConfigProblem> {
    let root = effective_root_path();

    if let Err(error) = fs::create_dir_all(&root) {
        return Err(ConfigProblem {
            file_path: root.clone(),
            field_path: None,
            kind: ConfigProblemKind::DiscoveryError {
                message: format!(
                    "failed to create Swage root directory '{}': {error}",
                    root.display(),
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

/// Scan `{space_dir}/agents/` for subdirectories.
/// Returns agent names (directory names), sorted alphabetically.
/// Returns an empty list if the `agents/` directory does not exist.
pub fn discover_agents(space_dir: &Path) -> Vec<String> {
    subdirectory_names(&space_dir.join("agents"))
}

/// Scan `{space_dir}/teams/` for subdirectories.
/// Returns team names (directory names), sorted alphabetically.
/// Returns an empty list if the `teams/` directory does not exist.
pub fn discover_teams(space_dir: &Path) -> Vec<String> {
    subdirectory_names(&space_dir.join("teams"))
}

/// Scan `{root}/spaces/` for subdirectories.
/// Each subdirectory is treated as a space regardless of whether it has
/// a `config.toml` — a space without one simply inherits global config.
/// Returns an empty list if the `spaces/` directory does not exist.
pub fn discover_spaces(root: &Path) -> Vec<DiscoveredSpace> {
    let spaces_dir = root.join("spaces");
    let names = subdirectory_names(&spaces_dir);

    names
        .into_iter()
        .map(|name| {
            let path = spaces_dir.join(&name);
            let agents = discover_agents(&path);
            let teams = discover_teams(&path);
            DiscoveredSpace {
                name,
                path,
                agents,
                teams,
            }
        })
        .collect()
}

/// Full discovery: returns all spaces and their agents under `root`.
///
/// Convenience wrapper around `discover_spaces` that also populates each
/// space's agent list.
pub fn discover(root: &Path) -> Vec<DiscoveredSpace> {
    discover_spaces(root)
}

/// Returns `true` if `{root}/lenses/default/` exists on disk.
pub fn has_default_lens(root: &Path) -> bool {
    root.join("lenses").join("default").exists()
}

/// Scan `dir` (an `assemblies/` directory) for `.py` files.
///
/// Returns one `AssemblyFile` per `.py` file found, with `name` set to the
/// filename minus the `.py` extension and `path` as the canonicalised absolute
/// path. Files that cannot be canonicalised are silently skipped. Non-`.py`
/// entries and subdirectories are ignored. Returns an empty list when `dir`
/// does not exist.
pub fn discover_assemblies(dir: &Path) -> Vec<AssemblyFile> {
    if !dir.exists() {
        return vec![];
    }

    let mut files: Vec<AssemblyFile> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        // Only regular files with a .py extension.
        .filter(|entry| {
            entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                && entry
                    .path()
                    .extension()
                    .map(|ext| ext == "py")
                    .unwrap_or(false)
        })
        .filter_map(|entry| {
            let path = entry.path().canonicalize().ok()?;
            // Strip the extension to produce the assembly name.
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(AssemblyFile { name, path })
        })
        .collect();

    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

/// Search `levels` in order for `<name>.py` inside each level's `assemblies/`
/// subdirectory.
///
/// `levels` should be ordered from highest to lowest priority (e.g. Agent →
/// Space → Global). The first match is returned with its provenance level.
/// Returns `None` if the assembly is not present at any level.
pub fn resolve_assembly(
    name: &str,
    levels: &[(HierarchyLevel, &Path)],
) -> Option<ResolvedAssembly> {
    let filename = format!("{name}.py");

    for (level, base_dir) in levels {
        let candidate = base_dir.join("assemblies").join(&filename);
        if candidate.exists() {
            // Canonicalise for a stable absolute path.
            let path = candidate.canonicalize().unwrap_or(candidate);
            return Some(ResolvedAssembly {
                name: name.to_owned(),
                path,
                level: level.clone(),
            });
        }
    }

    None
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

    // --- resolve_root ---

    #[test]
    fn resolve_root_uses_swage_home_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let tmp = TempDir::new().unwrap();
        let _env_guard = EnvVarGuard::set("SWAGE_HOME", tmp.path());
        let result = resolve_root();
        assert_eq!(result.unwrap(), tmp.path());
    }

    #[test]
    fn resolve_root_creates_missing_root() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("new-swage-root");
        let _env_guard = EnvVarGuard::set("SWAGE_HOME", &root);
        let result = resolve_root();
        assert_eq!(result.unwrap(), root);
        assert!(root.is_dir(), "missing root should be created");
    }

    #[test]
    fn resolve_root_errors_when_root_path_is_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("swage-file");
        fs::write(&root, "not a directory").unwrap();
        let _env_guard = EnvVarGuard::set("SWAGE_HOME", &root);
        let result = resolve_root();
        assert!(result.is_err(), "file root should produce an error");
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ConfigProblemKind::DiscoveryError { .. }));
    }

    // --- discover_spaces ---

    #[test]
    fn discover_spaces_finds_subdirectories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("spaces/alpha")).unwrap();
        fs::create_dir_all(tmp.path().join("spaces/beta")).unwrap();

        let spaces = discover_spaces(tmp.path());
        let names: Vec<&str> = spaces.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "should discover alpha");
        assert!(names.contains(&"beta"), "should discover beta");
    }

    #[test]
    fn discover_spaces_returns_empty_when_spaces_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let spaces = discover_spaces(tmp.path());
        assert!(spaces.is_empty());
    }

    #[test]
    fn discover_spaces_includes_path() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("spaces/myspace")).unwrap();

        let spaces = discover_spaces(tmp.path());
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].path, tmp.path().join("spaces/myspace"));
    }

    // --- discover_agents ---

    #[test]
    fn discover_agents_finds_agent_directories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("agents/coder")).unwrap();
        fs::create_dir_all(tmp.path().join("agents/reviewer")).unwrap();

        let agents = discover_agents(tmp.path());
        assert!(agents.contains(&"coder".to_string()), "should find coder");
        assert!(
            agents.contains(&"reviewer".to_string()),
            "should find reviewer"
        );
    }

    #[test]
    fn discover_agents_returns_empty_when_agents_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let agents = discover_agents(tmp.path());
        assert!(agents.is_empty());
    }

    // --- discover_teams ---

    #[test]
    fn discover_teams_finds_team_directories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("teams/research")).unwrap();
        fs::create_dir_all(tmp.path().join("teams/coding")).unwrap();

        let teams = discover_teams(tmp.path());
        assert!(
            teams.contains(&"research".to_string()),
            "should find research"
        );
        assert!(teams.contains(&"coding".to_string()), "should find coding");
    }

    #[test]
    fn discover_teams_returns_empty_when_teams_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let teams = discover_teams(tmp.path());
        assert!(teams.is_empty());
    }

    // --- discover ---

    #[test]
    fn discover_combines_spaces_and_agents() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("spaces/alpha/agents/assistant")).unwrap();
        fs::create_dir_all(tmp.path().join("spaces/alpha/agents/reviewer")).unwrap();
        fs::create_dir_all(tmp.path().join("spaces/beta")).unwrap();

        let spaces = discover(tmp.path());
        let alpha = spaces
            .iter()
            .find(|p| p.name == "alpha")
            .expect("alpha should be discovered");
        assert!(alpha.agents.contains(&"assistant".to_string()));
        assert!(alpha.agents.contains(&"reviewer".to_string()));

        let beta = spaces
            .iter()
            .find(|p| p.name == "beta")
            .expect("beta should be discovered");
        assert!(beta.agents.is_empty(), "beta has no agents");
    }

    #[test]
    fn discover_space_without_config_toml_still_found() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("spaces/bare")).unwrap();

        let spaces = discover(tmp.path());
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].name, "bare");
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

    // --- discover_assemblies ---

    #[test]
    fn discover_assemblies_returns_py_files() {
        let tmp = TempDir::new().unwrap();
        let asm_dir = tmp.path().join("assemblies");
        fs::create_dir_all(&asm_dir).unwrap();
        fs::write(asm_dir.join("custom.py"), b"").unwrap();
        fs::write(asm_dir.join("default.py"), b"").unwrap();
        // Non-.py file should be excluded.
        fs::write(asm_dir.join("readme.txt"), b"").unwrap();

        let mut files = discover_assemblies(&asm_dir);
        files.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(files.len(), 2, "only .py files should be returned");
        assert_eq!(files[0].name, "custom");
        assert_eq!(
            files[0].path,
            asm_dir.join("custom.py").canonicalize().unwrap()
        );
        assert_eq!(files[1].name, "default");
    }

    #[test]
    fn discover_assemblies_empty_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let files = discover_assemblies(&tmp.path().join("assemblies"));
        assert!(files.is_empty(), "missing dir should yield empty list");
    }

    #[test]
    fn discover_assemblies_empty_when_no_py_files() {
        let tmp = TempDir::new().unwrap();
        let asm_dir = tmp.path().join("assemblies");
        fs::create_dir_all(&asm_dir).unwrap();
        fs::write(asm_dir.join("notes.md"), b"").unwrap();

        let files = discover_assemblies(&asm_dir);
        assert!(files.is_empty());
    }

    // --- resolve_assembly ---

    #[test]
    fn resolve_assembly_finds_at_space_level() {
        use crate::model::HierarchyLevel;

        let global_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        // Place custom.py only at the space level.
        fs::create_dir_all(space_dir.path().join("assemblies")).unwrap();
        fs::write(space_dir.path().join("assemblies/custom.py"), b"").unwrap();

        let levels: Vec<(HierarchyLevel, &std::path::Path)> = vec![
            (HierarchyLevel::Agent, agent_dir.path()),
            (HierarchyLevel::Space, space_dir.path()),
            (HierarchyLevel::Global, global_dir.path()),
        ];

        let result = resolve_assembly("custom", &levels);
        assert!(result.is_some(), "custom.py at space level should be found");

        let resolved = result.unwrap();
        assert_eq!(resolved.name, "custom");
        assert_eq!(resolved.level, HierarchyLevel::Space);
        assert!(resolved.path.ends_with("custom.py"));
    }

    #[test]
    fn resolve_assembly_prefers_agent_over_space() {
        use crate::model::HierarchyLevel;

        let global_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        // Place custom.py at both agent and space levels.
        fs::create_dir_all(agent_dir.path().join("assemblies")).unwrap();
        fs::write(agent_dir.path().join("assemblies/custom.py"), b"").unwrap();
        fs::create_dir_all(space_dir.path().join("assemblies")).unwrap();
        fs::write(space_dir.path().join("assemblies/custom.py"), b"").unwrap();

        let levels: Vec<(HierarchyLevel, &std::path::Path)> = vec![
            (HierarchyLevel::Agent, agent_dir.path()),
            (HierarchyLevel::Space, space_dir.path()),
            (HierarchyLevel::Global, global_dir.path()),
        ];

        let resolved = resolve_assembly("custom", &levels).unwrap();
        assert_eq!(
            resolved.level,
            HierarchyLevel::Agent,
            "agent level should win over space when both have the file"
        );
    }

    #[test]
    fn resolve_assembly_returns_none_when_not_found() {
        use crate::model::HierarchyLevel;

        let global_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        let levels: Vec<(HierarchyLevel, &std::path::Path)> = vec![
            (HierarchyLevel::Agent, agent_dir.path()),
            (HierarchyLevel::Space, space_dir.path()),
            (HierarchyLevel::Global, global_dir.path()),
        ];

        let result = resolve_assembly("nonexistent", &levels);
        assert!(result.is_none(), "missing assembly should yield None");
    }

    #[test]
    fn resolve_assembly_empty_levels_returns_none() {
        use crate::model::HierarchyLevel;
        let levels: Vec<(HierarchyLevel, &std::path::Path)> = vec![];
        assert!(resolve_assembly("whatever", &levels).is_none());
    }
}

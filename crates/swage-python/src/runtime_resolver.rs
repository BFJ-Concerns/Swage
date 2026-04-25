use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use thiserror::Error;

use swage_config::model::{PythonExecuteToolConfig, PythonRuntimePolicy};

#[derive(Debug, Clone)]
pub struct ResolvedPythonRuntime {
    pub executable_path: PathBuf,
    pub runtime_root: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub read_only_mounts: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum PythonRuntimeError {
    #[error("Python runtime path does not exist: {path}")]
    RuntimeNotFound { path: PathBuf },
    #[error("Python runtime path is invalid: {path}")]
    InvalidRuntimePath { path: PathBuf },
    #[error("no Python runtime could be resolved")]
    RuntimeUnavailable,
    #[error("failed to probe Python runtime at {path}: {message}")]
    ProbeFailed { path: PathBuf, message: String },
}

#[derive(Debug, Deserialize)]
struct RuntimeProbe {
    executable: String,
    base_prefix: String,
    stdlib: Option<String>,
    platstdlib: Option<String>,
}

impl ResolvedPythonRuntime {
    pub fn resolve(config: &PythonExecuteToolConfig) -> Result<Self, PythonRuntimeError> {
        if let Some(path) = config.runtime_path.as_ref() {
            return Self::resolve_explicit(Path::new(path));
        }

        match config.runtime_policy {
            PythonRuntimePolicy::BundledThenSystem => {
                for candidate in bundled_runtime_candidates() {
                    if candidate.exists() && looks_like_runtime_dir(&candidate) {
                        return Self::resolve_bundled_root(&candidate);
                    }
                }

                let executable =
                    find_system_python().ok_or(PythonRuntimeError::RuntimeUnavailable)?;
                Self::resolve_system_executable(&executable)
            }
        }
    }

    fn resolve_explicit(path: &Path) -> Result<Self, PythonRuntimeError> {
        if !path.exists() {
            return Err(PythonRuntimeError::RuntimeNotFound {
                path: path.to_path_buf(),
            });
        }

        if path.is_file() {
            return Self::resolve_system_executable(path);
        }

        Self::resolve_bundled_root(path)
    }

    fn resolve_bundled_root(path: &Path) -> Result<Self, PythonRuntimeError> {
        if !looks_like_runtime_dir(path) {
            return Err(PythonRuntimeError::InvalidRuntimePath {
                path: path.to_path_buf(),
            });
        }

        let executable_path = bundled_python_executable(path).ok_or_else(|| {
            PythonRuntimeError::InvalidRuntimePath {
                path: path.to_path_buf(),
            }
        })?;

        let mut environment = BTreeMap::new();
        environment.insert("PYTHONHOME".to_string(), path.display().to_string());
        environment.insert(
            "PYTHONPATH".to_string(),
            path.join("lib").display().to_string(),
        );
        environment.insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());

        Ok(Self {
            executable_path,
            runtime_root: Some(path.to_path_buf()),
            environment,
            read_only_mounts: vec![path.to_path_buf()],
        })
    }

    fn resolve_system_executable(path: &Path) -> Result<Self, PythonRuntimeError> {
        let probe = probe_runtime(path)?;
        let executable_path = PathBuf::from(probe.executable);

        let mut read_only_mounts = vec![PathBuf::from(probe.base_prefix)];
        if let Some(stdlib) = probe.stdlib {
            read_only_mounts.push(PathBuf::from(stdlib));
        }
        if let Some(platstdlib) = probe.platstdlib {
            read_only_mounts.push(PathBuf::from(platstdlib));
        }

        for common_root in ["/lib", "/lib64", "/usr/lib64"] {
            let common_root = PathBuf::from(common_root);
            if common_root.exists() {
                read_only_mounts.push(common_root);
            }
        }

        dedupe_paths(&mut read_only_mounts);

        let mut environment = BTreeMap::new();
        environment.insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());

        Ok(Self {
            executable_path,
            runtime_root: None,
            environment,
            read_only_mounts,
        })
    }
}

fn bundled_runtime_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            candidates.push(parent.join("python"));
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".swage").join("python"));
    }

    candidates
}

fn looks_like_runtime_dir(path: &Path) -> bool {
    path.join("lib").is_dir()
        && (path.join("bin").is_dir() || path.join("Scripts").is_dir())
        && bundled_python_executable(path).is_some()
}

fn bundled_python_executable(path: &Path) -> Option<PathBuf> {
    [
        path.join("bin").join("python3"),
        path.join("bin").join("python"),
        path.join("Scripts").join("python.exe"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn find_system_python() -> Option<PathBuf> {
    ["python3", "python"].into_iter().find_map(find_on_path)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn probe_runtime(path: &Path) -> Result<RuntimeProbe, PythonRuntimeError> {
    const PROBE_CODE: &str = r#"
import json
import sys
import sysconfig

paths = sysconfig.get_paths()
print(json.dumps({
    "executable": sys.executable,
    "base_prefix": sys.base_prefix,
    "stdlib": paths.get("stdlib"),
    "platstdlib": paths.get("platstdlib"),
}))
"#;

    let output = Command::new(path)
        .arg("-S")
        .arg("-c")
        .arg(PROBE_CODE)
        .output()
        .map_err(|error| PythonRuntimeError::ProbeFailed {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;

    if !output.status.success() {
        return Err(PythonRuntimeError::ProbeFailed {
            path: path.to_path_buf(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    serde_json::from_slice::<RuntimeProbe>(&output.stdout).map_err(|error| {
        PythonRuntimeError::ProbeFailed {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })
}

fn dedupe_paths(paths: &mut Vec<PathBuf>) {
    paths.sort();
    paths.dedup();

    let original = std::mem::take(paths);
    for candidate in original {
        if paths.iter().any(|existing| candidate.starts_with(existing)) {
            continue;
        }
        paths.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_resolves_some_runtime() {
        let resolved = ResolvedPythonRuntime::resolve(&PythonExecuteToolConfig::default());
        assert!(
            resolved.is_ok(),
            "expected a Python runtime, got: {resolved:?}"
        );
    }

    #[test]
    fn missing_explicit_runtime_path_errors() {
        let config = PythonExecuteToolConfig {
            runtime_path: Some("/definitely/missing/python".to_string()),
            ..Default::default()
        };
        let error = ResolvedPythonRuntime::resolve(&config).unwrap_err();
        assert!(matches!(error, PythonRuntimeError::RuntimeNotFound { .. }));
    }
}

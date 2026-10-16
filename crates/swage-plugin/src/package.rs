//! A plugin package on disk: the manifest and the script it names, read from
//! a directory and copied into an install root by name and revision.

use std::path::{Path, PathBuf};

use crate::manifest::{MANIFEST_FILE, ManifestError, PluginManifest, validate_plugin_name};

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("{0}")]
    Manifest(#[from] ManifestError),
    #[error("plugin package {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("plugin package {path} is not a directory")]
    NotADirectory { path: PathBuf },
    #[error("plugin package {path} names script {script}, which is not a file in the package")]
    MissingScript { path: PathBuf, script: String },
    #[error("plugin {name} revision {revision} is already installed at {path}")]
    AlreadyInstalled {
        name: String,
        revision: u32,
        path: PathBuf,
    },
    #[error(
        "plugin package {path} holds a symbolic link at {link}, which an install never follows"
    )]
    SymbolicLink { path: PathBuf, link: PathBuf },
}

/// A package read from a directory: its manifest and script source.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginPackage {
    pub root: PathBuf,
    pub manifest: PluginManifest,
    pub script: String,
}

/// Read the package at `root`: the manifest at [`MANIFEST_FILE`], validated,
/// and the script it names.
pub fn read_package(root: &Path) -> Result<PluginPackage, PackageError> {
    if !root.is_dir() {
        return Err(PackageError::NotADirectory {
            path: root.to_path_buf(),
        });
    }
    let manifest_text =
        std::fs::read_to_string(root.join(MANIFEST_FILE)).map_err(|error| PackageError::Io {
            path: root.join(MANIFEST_FILE),
            message: error.to_string(),
        })?;
    let manifest = PluginManifest::parse(&manifest_text)?;
    let script_path = root.join(&manifest.script);
    if !script_path.is_file() {
        return Err(PackageError::MissingScript {
            path: root.to_path_buf(),
            script: manifest.script.clone(),
        });
    }
    let script = std::fs::read_to_string(&script_path).map_err(|error| PackageError::Io {
        path: script_path,
        message: error.to_string(),
    })?;
    Ok(PluginPackage {
        root: root.to_path_buf(),
        manifest,
        script,
    })
}

/// Where a plugin's revision lives under the install root.
pub fn install_dir(plugins_root: &Path, name: &str, revision: u32) -> PathBuf {
    plugins_root.join(name).join(revision.to_string())
}

/// Copy a package into the install root as `revision` of its plugin. The
/// target must not already exist: a revision is immutable once installed.
/// Returns the installed package, read back from its new root.
pub fn install_package(
    package: &PluginPackage,
    plugins_root: &Path,
    revision: u32,
) -> Result<PluginPackage, PackageError> {
    validate_plugin_name(&package.manifest.name)?;
    let target = install_dir(plugins_root, &package.manifest.name, revision);
    if target.exists() {
        return Err(PackageError::AlreadyInstalled {
            name: package.manifest.name.clone(),
            revision,
            path: target,
        });
    }
    if let Err(error) = copy_tree(&package.root, &package.root, &target) {
        // The revision directory is the next install's claim on that number,
        // so a refused copy must not leave it behind.
        if let Err(cleanup) = std::fs::remove_dir_all(&target)
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            return Err(PackageError::Io {
                path: target,
                message: format!("{error}; the partial copy could not be removed: {cleanup}"),
            });
        }
        return Err(error);
    }
    read_package(&target)
}

fn copy_tree(package_root: &Path, from: &Path, to: &Path) -> Result<(), PackageError> {
    let io = |path: &Path, error: std::io::Error| PackageError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    };
    std::fs::create_dir_all(to).map_err(|error| io(to, error))?;
    for entry in std::fs::read_dir(from).map_err(|error| io(from, error))? {
        let entry = entry.map_err(|error| io(from, error))?;
        let source = entry.path();
        let kind = entry.file_type().map_err(|error| io(&source, error))?;
        let destination = to.join(entry.file_name());
        if kind.is_symlink() {
            return Err(PackageError::SymbolicLink {
                path: package_root.to_path_buf(),
                link: source,
            });
        }
        if kind.is_dir() {
            copy_tree(package_root, &source, &destination)?;
        } else {
            std::fs::copy(&source, &destination).map_err(|error| io(&source, error))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
        name = "echo"
        version = "1.0.0"
        description = "Echoes its input."
        script = "src/plugin.js"
        [[provides.tools]]
        name = "echo"
        description = "Echo the text back."
        parameters = { type = "object" }
    "#;

    fn write_package(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(MANIFEST_FILE), MANIFEST).unwrap();
        std::fs::write(
            root.join("src/plugin.js"),
            "swage.provide.tool('echo', (p) => p);",
        )
        .unwrap();
        std::fs::write(root.join("README.md"), "# echo").unwrap();
    }

    #[test]
    fn a_package_is_its_manifest_and_script() {
        let dir = tempfile::tempdir().unwrap();
        write_package(dir.path());
        let package = read_package(dir.path()).expect("reads");
        assert_eq!(package.manifest.name, "echo");
        assert!(package.script.contains("swage.provide.tool"));
    }

    #[test]
    fn a_missing_script_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        write_package(dir.path());
        std::fs::remove_file(dir.path().join("src/plugin.js")).unwrap();
        let error = read_package(dir.path()).expect_err("refused");
        assert!(
            matches!(&error, PackageError::MissingScript { script, .. } if script == "src/plugin.js"),
            "{error}"
        );
    }

    #[test]
    fn an_install_copies_the_whole_package_under_name_and_revision_once() {
        let source = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        write_package(source.path());
        let package = read_package(source.path()).unwrap();
        let installed = install_package(&package, root.path(), 1).expect("installs");
        assert_eq!(installed.root, root.path().join("echo").join("1"));
        assert!(installed.root.join("README.md").is_file());
        assert_eq!(installed.script, package.script);
        let error = install_package(&package, root.path(), 1).expect_err("immutable");
        assert!(
            matches!(error, PackageError::AlreadyInstalled { revision: 1, .. }),
            "{error}"
        );
        install_package(&package, root.path(), 2).expect("a new revision");
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_in_the_package_is_never_followed() {
        let source = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        write_package(source.path());
        std::os::unix::fs::symlink("/etc/hostname", source.path().join("hostname")).unwrap();
        let package = read_package(source.path()).unwrap();
        let error = install_package(&package, root.path(), 1).expect_err("refused");
        assert!(
            matches!(error, PackageError::SymbolicLink { .. }),
            "{error}"
        );
        assert!(
            !install_dir(root.path(), &package.manifest.name, 1).exists(),
            "a refused copy must not keep the revision directory"
        );
    }
}

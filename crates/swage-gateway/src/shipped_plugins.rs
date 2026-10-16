//! Plugins compiled into the binary: the shipped parts that run on the
//! plugin contracts. Each is installed through the same path an operator's
//! package takes, with provenance `shipped`, so it is governed, presented
//! and invoked as any installed plugin — the provenance is the one thing
//! that tells them apart.

use std::path::{Path, PathBuf};

pub(crate) struct ShippedPluginFile {
    pub(crate) path: &'static str,
    pub(crate) contents: &'static str,
}

pub(crate) struct ShippedPlugin {
    pub(crate) name: &'static str,
    pub(crate) files: &'static [ShippedPluginFile],
}

macro_rules! shipped_plugin_file {
    ($path:literal) => {
        ShippedPluginFile {
            path: $path,
            contents: include_str!(concat!("../../../shipped/plugins/", $path)),
        }
    };
}

pub(crate) const SHIPPED_PLUGINS: &[ShippedPlugin] = &[ShippedPlugin {
    name: "succession-summariser",
    files: &[
        shipped_plugin_file!("succession-summariser/plugin.toml"),
        shipped_plugin_file!("succession-summariser/plugin.js"),
    ],
}];

const PROJECTION_DIR: &str = "generated/shipped-plugins";

pub(crate) fn root(data_root: &Path) -> PathBuf {
    data_root.join(PROJECTION_DIR)
}

/// The directory a shipped plugin's package is projected to — the source
/// its install reads, as an operator's package directory is.
pub(crate) fn package_dir(data_root: &Path, name: &str) -> PathBuf {
    root(data_root).join(name)
}

/// Replace the complete projection with the packages compiled into this
/// binary.
pub(crate) async fn regenerate(data_root: &Path) -> std::io::Result<()> {
    let projection_root = root(data_root);
    match tokio::fs::remove_dir_all(&projection_root).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for plugin in SHIPPED_PLUGINS {
        for file in plugin.files {
            let relative = file
                .path
                .strip_prefix(plugin.name)
                .and_then(|path| path.strip_prefix('/'))
                .expect("catalogue file belongs to its plugin");
            let path = package_dir(data_root, plugin.name).join(relative);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, file.contents).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_shipped_plugin_projects_to_a_package_its_manifest_reads() {
        let root = tempfile::tempdir().unwrap();
        regenerate(root.path()).await.unwrap();
        for plugin in SHIPPED_PLUGINS {
            let package = swage_plugin::read_package(&package_dir(root.path(), plugin.name))
                .expect("the projected package reads");
            assert_eq!(package.manifest.name, plugin.name);
        }
    }
}

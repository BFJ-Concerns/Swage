//! Disposable filesystem projection of compiled shipped-skill resources.

use std::path::{Path, PathBuf};

use crate::shipped_skills::SHIPPED_SKILLS;

const PROJECTION_DIR: &str = "generated/shipped-skills";

pub(crate) fn root(data_root: &Path) -> PathBuf {
    data_root.join(PROJECTION_DIR)
}

pub(crate) fn skill_dir(data_root: &Path, skill_name: &str) -> PathBuf {
    root(data_root).join(skill_name)
}

/// Replace the complete projection with the resources compiled into this binary.
pub(crate) async fn regenerate(data_root: &Path) -> std::io::Result<()> {
    let projection_root = root(data_root);
    match tokio::fs::remove_dir_all(&projection_root).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    for skill in SHIPPED_SKILLS {
        for file in skill.files {
            let relative = file
                .path
                .strip_prefix(skill.name)
                .and_then(|path| path.strip_prefix('/'))
                .expect("catalogue file belongs to its skill");
            let path = skill_dir(data_root, skill.name).join(relative);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, file.contents).await?;
            set_executable_if_script(&path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable_if_script(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.extension().and_then(|extension| extension.to_str()) == Some("sh") {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_if_script(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

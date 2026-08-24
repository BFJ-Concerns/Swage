//! Gateway-owned lifecycle for runtime-provisioned worker git worktrees.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
};

use tokio::{process::Command, sync::Mutex};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct WorktreeConfig {
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedWorktree {
    pub owner_id: Uuid,
    pub seq: u64,
    pub repo: PathBuf,
    pub path: PathBuf,
}

pub struct WorktreeHost {
    config: WorktreeConfig,
    inner: Mutex<HashMap<(Uuid, u64), ProvisionedWorktree>>,
}

impl WorktreeHost {
    pub fn new(config: WorktreeConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub async fn provision(
        &self,
        owner_id: Uuid,
        seq: u64,
        repo: &Path,
    ) -> Result<PathBuf, String> {
        let path = self
            .config
            .root
            .join(owner_id.to_string())
            .join(format!("worker-{seq}"));
        self.provision_at(owner_id, seq, repo, path).await
    }

    pub async fn restore(
        &self,
        owner_id: Uuid,
        seq: u64,
        repo: &Path,
        path: &Path,
    ) -> Result<PathBuf, String> {
        self.provision_at(owner_id, seq, repo, path.to_path_buf())
            .await
    }

    async fn provision_at(
        &self,
        owner_id: Uuid,
        seq: u64,
        repo: &Path,
        path: PathBuf,
    ) -> Result<PathBuf, String> {
        let key = (owner_id, seq);
        if let Some(existing) = self.inner.lock().await.get(&key) {
            return Ok(existing.path.clone());
        }
        let repo = tokio::fs::canonicalize(repo).await.map_err(|error| {
            format!(
                "cannot resolve placement repository {}: {error}",
                repo.display()
            )
        })?;
        if path.join(".git").exists() {
            self.inner.lock().await.insert(
                key,
                ProvisionedWorktree {
                    owner_id,
                    seq,
                    repo,
                    path: path.clone(),
                },
            );
            return Ok(path);
        }
        tokio::fs::create_dir_all(path.parent().expect("worktree path has a parent"))
            .await
            .map_err(|error| format!("cannot create worktree root: {error}"))?;
        let output = Command::new("git")
            .args([
                "-C",
                &repo.to_string_lossy(),
                "worktree",
                "add",
                "--detach",
                &path.to_string_lossy(),
                "HEAD",
            ])
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| format!("failed to start git worktree provisioning: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git worktree provisioning failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        self.inner.lock().await.insert(
            key,
            ProvisionedWorktree {
                owner_id,
                seq,
                repo,
                path: path.clone(),
            },
        );
        Ok(path)
    }

    pub async fn list_owner(&self, owner_id: Uuid) -> Vec<ProvisionedWorktree> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|item| item.owner_id == owner_id)
            .cloned()
            .collect()
    }

    /// Reclaim worktrees left behind by a gateway that did not shut down.
    ///
    /// Failed and cancelled owners remain available until the gateway exits or
    /// restarts. At startup no live owner can exist, so every registered
    /// worktree beneath the host's UUID/worker-sequence layout is stale.
    pub async fn recover(&self) -> Result<usize, String> {
        let mut owners = match tokio::fs::read_dir(&self.config.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(format!(
                    "cannot read managed worktree root {}: {error}",
                    self.config.root.display()
                ));
            }
        };
        let mut recovered = 0;
        while let Some(owner) = owners
            .next_entry()
            .await
            .map_err(|error| format!("cannot read managed worktree owner: {error}"))?
        {
            if !owner
                .file_type()
                .await
                .map_err(|error| format!("cannot inspect managed worktree owner: {error}"))?
                .is_dir()
                || Uuid::parse_str(&owner.file_name().to_string_lossy()).is_err()
            {
                continue;
            }
            let owner_path = owner.path();
            let mut workers = tokio::fs::read_dir(&owner_path).await.map_err(|error| {
                format!(
                    "cannot read managed worktree owner {}: {error}",
                    owner_path.display()
                )
            })?;
            while let Some(worker) = workers
                .next_entry()
                .await
                .map_err(|error| format!("cannot read managed worktree: {error}"))?
            {
                let worker_name = worker.file_name();
                let Some(sequence) = worker_name
                    .to_str()
                    .and_then(|name| name.strip_prefix("worker-"))
                else {
                    continue;
                };
                if sequence.parse::<u64>().is_err()
                    || !worker
                        .file_type()
                        .await
                        .map_err(|error| format!("cannot inspect managed worktree: {error}"))?
                        .is_dir()
                {
                    continue;
                }
                let path = worker.path();
                if !path.join(".git").is_file() {
                    continue;
                }
                let canonical_path = tokio::fs::canonicalize(&path).await.map_err(|error| {
                    format!(
                        "cannot resolve managed worktree {}: {error}",
                        path.display()
                    )
                })?;
                let listed = Command::new("git")
                    .args([
                        "-C",
                        &path.to_string_lossy(),
                        "worktree",
                        "list",
                        "--porcelain",
                    ])
                    .stdin(Stdio::null())
                    .output()
                    .await
                    .map_err(|error| {
                        format!(
                            "failed to inspect managed worktree {}: {error}",
                            path.display()
                        )
                    })?;
                if !listed.status.success() {
                    return Err(format!(
                        "cannot inspect managed worktree {}: {}",
                        path.display(),
                        String::from_utf8_lossy(&listed.stderr).trim()
                    ));
                }
                let registered = String::from_utf8_lossy(&listed.stdout).lines().any(|line| {
                    line.strip_prefix("worktree ")
                        .is_some_and(|listed_path| Path::new(listed_path) == canonical_path)
                });
                if !registered {
                    continue;
                }
                let removed = Command::new("git")
                    .args([
                        "-C",
                        &path.to_string_lossy(),
                        "worktree",
                        "remove",
                        "--force",
                        &path.to_string_lossy(),
                    ])
                    .stdin(Stdio::null())
                    .output()
                    .await
                    .map_err(|error| {
                        format!(
                            "failed to start worktree recovery for {}: {error}",
                            path.display()
                        )
                    })?;
                if !removed.status.success() {
                    return Err(format!(
                        "worktree recovery failed for {}: {}",
                        path.display(),
                        String::from_utf8_lossy(&removed.stderr).trim()
                    ));
                }
                recovered += 1;
            }
            let _ = tokio::fs::remove_dir(owner_path).await;
        }
        Ok(recovered)
    }

    pub async fn cleanup_owner(&self, owner_id: Uuid) {
        let resources = {
            let mut inner = self.inner.lock().await;
            let keys = inner
                .keys()
                .filter(|(id, _)| *id == owner_id)
                .copied()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| inner.remove(&key))
                .collect::<Vec<_>>()
        };
        for resource in resources {
            let _ = Command::new("git")
                .args([
                    "-C",
                    &resource.repo.to_string_lossy(),
                    "worktree",
                    "remove",
                    "--force",
                    &resource.path.to_string_lossy(),
                ])
                .stdin(Stdio::null())
                .output()
                .await;
        }
        let _ = tokio::fs::remove_dir(self.config.root.join(owner_id.to_string())).await;
    }

    pub async fn shutdown(&self) {
        let owner_ids = self
            .inner
            .lock()
            .await
            .values()
            .map(|item| item.owner_id)
            .collect::<std::collections::HashSet<_>>();
        for owner_id in owner_ids {
            self.cleanup_owner(owner_id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provisions_isolated_siblings_and_lists_then_removes_them() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        tokio::fs::create_dir(&repo).await.unwrap();
        for args in [
            &["init"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "Test"][..],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .await
                    .unwrap()
                    .success()
            );
        }
        tokio::fs::write(repo.join("seed"), "seed").await.unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["add", "seed"])
                .status()
                .await
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["commit", "-m", "seed"])
                .status()
                .await
                .unwrap()
                .success()
        );
        let config = WorktreeConfig {
            root: temp.path().join("worktrees"),
        };
        let host = WorktreeHost::new(config.clone());
        let owner_id = Uuid::new_v4();
        let first = host.provision(owner_id, 0, &repo).await.unwrap();
        let second = host.provision(owner_id, 1, &repo).await.unwrap();
        tokio::fs::write(first.join("first-only"), "one")
            .await
            .unwrap();
        tokio::fs::write(second.join("second-only"), "two")
            .await
            .unwrap();
        assert!(!second.join("first-only").exists());
        assert!(!first.join("second-only").exists());
        assert_eq!(host.list_owner(owner_id).await.len(), 2);
        let resumed_host = WorktreeHost::new(config);
        let resumed = resumed_host.provision(owner_id, 0, &repo).await.unwrap();
        assert_eq!(resumed, first, "resume must reuse the owned path");
        let listed = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&listed.stdout)
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            3,
            "resume must not provision a duplicate beside the main tree and two workers"
        );
        resumed_host.shutdown().await;
        host.cleanup_owner(owner_id).await;
        assert!(!first.exists() && !second.exists());
        assert!(
            !temp
                .path()
                .join("worktrees")
                .join(owner_id.to_string())
                .exists()
        );
    }

    #[tokio::test]
    async fn restart_recovers_registered_worktrees_for_retained_terminal_owners() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        tokio::fs::create_dir(&repo).await.unwrap();
        for args in [
            &["init"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "Test"][..],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .await
                    .unwrap()
                    .success()
            );
        }
        tokio::fs::write(repo.join("seed"), "seed").await.unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["add", "seed"])
                .status()
                .await
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["commit", "-m", "seed"])
                .status()
                .await
                .unwrap()
                .success()
        );
        let config = WorktreeConfig {
            root: temp.path().join("worktrees"),
        };
        let failed_owner = Uuid::new_v4();
        let cancelled_owner = Uuid::new_v4();
        let failed_path;
        let cancelled_path;
        {
            let host = WorktreeHost::new(config.clone());
            failed_path = host.provision(failed_owner, 0, &repo).await.unwrap();
            cancelled_path = host.provision(cancelled_owner, 0, &repo).await.unwrap();
            assert!(failed_path.exists() && cancelled_path.exists());
            // Dropping without shutdown models an unclean gateway exit.
        }
        let unrelated = config.root.join("not-a-managed-owner");
        tokio::fs::create_dir_all(&unrelated).await.unwrap();
        tokio::fs::write(unrelated.join("keep"), "keep")
            .await
            .unwrap();

        let recovered = WorktreeHost::new(config.clone()).recover().await.unwrap();

        assert_eq!(recovered, 2);
        assert!(!failed_path.exists() && !cancelled_path.exists());
        assert!(unrelated.join("keep").exists());
        let listed = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&listed.stdout)
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1,
            "recovery must remove registrations as well as directories"
        );
    }
}

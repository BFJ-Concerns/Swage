//! Gateway-owned lifecycle for runtime-provisioned worker git worktrees.

use std::{
    collections::{HashMap, HashSet},
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
    pub base_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCompletion {
    pub base_commit: String,
    pub tip_commit: String,
    pub changed: bool,
    pub removed: bool,
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
    ) -> Result<ProvisionedWorktree, String> {
        let path = self
            .config
            .root
            .join(owner_id.to_string())
            .join(format!("worker-{seq}"));
        self.provision_at(owner_id, seq, repo, path, None).await
    }

    pub async fn restore(
        &self,
        owner_id: Uuid,
        seq: u64,
        repo: &Path,
        path: &Path,
        base_commit: Option<String>,
    ) -> Result<ProvisionedWorktree, String> {
        self.provision_at(owner_id, seq, repo, path.to_path_buf(), base_commit)
            .await
    }

    async fn provision_at(
        &self,
        owner_id: Uuid,
        seq: u64,
        repo: &Path,
        path: PathBuf,
        restored_base_commit: Option<String>,
    ) -> Result<ProvisionedWorktree, String> {
        let key = (owner_id, seq);
        if let Some(existing) = self.inner.lock().await.get(&key) {
            return Ok(existing.clone());
        }
        let repo = tokio::fs::canonicalize(repo).await.map_err(|error| {
            format!(
                "cannot resolve placement repository {}: {error}",
                repo.display()
            )
        })?;
        if path.join(".git").exists() {
            let base_commit =
                restored_base_commit.unwrap_or(git_stdout(&path, ["rev-parse", "HEAD"]).await?);
            let worktree = ProvisionedWorktree {
                owner_id,
                seq,
                repo,
                path,
                base_commit,
            };
            self.inner.lock().await.insert(key, worktree.clone());
            return Ok(worktree);
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
        let base_commit = git_stdout(&path, ["rev-parse", "HEAD"]).await?;
        let worktree = ProvisionedWorktree {
            owner_id,
            seq,
            repo,
            path,
            base_commit,
        };
        self.inner.lock().await.insert(key, worktree.clone());
        Ok(worktree)
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

    pub async fn finish(&self, owner_id: Uuid, seq: u64) -> Result<WorktreeCompletion, String> {
        let resource = self
            .inner
            .lock()
            .await
            .get(&(owner_id, seq))
            .cloned()
            .ok_or_else(|| format!("managed worktree {owner_id}/{seq} is not registered"))?;
        let status = git_stdout(
            &resource.path,
            ["status", "--porcelain=v1", "--untracked-files=all"],
        )
        .await?;
        let tip_commit = git_stdout(&resource.path, ["rev-parse", "HEAD"]).await?;
        let changed = !status.trim().is_empty() || tip_commit != resource.base_commit;
        if changed {
            self.inner.lock().await.remove(&(owner_id, seq));
            return Ok(WorktreeCompletion {
                base_commit: resource.base_commit,
                tip_commit,
                changed: true,
                removed: false,
            });
        }
        remove_worktree(&resource.repo, &resource.path).await?;
        self.inner.lock().await.remove(&(owner_id, seq));
        let _ = tokio::fs::remove_dir(self.config.root.join(owner_id.to_string())).await;
        Ok(WorktreeCompletion {
            base_commit: resource.base_commit,
            tip_commit,
            changed: false,
            removed: true,
        })
    }

    /// Reclaim worktrees left behind by a gateway that did not shut down.
    ///
    /// Workflow owners that replay from their durable journal are retained for
    /// the wake loop. Failed, cancelled, and delegated-worker owners have no
    /// such live replay claim and are reclaimed.
    pub async fn recover(&self, retained_owner_ids: &HashSet<Uuid>) -> Result<usize, String> {
        self.recover_with_git(retained_owner_ids, Path::new("git"))
            .await
    }

    async fn recover_with_git(
        &self,
        retained_owner_ids: &HashSet<Uuid>,
        git: &Path,
    ) -> Result<usize, String> {
        let mut owners = match tokio::fs::read_dir(&self.config.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                tracing::warn!(path = %self.config.root.display(), %error, "cannot enumerate managed worktree root; leaving it untouched");
                return Ok(0);
            }
        };
        let mut recovered = 0;
        loop {
            let owner = match owners.next_entry().await {
                Ok(Some(owner)) => owner,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(path = %self.config.root.display(), %error, "cannot enumerate managed worktree root; leaving its remaining entries untouched");
                    break;
                }
            };
            let Ok(owner_type) = owner.file_type().await else {
                tracing::warn!(path = %owner.path().display(), "cannot inspect managed worktree owner; leaving it untouched");
                continue;
            };
            let Ok(owner_id) = Uuid::parse_str(&owner.file_name().to_string_lossy()) else {
                continue;
            };
            if !owner_type.is_dir() || retained_owner_ids.contains(&owner_id) {
                continue;
            }
            let owner_path = owner.path();
            let mut workers = match tokio::fs::read_dir(&owner_path).await {
                Ok(workers) => workers,
                Err(error) => {
                    tracing::warn!(path = %owner_path.display(), %error, "cannot inspect managed worktree owner; leaving it untouched");
                    continue;
                }
            };
            loop {
                let worker = match workers.next_entry().await {
                    Ok(Some(worker)) => worker,
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(path = %owner_path.display(), %error, "cannot enumerate managed worktree owner; leaving its remaining entries untouched");
                        break;
                    }
                };
                let worker_name = worker.file_name();
                let Some(sequence) = worker_name
                    .to_str()
                    .and_then(|name| name.strip_prefix("worker-"))
                else {
                    continue;
                };
                let Ok(worker_type) = worker.file_type().await else {
                    tracing::warn!(path = %worker.path().display(), "cannot inspect managed worktree candidate; leaving it untouched");
                    continue;
                };
                if sequence.parse::<u64>().is_err() || !worker_type.is_dir() {
                    continue;
                }
                let path = worker.path();
                if !path.join(".git").is_file() {
                    continue;
                }
                let canonical_path = match tokio::fs::canonicalize(&path).await {
                    Ok(path) => path,
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "cannot resolve managed worktree candidate; leaving it untouched");
                        continue;
                    }
                };
                let listed = match Command::new(git)
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
                {
                    Ok(listed) => listed,
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "cannot inspect managed worktree candidate; leaving it untouched");
                        continue;
                    }
                };
                if !listed.status.success() {
                    tracing::warn!(path = %path.display(), error = %String::from_utf8_lossy(&listed.stderr).trim(), "cannot inspect managed worktree candidate; leaving it untouched");
                    continue;
                }
                let registered = String::from_utf8_lossy(&listed.stdout).lines().any(|line| {
                    line.strip_prefix("worktree ")
                        .is_some_and(|listed_path| Path::new(listed_path) == canonical_path)
                });
                if !registered {
                    continue;
                }
                if let Err(error) = remove_worktree(&path, &path).await {
                    tracing::warn!(path = %path.display(), %error, "worktree recovery failed; leaving candidate untouched");
                    continue;
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
            let _ = remove_worktree(&resource.repo, &resource.path).await;
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

async fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| format!("failed to inspect worktree {}: {error}", repo.display()))?;
    if !output.status.success() {
        return Err(format!(
            "git inspection failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn remove_worktree(repo: &Path, path: &Path) -> Result<(), String> {
    let removed = Command::new("git")
        .args([
            "-C",
            &repo.to_string_lossy(),
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
                "failed to start worktree removal for {}: {error}",
                path.display()
            )
        })?;
    if removed.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git worktree removal failed: {}",
            String::from_utf8_lossy(&removed.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finalises_changed_and_unchanged_worktrees_differently() {
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
        tokio::fs::write(first.path.join("first-only"), "one")
            .await
            .unwrap();
        tokio::fs::write(second.path.join("second-only"), "two")
            .await
            .unwrap();
        assert!(!second.path.join("first-only").exists());
        assert!(!first.path.join("second-only").exists());
        tokio::fs::remove_file(second.path.join("second-only"))
            .await
            .unwrap();
        assert_eq!(host.list_owner(owner_id).await.len(), 2);
        let resumed_host = WorktreeHost::new(config);
        let resumed = resumed_host.provision(owner_id, 0, &repo).await.unwrap();
        assert_eq!(resumed.path, first.path, "resume must reuse the owned path");
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
        let changed = host.finish(owner_id, 0).await.unwrap();
        let removed = host.finish(owner_id, 1).await.unwrap();
        assert_eq!(changed.base_commit, first.base_commit);
        assert_eq!(changed.tip_commit, first.base_commit);
        assert!(changed.changed && !changed.removed);
        assert!(!removed.changed && removed.removed);
        assert!(first.path.exists(), "changed worktree remains for merging");
        assert!(!second.path.exists(), "unchanged worktree is removed");
        resumed_host.shutdown().await;
        host.cleanup_owner(owner_id).await;
        assert!(!first.path.exists() && !second.path.exists());
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
            failed_path = host.provision(failed_owner, 0, &repo).await.unwrap().path;
            cancelled_path = host
                .provision(cancelled_owner, 0, &repo)
                .await
                .unwrap()
                .path;
            assert!(failed_path.exists() && cancelled_path.exists());
            // Dropping without shutdown models an unclean gateway exit.
        }
        let unrelated = config.root.join("not-a-managed-owner");
        tokio::fs::create_dir_all(&unrelated).await.unwrap();
        tokio::fs::write(unrelated.join("keep"), "keep")
            .await
            .unwrap();
        let uninspectable = config
            .root
            .join(Uuid::new_v4().to_string())
            .join("worker-0");
        tokio::fs::create_dir_all(&uninspectable).await.unwrap();
        tokio::fs::write(uninspectable.join(".git"), "gitdir: /missing")
            .await
            .unwrap();

        let recovered = WorktreeHost::new(config.clone())
            .recover(&HashSet::new())
            .await
            .unwrap();

        assert_eq!(recovered, 2);
        assert!(!failed_path.exists() && !cancelled_path.exists());
        assert!(unrelated.join("keep").exists());
        assert!(
            uninspectable.exists(),
            "an uninspectable candidate must not block recovery or be removed"
        );
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

    #[tokio::test]
    async fn recovery_leaves_unenumerable_root_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worktrees");
        tokio::fs::write(&root, "not a directory").await.unwrap();
        let host = WorktreeHost::new(WorktreeConfig { root: root.clone() });

        assert_eq!(host.recover(&HashSet::new()).await.unwrap(), 0);
        assert_eq!(
            tokio::fs::read_to_string(root).await.unwrap(),
            "not a directory"
        );
    }

    #[tokio::test]
    async fn recovery_leaves_candidate_untouched_when_git_cannot_start() {
        let temp = tempfile::tempdir().unwrap();
        let candidate = temp
            .path()
            .join("worktrees")
            .join(Uuid::new_v4().to_string())
            .join("worker-0");
        tokio::fs::create_dir_all(&candidate).await.unwrap();
        tokio::fs::write(candidate.join(".git"), "gitdir: /missing")
            .await
            .unwrap();
        let host = WorktreeHost::new(WorktreeConfig {
            root: temp.path().join("worktrees"),
        });

        assert_eq!(
            host.recover_with_git(&HashSet::new(), &temp.path().join("missing-git"))
                .await
                .unwrap(),
            0
        );
        assert!(
            candidate.exists(),
            "a candidate must remain untouched when Git inspection cannot start"
        );
    }
}

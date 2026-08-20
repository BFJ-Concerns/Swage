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
    pub run_id: Uuid,
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

    pub async fn provision(&self, run_id: Uuid, seq: u64, repo: &Path) -> Result<PathBuf, String> {
        let key = (run_id, seq);
        if let Some(existing) = self.inner.lock().await.get(&key) {
            return Ok(existing.path.clone());
        }
        let repo = tokio::fs::canonicalize(repo).await.map_err(|error| {
            format!(
                "cannot resolve placement repository {}: {error}",
                repo.display()
            )
        })?;
        let path = self
            .config
            .root
            .join(run_id.to_string())
            .join(format!("worker-{seq}"));
        if path.join(".git").exists() {
            self.inner.lock().await.insert(
                key,
                ProvisionedWorktree {
                    run_id,
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
                run_id,
                seq,
                repo,
                path: path.clone(),
            },
        );
        Ok(path)
    }

    pub async fn list_run(&self, run_id: Uuid) -> Vec<ProvisionedWorktree> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|item| item.run_id == run_id)
            .cloned()
            .collect()
    }

    pub async fn cleanup_run(&self, run_id: Uuid) {
        let resources = {
            let mut inner = self.inner.lock().await;
            let keys = inner
                .keys()
                .filter(|(id, _)| *id == run_id)
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
    }

    pub async fn shutdown(&self) {
        let run_ids = self
            .inner
            .lock()
            .await
            .values()
            .map(|item| item.run_id)
            .collect::<std::collections::HashSet<_>>();
        for run_id in run_ids {
            self.cleanup_run(run_id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provisions_isolated_siblings_and_lists_then_removes_them() {
        let temp = tempfile::tempdir_in("/tmp/scratch").unwrap();
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
        let run_id = Uuid::new_v4();
        let first = host.provision(run_id, 0, &repo).await.unwrap();
        let second = host.provision(run_id, 1, &repo).await.unwrap();
        tokio::fs::write(first.join("first-only"), "one")
            .await
            .unwrap();
        tokio::fs::write(second.join("second-only"), "two")
            .await
            .unwrap();
        assert!(!second.join("first-only").exists());
        assert!(!first.join("second-only").exists());
        assert_eq!(host.list_run(run_id).await.len(), 2);
        let resumed_host = WorktreeHost::new(config);
        let resumed = resumed_host.provision(run_id, 0, &repo).await.unwrap();
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
        host.cleanup_run(run_id).await;
        assert!(!first.exists() && !second.exists());
    }
}

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
}

/// Names the placements a run's terminal account points a reader at. The
/// marker sits in the owner's directory beside the worker directories, never
/// inside one, so a worker's own git worktree stays clean.
const RETAINED_PLACEMENTS_MARKER: &str = ".retained-placements";

pub struct WorktreeHost {
    config: WorktreeConfig,
    inner: Mutex<HashMap<(Uuid, u64), ProvisionedWorktree>>,
    retained: Mutex<HashSet<PathBuf>>,
}

impl WorktreeHost {
    pub fn new(config: WorktreeConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(HashMap::new()),
            retained: Mutex::new(HashSet::new()),
        }
    }

    /// Keep the placements a run's terminal account names.
    ///
    /// A run that ended without returning offers its unfinished work through
    /// the directory each worker ran in, so that directory has to outlive
    /// both this gateway's shutdown and the next one's reclaim sweep. The
    /// claim is durable — a marker beside the worker directories — because
    /// the process that wrote the account is rarely the process a reader
    /// comes back to.
    pub async fn retain(&self, owner_id: Uuid, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        self.retained.lock().await.extend(paths.iter().cloned());
        let mut claims = HashMap::<PathBuf, HashSet<PathBuf>>::new();
        for path in paths {
            let owner_root = path
                .strip_prefix(&self.config.root)
                .ok()
                .and_then(|relative| relative.components().next())
                .map(|owner| self.config.root.join(owner.as_os_str()))
                .unwrap_or_else(|| self.config.root.join(owner_id.to_string()));
            if path.starts_with(&owner_root) {
                claims.entry(owner_root).or_default().insert(path);
            }
        }
        for (owner_root, claimed) in claims {
            if !owner_root.is_dir() {
                continue;
            }
            let marker = owner_root.join(RETAINED_PLACEMENTS_MARKER);
            let mut recorded = read_retained_placements(&marker).await;
            recorded.extend(claimed);
            let mut lines = recorded
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            lines.sort();
            if let Err(error) = tokio::fs::write(&marker, lines.join("\n") + "\n").await {
                tracing::warn!(
                    path = %marker.display(),
                    %error,
                    "could not claim the run's retained placements on disk; they survive only until this gateway stops"
                );
            }
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
            if !owner_type.is_dir() {
                continue;
            }
            let owner_path = owner.path();
            // A placement a terminal account claimed outlives the gateway
            // that wrote the account: the claim is read back here so this
            // process both leaves it alone and honours it in its own cleanup.
            let retained =
                read_retained_placements(&owner_path.join(RETAINED_PLACEMENTS_MARKER)).await;
            self.retained.lock().await.extend(retained.iter().cloned());
            if retained_owner_ids.contains(&owner_id) {
                continue;
            }
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
                if retained.contains(&path) || retained.contains(&canonical_path) {
                    continue;
                }
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

    /// Reclaim an owner's worktrees, except the placements a terminal account
    /// has claimed. A claimed placement keeps its entry in the map as well as
    /// its directory on disk: it is still a worktree this host provisioned,
    /// and the account names it for a reader who has not read it yet.
    pub async fn cleanup_owner(&self, owner_id: Uuid) {
        let retained = self.retained.lock().await.clone();
        let resources = {
            let mut inner = self.inner.lock().await;
            let keys = inner
                .iter()
                .filter(|((id, _), worktree)| *id == owner_id && !retained.contains(&worktree.path))
                .map(|(key, _)| *key)
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

/// The placements claimed in one owner's marker, or none where no claim has
/// been written — an owner with nothing to keep reads the same as an owner
/// whose marker cannot be read, and both leave the ordinary sweep in charge.
async fn read_retained_placements(marker: &Path) -> HashSet<PathBuf> {
    match tokio::fs::read_to_string(marker).await {
        Ok(claimed) => claimed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
        Err(error) => {
            tracing::warn!(path = %marker.display(), %error, "cannot read retained placement claims");
            HashSet::new()
        }
    }
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

    /// A one-commit git repository for worktrees to be cut from.
    async fn seed_repo(repo: &Path) {
        tokio::fs::create_dir(repo).await.unwrap();
        for args in [
            &["init"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "Test"][..],
        ] {
            assert!(git(repo, args).await);
        }
        tokio::fs::write(repo.join("seed"), "seed").await.unwrap();
        assert!(git(repo, &["add", "seed"]).await);
        assert!(git(repo, &["commit", "-m", "seed"]).await);
    }

    async fn git(repo: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .await
            .unwrap()
            .success()
    }

    #[tokio::test]
    async fn provisions_isolated_siblings_and_lists_then_removes_them() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        seed_repo(&repo).await;
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
        seed_repo(&repo).await;
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
    async fn claimed_placements_outlive_shutdown_and_the_next_startup_sweep() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        seed_repo(&repo).await;
        let config = WorktreeConfig {
            root: temp.path().join("worktrees"),
        };
        let host = WorktreeHost::new(config.clone());
        let claimed_owner = Uuid::new_v4();
        let swept_owner = Uuid::new_v4();
        let claimed = host.provision(claimed_owner, 0, &repo).await.unwrap();
        let alongside = host.provision(claimed_owner, 1, &repo).await.unwrap();
        let swept = host.provision(swept_owner, 0, &repo).await.unwrap();

        let terminal_run = Uuid::new_v4();
        host.retain(terminal_run, &[claimed.to_string_lossy().into_owned()])
            .await;
        host.shutdown().await;

        assert!(
            claimed.exists(),
            "shutdown must keep a placement a terminal account claimed"
        );
        assert!(
            !alongside.exists() && !swept.exists(),
            "shutdown must still reclaim every unclaimed placement"
        );

        let restarted = WorktreeHost::new(config);
        assert_eq!(
            restarted.recover(&HashSet::new()).await.unwrap(),
            0,
            "the next startup's sweep must reclaim nothing that was claimed"
        );
        assert!(claimed.exists());
        restarted
            .restore(claimed_owner, 0, &repo, &claimed)
            .await
            .unwrap();
        restarted.cleanup_owner(claimed_owner).await;
        assert!(
            claimed.exists(),
            "a claim read back from disk governs the restarted gateway's cleanup too"
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

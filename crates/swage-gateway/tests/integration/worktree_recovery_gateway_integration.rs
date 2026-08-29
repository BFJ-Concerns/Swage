#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use swage_gateway::worktree::{WorktreeConfig, WorktreeHost};
use tokio::process::Command;
use uuid::Uuid;

async fn git(repo: &std::path::Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .await
            .unwrap()
            .success(),
        "git {args:?}"
    );
}

#[tokio::test]
async fn recovery_continues_past_an_uninspectable_candidate_and_reclaims_the_later_registered_worktree()
 {
    let fixture = tempfile::tempdir().unwrap();
    let repo = fixture.path().join("repo");
    tokio::fs::create_dir(&repo).await.unwrap();
    git(&repo, &["init"]).await;
    git(&repo, &["config", "user.email", "test@example.invalid"]).await;
    git(&repo, &["config", "user.name", "Test"]).await;
    tokio::fs::write(repo.join("seed"), "seed").await.unwrap();
    git(&repo, &["add", "seed"]).await;
    git(&repo, &["commit", "-m", "seed"]).await;

    let root = fixture.path().join("worktrees");
    let owner = Uuid::new_v4();
    let invalid = root.join(owner.to_string()).join("worker-0");
    tokio::fs::create_dir_all(&invalid).await.unwrap();
    tokio::fs::write(invalid.join(".git"), "gitdir: /missing")
        .await
        .unwrap();
    let valid = WorktreeHost::new(WorktreeConfig { root: root.clone() })
        .provision(owner, 1, &repo)
        .await
        .unwrap();
    assert!(valid.exists());

    let recovered = WorktreeHost::new(WorktreeConfig { root: root.clone() })
        .recover(&HashSet::new())
        .await
        .unwrap();
    assert_eq!(recovered, 1);
    assert!(
        invalid.exists(),
        "the failed candidate is deliberately left untouched"
    );
    assert!(
        !valid.exists(),
        "the later valid candidate must still be reclaimed"
    );
    let registrations = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&registrations.stdout)
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "recovery removes the valid checkout registration too"
    );
}

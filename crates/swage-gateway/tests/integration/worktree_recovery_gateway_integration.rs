#![allow(clippy::unwrap_used)]

use std::{collections::HashSet, os::unix::fs::PermissionsExt};

use swage_gateway::worktree::{WorktreeConfig, WorktreeHost};
use tokio::process::Command;
use uuid::Uuid;

use crate::support::ProcessEnvGuard;

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
    let host = WorktreeHost::new(WorktreeConfig { root: root.clone() });
    let first = host.provision(owner, 0, &repo).await.unwrap();
    let second = host.provision(owner, 1, &repo).await.unwrap();
    let owner_path = root.join(owner.to_string());
    // Probe this filesystem's enumeration before assigning the candidates:
    // recovery deliberately does not sort `read_dir`, so the first observed
    // valid checkout is corrupted into the uninspectable candidate.
    let mut entries = tokio::fs::read_dir(&owner_path).await.unwrap();
    let first_name = entries.next_entry().await.unwrap().unwrap().file_name();
    let _second_name = entries.next_entry().await.unwrap().unwrap().file_name();
    assert!(entries.next_entry().await.unwrap().is_none());
    let invalid = owner_path.join(&first_name);
    let valid = if invalid == first { second } else { first };
    tokio::fs::write(invalid.join(".git"), "gitdir: /missing")
        .await
        .unwrap();
    assert!(valid.exists());

    // `read_dir` deliberately promises no entry order. The wrapper records the
    // actual `git -C <candidate> worktree list` order and the assertion below
    // admits this fixture only when the bad candidate was observed first.
    let wrapper_dir = fixture.path().join("git-wrapper");
    tokio::fs::create_dir(&wrapper_dir).await.unwrap();
    let command_log = fixture.path().join("recovery-git-order.log");
    let wrapper = wrapper_dir.join("git");
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .await
        .unwrap();
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    let real_git = real_git.trim().replace('"', "\\\"");
    tokio::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n# Recovery invokes git as -C <candidate>; argv[2] is that inspected path.\nprintf '%s\\n' \"$2\" >> \"$SWAGE_RECOVERY_GIT_LOG\"\nexec \"{real_git}\" \"$@\"\n"
        ),
    )
    .await
    .unwrap();
    tokio::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
        .await
        .unwrap();
    let mut environment = ProcessEnvGuard::new();
    environment.set_var("SWAGE_RECOVERY_GIT_LOG", &command_log);
    environment.set_var(
        "PATH",
        format!(
            "{}:{}",
            wrapper_dir.display(),
            std::env::var("PATH").unwrap()
        ),
    );
    let recovered = WorktreeHost::new(WorktreeConfig { root: root.clone() })
        .recover(&HashSet::new())
        .await
        .unwrap();
    let inspection_order = tokio::fs::read_to_string(&command_log).await.unwrap();
    assert_eq!(
        inspection_order.lines().next(),
        Some(invalid.to_string_lossy().as_ref()),
        "the fixture must observe the uninspectable candidate before the valid one"
    );
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
        2,
        "recovery removes the later valid checkout registration while leaving the failed candidate untouched"
    );
}

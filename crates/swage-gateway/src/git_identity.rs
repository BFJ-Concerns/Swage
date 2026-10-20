//! The operator's git author identity, carried to sandboxed git.
//!
//! A sandboxed command's git runs with no inherited configuration, so a
//! commit made inside would have no author unless one is supplied. The
//! author of such a commit is the operator the session acts for: their
//! `user.name` and `user.email`, read on the host as the turn's workspace
//! sees them — conditional includes and all — and appended into the
//! sandboxed git's configuration beside the credential helper's and the
//! signer's entries. An operator with no identity configured is given none:
//! sandboxed git then refuses to commit with its own "please tell me who you
//! are", and the agent's standing text has it ask the person rather than
//! invent an author.

use std::path::Path;

use crate::git_credential::append_git_config;
use crate::git_signer::host_git_config;

/// The operator's git author identity as the workspace's git sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitIdentity {
    pub name: String,
    pub email: String,
}

impl CommitIdentity {
    /// Read `user.name` and `user.email` through the host's git for the
    /// workspace. `None` when either is unset: half an identity is not one
    /// git would commit under, and supplying half would invite the agent
    /// to invent the rest.
    pub async fn from_host_git(workspace: &Path) -> Option<Self> {
        let name = host_git_config(workspace, &["--get", "user.name"]).await;
        let email = host_git_config(workspace, &["--get", "user.email"]).await;
        match (name, email) {
            (Some(name), Some(email)) => Some(Self { name, email }),
            _ => {
                tracing::info!(
                    workspace = %workspace.display(),
                    "the host's git has no author identity for this workspace; sandboxed commits will ask for one"
                );
                None
            }
        }
    }

    /// Give the sandboxed git this identity, through the same configuration
    /// entries the credential helper and the signer append theirs by.
    pub fn configure(&self, config: &mut swage_sandbox::SandboxConfig) {
        append_git_config(
            config,
            &[
                ("user.name", self.name.clone()),
                ("user.email", self.email.clone()),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let done = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    /// The identity is the workspace's own: a repository's local
    /// configuration is what the host's git answers with for it.
    #[tokio::test]
    async fn the_workspaces_identity_is_read_and_appended_as_git_configuration() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Operator Example"]);
        git(
            repo.path(),
            &["config", "user.email", "operator@example.invalid"],
        );

        let identity = CommitIdentity::from_host_git(repo.path())
            .await
            .expect("a configured identity is read");
        assert_eq!(
            identity,
            CommitIdentity {
                name: "Operator Example".into(),
                email: "operator@example.invalid".into(),
            }
        );

        let mut policy = swage_sandbox::SandboxConfig::builder()
            .working_directory(repo.path())
            .build()
            .unwrap();
        identity.configure(&mut policy);
        let entries: Vec<(String, String)> = (0..2)
            .map(|index| {
                (
                    policy.environment[&format!("GIT_CONFIG_KEY_{index}")].clone(),
                    policy.environment[&format!("GIT_CONFIG_VALUE_{index}")].clone(),
                )
            })
            .collect();
        assert_eq!(
            entries,
            vec![
                ("user.name".to_string(), "Operator Example".to_string()),
                (
                    "user.email".to_string(),
                    "operator@example.invalid".to_string()
                ),
            ]
        );
        assert_eq!(policy.environment["GIT_CONFIG_COUNT"], "2");
    }

    /// Both halves or neither: a name without an email is no identity to
    /// commit under.
    #[tokio::test]
    async fn half_an_identity_is_none() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Operator Example"]);
        // The email, from wherever the host's configuration would otherwise
        // supply it, is emptied for this repository.
        git(repo.path(), &["config", "user.email", ""]);

        assert_eq!(CommitIdentity::from_host_git(repo.path()).await, None);
    }
}

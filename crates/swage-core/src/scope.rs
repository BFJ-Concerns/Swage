//! Where a session's scope keeps its files.
//!
//! Every session belongs to exactly one scope: a **topic** the user chose, or
//! the **general scope** — the scope of a session started without a topic.
//! Both are rows in the same table and both own memory, a knowledge base,
//! materials and agent overlays, so storage never has to special-case an
//! absent topic. What differs is where those things live on disk and which
//! configuration layers apply, and that difference is this type's whole job:
//! callers that touch the filesystem or the configuration cascade ask the
//! scope for its location instead of building paths from a slug.
//!
//! The general scope has no scope-level configuration layer: its cascade is
//! Global → Agent. A topic's cascade is Global → Topic → Agent.

use std::path::{Path, PathBuf};

/// The on-disk home of one scope under the Swage root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScopeLocation {
    /// The general scope: `<root>/general/`.
    General,
    /// A topic, by its filesystem-safe slug: `<root>/topics/<slug>/`.
    Topic(String),
}

impl ScopeLocation {
    pub fn topic(slug: impl Into<String>) -> Self {
        Self::Topic(slug.into())
    }

    pub fn is_general(&self) -> bool {
        matches!(self, Self::General)
    }

    /// The scope's data directory: memory, knowledge bodies, materials and
    /// agent overlays all live beneath it.
    pub fn data_dir(&self, root: &Path) -> PathBuf {
        match self {
            Self::General => root.join("general"),
            Self::Topic(slug) => root.join("topics").join(slug),
        }
    }

    /// The directory an agent's scope-local overlay lives in: its own
    /// `config.toml`, skills and elements beneath this scope.
    pub fn agent_dir(&self, root: &Path, agent_id: &str) -> PathBuf {
        self.data_dir(root).join("agents").join(agent_id)
    }

    /// The scope's managed scratch root, separate from durable scope data.
    pub fn scratch_dir(&self, root: &Path) -> PathBuf {
        let scratch = root.join("scratch");
        match self {
            Self::General => scratch.join("general"),
            Self::Topic(slug) => scratch.join("topics").join(slug),
        }
    }

    /// The name the scope is described by to a reader: a topic's slug, or
    /// `general`.
    pub fn name(&self) -> &str {
        match self {
            Self::General => "general",
            Self::Topic(slug) => slug,
        }
    }
}

impl std::fmt::Display for ScopeLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_and_topic_scopes_live_in_different_trees() {
        let root = Path::new("/srv/swage");
        assert_eq!(
            ScopeLocation::General.data_dir(root),
            PathBuf::from("/srv/swage/general")
        );
        assert_eq!(
            ScopeLocation::topic("alloys").data_dir(root),
            PathBuf::from("/srv/swage/topics/alloys")
        );
        assert_eq!(
            ScopeLocation::General.agent_dir(root, "assistant"),
            PathBuf::from("/srv/swage/general/agents/assistant")
        );
    }

    #[test]
    fn the_general_scope_names_itself() {
        assert_eq!(ScopeLocation::General.to_string(), "general");
        assert_eq!(ScopeLocation::topic("alloys").to_string(), "alloys");
        assert!(ScopeLocation::General.is_general());
        assert!(!ScopeLocation::topic("alloys").is_general());
    }

    #[test]
    fn scratch_roots_follow_scope_without_entering_durable_data() {
        let root = Path::new("/srv/swage");
        let general = ScopeLocation::General;
        let topic = ScopeLocation::topic("alloys");
        assert_eq!(general.scratch_dir(root), root.join("scratch/general"));
        assert_eq!(topic.scratch_dir(root), root.join("scratch/topics/alloys"));
        assert_ne!(general.scratch_dir(root), topic.scratch_dir(root));
        assert!(
            !general
                .scratch_dir(root)
                .starts_with(general.data_dir(root))
        );
        assert!(!topic.scratch_dir(root).starts_with(topic.data_dir(root)));
    }
}

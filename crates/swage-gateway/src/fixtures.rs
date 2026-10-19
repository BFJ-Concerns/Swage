//! Development-only gateway fixtures with owned filesystem state.

use std::sync::Arc;

use swage_config::ResolvedConfig;
use swage_hook::HookRegistry;
use swage_provider::LlmProvider;
use swage_storage::{Storage, TopicStore};

use crate::{state::AppState, tool::ToolDispatcher};

impl AppState {
    /// Retain an isolated root until the last clone of this state is dropped.
    pub fn for_test(
        storage: Arc<dyn Storage>,
        topic_store: Arc<dyn TopicStore>,
        provider: Arc<dyn LlmProvider>,
        config: ResolvedConfig,
        tool_dispatcher: ToolDispatcher,
        hook_registry: Option<Arc<HookRegistry>>,
    ) -> Self {
        let root = Arc::new(tempfile::tempdir().expect("temporary gateway root"));
        let mut state = Self::new_with_config_root(
            storage,
            topic_store,
            provider,
            config,
            tool_dispatcher,
            hook_registry,
            root.path().to_path_buf(),
        );
        state.test_root = Some(root);
        state
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use swage_core::ScopeLocation;

    async fn exercise_owned_root(state: crate::state::AppState) {
        let root = state.config_root.clone();
        assert!(
            root.is_dir(),
            "fixture root must exist before its first write"
        );
        let clone = state.clone();
        drop(state);
        assert!(root.is_dir(), "a state clone must retain its root");
        let reference = crate::memory::MemoryReference::parse("general/isolation").unwrap();
        crate::memory::write_entry(
            &clone.config_root,
            &ScopeLocation::General,
            &reference,
            "Fixture memory",
            "only in the retained root",
        )
        .await
        .unwrap();
        assert!(root.join("general/memory/isolation.md").is_file());
        assert!(
            crate::memory::read_entry(&clone.config_root, &ScopeLocation::General, &reference,)
                .await
                .unwrap()
                .is_some()
        );
        assert_ne!(root, swage_config::discovery::effective_root_path());
        drop(clone);
        assert!(
            !root.exists(),
            "last state clone must release its temporary root"
        );
    }

    #[tokio::test]
    async fn default_gateway_fixture_owns_memory_until_last_clone() {
        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().unwrap());
        exercise_owned_root(crate::test_support::state(storage)).await;
    }

    #[tokio::test]
    async fn default_turn_fixture_owns_memory_until_last_clone() {
        let (state, _) = crate::turn::tests::make_state_with_session(vec![]).await;
        exercise_owned_root(state).await;
    }
}

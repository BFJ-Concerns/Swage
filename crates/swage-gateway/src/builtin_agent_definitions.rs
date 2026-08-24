//! Binary-versioned agent definitions available without authored rows.

use std::sync::LazyLock;

use swage_storage::NewAgentDefinition;

pub(crate) struct BuiltinAgentDefinition {
    pub(crate) id: &'static str,
    pub(crate) source: &'static str,
}

pub(crate) const BUILTIN_AGENT_DEFINITIONS: &[BuiltinAgentDefinition] = &[
    BuiltinAgentDefinition {
        id: "organiser",
        source: include_str!("../../../shipped/agents/organiser.json"),
    },
    BuiltinAgentDefinition {
        id: "researcher",
        source: include_str!("../../../shipped/agents/researcher.json"),
    },
    BuiltinAgentDefinition {
        id: "synthesist",
        source: include_str!("../../../shipped/agents/synthesist.json"),
    },
];

static DEFINITIONS: LazyLock<Vec<NewAgentDefinition>> = LazyLock::new(|| {
    BUILTIN_AGENT_DEFINITIONS
        .iter()
        .map(|entry| {
            let definition: NewAgentDefinition = serde_json::from_str(entry.source)
                .expect("shipped agent definition must be valid JSON");
            assert_eq!(definition.id, entry.id, "shipped agent definition id");
            assert!(
                definition.space_id.is_none(),
                "shipped agent definitions cannot carry a storage scope"
            );
            definition
        })
        .collect()
});

pub(crate) fn all() -> impl Iterator<Item = &'static NewAgentDefinition> {
    DEFINITIONS.iter()
}

pub(crate) fn find_by_name(name: &str) -> Option<&'static NewAgentDefinition> {
    DEFINITIONS.iter().find(|definition| definition.id == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_embeds_the_shipped_definitions_byte_for_byte() {
        let expected = [
            (
                "organiser",
                include_str!("../../../shipped/agents/organiser.json"),
            ),
            (
                "researcher",
                include_str!("../../../shipped/agents/researcher.json"),
            ),
            (
                "synthesist",
                include_str!("../../../shipped/agents/synthesist.json"),
            ),
        ];

        assert_eq!(BUILTIN_AGENT_DEFINITIONS.len(), expected.len());
        for (id, source) in expected {
            let entry = BUILTIN_AGENT_DEFINITIONS
                .iter()
                .find(|entry| entry.id == id)
                .expect("catalogue entry");
            assert_eq!(entry.source.as_bytes(), source.as_bytes());
        }
    }

    #[test]
    fn definitions_have_exact_least_privilege_grants() {
        let grants = all()
            .map(|definition| {
                let permissions: swage_config::model::ToolPermissionConfig =
                    serde_json::from_value(
                        definition
                            .tool_permissions
                            .clone()
                            .expect("shipped definition permissions"),
                    )
                    .expect("valid shipped permissions");
                (definition.id.as_str(), permissions.allow)
            })
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(
            grants["researcher"],
            [
                "web_search",
                "url_fetch",
                "browser_navigate",
                "browser_snapshot",
                "browser_click",
                "browser_type",
                "browser_select",
                "browser_scroll",
                "knowledge_record",
            ]
        );
        assert_eq!(
            grants["organiser"],
            [
                "knowledge_search",
                "knowledge_curate",
                "knowledge_curate_orientation",
            ]
        );
        assert_eq!(grants["synthesist"], ["knowledge_search", "knowledge_cite"]);
        assert!(grants.values().flatten().all(|tool| {
            !matches!(tool.as_str(), "config_apply" | "schedule" | "wake_me_later")
        }));
    }
}

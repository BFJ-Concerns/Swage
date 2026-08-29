//! Binary-versioned workflow scripts available without authored rows.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::synthetic_id::synthetic_id;

const SYNTHETIC_ID_DOMAIN: &[u8] = b"swage builtin workflow";

pub(crate) struct BuiltinWorkflow {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) source: &'static str,
    pub(crate) captured_sources_grant: bool,
    pub(crate) released_at: &'static str,
}

pub(crate) const BUILTIN_WORKFLOWS: &[BuiltinWorkflow] = &[BuiltinWorkflow {
    name: "research-sweep",
    description: "Gather, organise, and synthesise live web research into the space knowledge base.",
    source: include_str!("../../../examples/workflows/research-sweep.js"),
    captured_sources_grant: true,
    released_at: "2026-08-11T17:56:09+01:00",
}];

pub(crate) fn find_by_name(name: &str) -> Option<&'static BuiltinWorkflow> {
    BUILTIN_WORKFLOWS
        .iter()
        .find(|workflow| workflow.name == name)
}

pub(crate) fn find_by_id(space_id: Uuid, id: Uuid) -> Option<&'static BuiltinWorkflow> {
    BUILTIN_WORKFLOWS
        .iter()
        .find(|workflow| self::id(space_id, workflow.name) == id)
}

pub(crate) fn id(space_id: Uuid, name: &str) -> Uuid {
    synthetic_id(SYNTHETIC_ID_DOMAIN, space_id, name)
}

pub(crate) fn released_at(workflow: &BuiltinWorkflow) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(workflow.released_at)
        .expect("built-in workflow release timestamp")
        .with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_embeds_the_authored_scripts_byte_for_byte() {
        let expected = [(
            "research-sweep",
            include_str!("../../../examples/workflows/research-sweep.js"),
        )];

        assert_eq!(BUILTIN_WORKFLOWS.len(), expected.len());
        for (name, source) in expected {
            let workflow = find_by_name(name).expect("catalogue entry");
            assert_eq!(workflow.source.as_bytes(), source.as_bytes());
        }
    }

    #[test]
    fn identifiers_are_stable_per_space_and_distinct_between_spaces() {
        let first_space = Uuid::from_u128(1);
        let second_space = Uuid::from_u128(2);
        assert_eq!(
            id(first_space, "research-sweep"),
            id(first_space, "research-sweep")
        );
        assert_ne!(
            id(first_space, "research-sweep"),
            id(second_space, "research-sweep")
        );
    }
}

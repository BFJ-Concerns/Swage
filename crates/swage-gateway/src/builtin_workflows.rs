//! Binary-versioned workflow scripts available without authored rows.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) struct BuiltinWorkflow {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) source: &'static str,
    pub(crate) released_at: &'static str,
}

pub(crate) const BUILTIN_WORKFLOWS: &[BuiltinWorkflow] = &[
    BuiltinWorkflow {
        name: "front-loaded-run",
        description: "Coordinate a long unattended run through fresh lead segments, worker-verifier pipelines, wake gates, and a closing handover.",
        source: include_str!("../../../examples/workflows/front-loaded-run.js"),
        released_at: "2026-08-12T00:51:42+01:00",
    },
    BuiltinWorkflow {
        name: "research-sweep",
        description: "Gather, organise, and synthesise live web research into the space knowledge base.",
        source: include_str!("../../../examples/workflows/research-sweep.js"),
        released_at: "2026-08-11T17:56:09+01:00",
    },
    BuiltinWorkflow {
        name: "segment-orchestration",
        description: "Run long work as fresh-context lead segments separated by durable completion or timer wakes.",
        source: include_str!("../../../examples/workflows/segment-orchestration.js"),
        released_at: "2026-08-10T17:54:28+01:00",
    },
];

pub(crate) fn find_by_name(name: &str) -> Option<&'static BuiltinWorkflow> {
    BUILTIN_WORKFLOWS
        .iter()
        .find(|workflow| workflow.name == name)
}

pub(crate) fn id(space_id: Uuid, name: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"swage builtin workflow\0");
    digest.update(space_id.as_bytes());
    digest.update([0]);
    digest.update(name.as_bytes());
    let mut bytes: [u8; 16] = digest.finalize()[..16]
        .try_into()
        .expect("fixed digest slice");
    // Mark this deterministic identifier as an RFC 9562 name-based UUID.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
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
        let expected = [
            (
                "front-loaded-run",
                include_str!("../../../examples/workflows/front-loaded-run.js"),
            ),
            (
                "research-sweep",
                include_str!("../../../examples/workflows/research-sweep.js"),
            ),
            (
                "segment-orchestration",
                include_str!("../../../examples/workflows/segment-orchestration.js"),
            ),
        ];

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

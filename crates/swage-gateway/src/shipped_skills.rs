//! Binary-versioned skills available without operator-authored files.

use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) struct ShippedFile {
    pub(crate) path: &'static str,
    pub(crate) contents: &'static str,
}

pub(crate) struct ShippedSkill {
    pub(crate) name: &'static str,
    pub(crate) source: &'static str,
    pub(crate) files: &'static [ShippedFile],
}

macro_rules! shipped_file {
    ($path:literal) => {
        ShippedFile {
            path: $path,
            contents: include_str!(concat!("../../../shipped/skills/", $path)),
        }
    };
}

const ASSEMBLY_FILES: &[ShippedFile] = &[
    shipped_file!("assembly/SKILL.md"),
    shipped_file!("assembly/references/briefs.md"),
    shipped_file!("assembly/references/build-ledger.md"),
    shipped_file!("assembly/references/effort-calibration.md"),
    shipped_file!("assembly/references/landing.md"),
    shipped_file!("assembly/references/ledger.md"),
    shipped_file!("assembly/references/model-selection.md"),
    shipped_file!("assembly/references/pipelines.md"),
    shipped_file!("assembly/workflows/build-verify.js"),
    shipped_file!("assembly/workflows/docs-update.js"),
    shipped_file!("assembly/workflows/estate-unit.js"),
    shipped_file!("assembly/workflows/triage-wave.js"),
];

const COMMISSION_FILES: &[ShippedFile] = &[
    shipped_file!("commission/SKILL.md"),
    shipped_file!("commission/references/alignment-handshake.md"),
    shipped_file!("commission/references/commitment-register.md"),
    shipped_file!("commission/references/dispatched-agents.md"),
    shipped_file!("commission/references/effort-calibration.md"),
    shipped_file!("commission/references/elicitation-moves.md"),
    shipped_file!("commission/references/ensemble-workflows.md"),
    shipped_file!("commission/references/friction-signals.md"),
    shipped_file!("commission/references/held-intent.md"),
    shipped_file!("commission/references/misconception-moves.md"),
    shipped_file!("commission/references/model-selection.md"),
    shipped_file!("commission/references/shedding-workflow.md"),
    shipped_file!("commission/references/signoff-page.md"),
    shipped_file!("commission/scripts/create-annexe.sh"),
    shipped_file!("commission/workflows/blind-rederivation.js"),
    shipped_file!("commission/workflows/commitment-audit.js"),
    shipped_file!("commission/workflows/divergent-survey.js"),
    shipped_file!("commission/workflows/draft-review.js"),
    shipped_file!("commission/workflows/grounding.js"),
    shipped_file!("commission/workflows/preservation.js"),
];

const MAESTRO_FILES: &[ShippedFile] = &[
    shipped_file!("maestro/SKILL.md"),
    shipped_file!("maestro/references/build-ledger.md"),
    shipped_file!("maestro/references/effort-calibration.md"),
    shipped_file!("maestro/references/model-selection.md"),
    shipped_file!("maestro/references/spine-authoring.md"),
    shipped_file!("maestro/references/teardown.md"),
    shipped_file!("maestro/workflows/teardown-audit.js"),
];

const PROJECT_DOCS_FILES: &[ShippedFile] = &[
    shipped_file!("project-docs/SKILL.md"),
    shipped_file!("project-docs/prompts/accuracy-review.md"),
    shipped_file!("project-docs/prompts/survey-worker.md"),
    shipped_file!("project-docs/references/document-shapes.md"),
    shipped_file!("project-docs/references/human-register.md"),
    shipped_file!("project-docs/references/parallel-sets.md"),
    shipped_file!("project-docs/references/survey-protocol.md"),
    shipped_file!("project-docs/references/workflow-refresh.md"),
    shipped_file!("project-docs/references/workflow-write.md"),
    shipped_file!("project-docs/scripts/check-doc-links.sh"),
    shipped_file!("project-docs/scripts/doc-indexes.sh"),
];

const WORKFLOW_AUTHORING_FILES: &[ShippedFile] = &[shipped_file!("workflow-authoring/SKILL.md")];

pub(crate) const SHIPPED_SKILLS: &[ShippedSkill] = &[
    ShippedSkill {
        name: "assembly",
        source: include_str!("../../../shipped/skills/assembly/SKILL.md"),
        files: ASSEMBLY_FILES,
    },
    ShippedSkill {
        name: "commission",
        source: include_str!("../../../shipped/skills/commission/SKILL.md"),
        files: COMMISSION_FILES,
    },
    ShippedSkill {
        name: "maestro",
        source: include_str!("../../../shipped/skills/maestro/SKILL.md"),
        files: MAESTRO_FILES,
    },
    ShippedSkill {
        name: "project-docs",
        source: include_str!("../../../shipped/skills/project-docs/SKILL.md"),
        files: PROJECT_DOCS_FILES,
    },
    ShippedSkill {
        name: "workflow-authoring",
        source: include_str!("../../../shipped/skills/workflow-authoring/SKILL.md"),
        files: WORKFLOW_AUTHORING_FILES,
    },
];

pub(crate) fn find_by_name(name: &str) -> Option<&'static ShippedSkill> {
    SHIPPED_SKILLS.iter().find(|skill| skill.name == name)
}

pub(crate) fn id(space_id: Uuid, name: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"swage shipped skill\0");
    digest.update(space_id.as_bytes());
    digest.update([0]);
    digest.update(name.as_bytes());
    let mut bytes: [u8; 16] = digest.finalize()[..16]
        .try_into()
        .expect("fixed digest slice");
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_embeds_every_shipped_file_byte_for_byte() {
        let repo_files = [
            include_str!("../../../shipped/skills/assembly/SKILL.md"),
            include_str!("../../../shipped/skills/commission/SKILL.md"),
            include_str!("../../../shipped/skills/maestro/SKILL.md"),
            include_str!("../../../shipped/skills/project-docs/SKILL.md"),
            include_str!("../../../shipped/skills/workflow-authoring/SKILL.md"),
        ];
        assert_eq!(SHIPPED_SKILLS.len(), repo_files.len());
        assert_eq!(
            SHIPPED_SKILLS
                .iter()
                .map(|skill| skill.files.len())
                .sum::<usize>(),
            51
        );
        for (skill, repo_source) in SHIPPED_SKILLS.iter().zip(repo_files) {
            assert_eq!(skill.source.as_bytes(), repo_source.as_bytes());
            let embedded_skill = skill
                .files
                .iter()
                .find(|file| file.path == format!("{}/SKILL.md", skill.name))
                .expect("embedded SKILL.md");
            assert_eq!(embedded_skill.contents.as_bytes(), repo_source.as_bytes());
        }
    }

    #[test]
    fn identifiers_are_stable_and_domain_separated() {
        let space = Uuid::from_u128(1);
        assert_eq!(id(space, "assembly"), id(space, "assembly"));
        assert_ne!(id(space, "assembly"), id(Uuid::from_u128(2), "assembly"));
        assert_ne!(
            id(space, "assembly"),
            crate::builtin_workflows::id(space, "assembly")
        );
    }
}

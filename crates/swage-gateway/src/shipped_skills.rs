//! Binary-versioned skills available without operator-authored files.

use uuid::Uuid;

use crate::synthetic_id::synthetic_id;

const SYNTHETIC_ID_DOMAIN: &[u8] = b"swage shipped skill";

pub(crate) struct ShippedFile {
    pub(crate) path: &'static str,
    pub(crate) contents: &'static str,
    pub(crate) captured_sources_grant: bool,
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
            captured_sources_grant: false,
        }
    };
}

macro_rules! shipped_workflow_file {
    ($path:literal, $captured_sources_grant:expr) => {
        ShippedFile {
            path: $path,
            contents: include_str!(concat!("../../../shipped/skills/", $path)),
            captured_sources_grant: $captured_sources_grant,
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

const RESEARCH_FILES: &[ShippedFile] = &[
    shipped_file!("research/SKILL.md"),
    shipped_workflow_file!("research/workflows/research-sweep.js", true),
];

const SHAKEDOWN_FILES: &[ShippedFile] = &[
    shipped_file!("shakedown/SKILL.md"),
    shipped_file!("shakedown/references/build-ledger.md"),
    shipped_file!("shakedown/references/campaign.md"),
    shipped_file!("shakedown/references/charters.md"),
    shipped_file!("shakedown/references/effort-calibration.md"),
    shipped_file!("shakedown/references/findings.md"),
    shipped_file!("shakedown/references/instruments.md"),
    shipped_file!("shakedown/references/model-selection.md"),
    shipped_file!("shakedown/references/state.md"),
    shipped_file!("shakedown/workflows/driver-wave.js"),
    shipped_file!("shakedown/workflows/judgement-wave.js"),
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
        name: "research",
        source: include_str!("../../../shipped/skills/research/SKILL.md"),
        files: RESEARCH_FILES,
    },
    ShippedSkill {
        name: "shakedown",
        source: include_str!("../../../shipped/skills/shakedown/SKILL.md"),
        files: SHAKEDOWN_FILES,
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

pub(crate) fn workflow_files(skill_name: &str) -> impl Iterator<Item = &'static ShippedFile> {
    find_by_name(skill_name)
        .into_iter()
        .flat_map(|skill| skill.files.iter())
        .filter(move |file| {
            file.path.strip_prefix(skill_name).is_some_and(|path| {
                path.starts_with("/workflows/")
                    && std::path::Path::new(path)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
            })
        })
}

pub(crate) fn id(topic_id: Uuid, name: &str) -> Uuid {
    synthetic_id(SYNTHETIC_ID_DOMAIN, topic_id, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, path::Path};

    fn repository_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(directory).expect("read shipped skills directory") {
                let entry = entry.expect("read shipped skill entry");
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .expect("shipped file beneath catalogue root")
                        .to_str()
                        .expect("UTF-8 shipped file path")
                        .replace('\\', "/");
                    let previous =
                        files.insert(relative, fs::read(&path).expect("read shipped skill file"));
                    assert!(previous.is_none(), "duplicate shipped file path");
                }
            }
        }

        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    #[test]
    fn catalogue_embeds_every_shipped_file_byte_for_byte() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shipped/skills");
        let repo_files = repository_files(&root);
        let mut embedded_files = BTreeMap::new();
        for file in SHIPPED_SKILLS.iter().flat_map(|skill| skill.files.iter()) {
            let previous = embedded_files.insert(file.path.to_owned(), file.contents.as_bytes());
            assert!(previous.is_none(), "duplicate embedded file path");
        }

        assert_eq!(
            embedded_files.keys().collect::<Vec<_>>(),
            repo_files.keys().collect::<Vec<_>>()
        );
        for (path, repo_bytes) in &repo_files {
            assert_eq!(
                embedded_files.get(path).expect("repository file embedded"),
                &repo_bytes.as_slice(),
                "embedded bytes differ for {path}"
            );
        }

        for skill in SHIPPED_SKILLS {
            let embedded_skill = skill
                .files
                .iter()
                .find(|file| file.path == format!("{}/SKILL.md", skill.name))
                .expect("embedded SKILL.md");
            assert_eq!(embedded_skill.contents, skill.source);
        }
    }

    #[test]
    fn identifiers_are_stable_and_domain_separated() {
        let topic = Uuid::from_u128(1);
        assert_eq!(id(topic, "assembly"), id(topic, "assembly"));
        assert_ne!(id(topic, "assembly"), id(Uuid::from_u128(2), "assembly"));
        assert_ne!(id(topic, "assembly"), id(topic, "research"));
    }

    #[test]
    fn workflow_catalogue_returns_embedded_auxiliary_sources() {
        let workflows = workflow_files("assembly").collect::<Vec<_>>();
        assert!(
            workflows
                .iter()
                .any(|file| file.path == "assembly/workflows/build-verify.js")
        );
        let build_verify = workflows
            .iter()
            .find(|file| file.path.ends_with("/build-verify.js"))
            .expect("embedded build-verify workflow");
        assert_eq!(
            build_verify.contents.as_bytes(),
            include_bytes!("../../../shipped/skills/assembly/workflows/build-verify.js")
        );
        assert!(workflow_files("workflow-authoring").next().is_none());
    }
}

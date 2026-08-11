//! Knowledge-base browse mirror — a generated markdown projection of a
//! space's knowledge base for the human operator to read on disk.
//!
//! Authority lives in the database; the mirror is a disposable projection,
//! never the store. Deleting it is always safe, and regenerating from the
//! same rows produces byte-identical files: nothing here derives content
//! from the wall clock or any other non-database input. That idempotence is
//! the pattern's declared property — a projection whose bytes drift between
//! regenerations has smuggled state from outside the database.
//!
//! The mirror lives under the space data root at
//! `spaces/<slug>/.swage/knowledge/mirror/`, beside the body files, as an
//! `index.md` plus one page per observation. Regeneration wipes the
//! directory and rewrites it whole; a crash mid-write leaves a partial
//! mirror, and the next regeneration replaces it — disposability is the
//! recovery path, so no atomic-rename ceremony is warranted here.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path, State},
};
use swage_config::discovery::resolve_root;
use swage_core::GatewayError;
use swage_protocol::hosting::KnowledgeMirrorRegenerateResponse;
use swage_storage::{
    CurationAssertion, CurationAssertionKind, ExtractionDerivation, SourceObservation, Storage,
};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// Every generated file opens with this, so a reader landing on one knows
/// what it is and what it is not.
const GENERATED_NOTE: &str = "<!-- Generated from the knowledge base. \
Authority lives in the database — do not edit; deleting this directory is \
always safe, and regenerating produces it identically. -->";

/// `POST /api/spaces/{id}/knowledge-mirror` — regenerate the space's browse
/// mirror from the database.
pub async fn regenerate(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
) -> Result<Json<KnowledgeMirrorRegenerateResponse>, ApiError> {
    let space = state
        .space_store
        .get(space_id)
        .await
        .map_err(GatewayError::from)?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
    let response = regenerate_for_space(state.storage.as_ref(), space_id, &space.slug).await?;
    Ok(Json(response))
}

/// Rebuild the mirror directory whole from the space's rows.
pub async fn regenerate_for_space(
    storage: &dyn Storage,
    space_id: Uuid,
    space_slug: &str,
) -> Result<KnowledgeMirrorRegenerateResponse, GatewayError> {
    let mut observations = storage.list_observations_by_space(space_id).await?;
    // The store orders by retrieved_at alone, which leaves ties to the
    // engine; byte-identical regeneration needs a total order.
    observations.sort_by(|a, b| b.retrieved_at.cmp(&a.retrieved_at).then(a.id.cmp(&b.id)));

    let curation: BTreeMap<Uuid, CurationAssertion> = storage
        .effective_curation_by_space(space_id)
        .await?
        .into_iter()
        .map(|assertion| (assertion.observation_id, assertion))
        .collect();

    let mut pages = Vec::with_capacity(observations.len());
    for observation in observations {
        let latest = storage.latest_extraction(observation.id).await?;
        let effective = curation.get(&observation.id).cloned();
        pages.push(ObservationPage {
            observation,
            curation: effective,
            latest,
        });
    }

    let files = render_mirror(space_slug, &pages);
    let root = mirror_root(space_slug)?;
    let files_written = write_mirror(&root, &files).await?;

    Ok(KnowledgeMirrorRegenerateResponse {
        mirror_root: root.display().to_string(),
        files_written,
    })
}

/// One observation's mirror-facing state, joined from the three stores.
struct ObservationPage {
    observation: SourceObservation,
    curation: Option<CurationAssertion>,
    latest: Option<ExtractionDerivation>,
}

fn mirror_root(space_slug: &str) -> Result<PathBuf, GatewayError> {
    Ok(resolve_root()
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))?
        .join("spaces")
        .join(space_slug)
        .join(".swage")
        .join("knowledge")
        .join("mirror"))
}

/// Render the whole mirror as relative path → content. Pure: everything in
/// the output comes from the rows, so equal input renders equal bytes.
fn render_mirror(space_slug: &str, pages: &[ObservationPage]) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();

    let mut index = String::new();
    let _ = writeln!(index, "# Knowledge base — {space_slug}");
    let _ = writeln!(index);
    let _ = writeln!(index, "{GENERATED_NOTE}");
    let _ = writeln!(index);
    if pages.is_empty() {
        let _ = writeln!(index, "No observations have been captured yet.");
    } else {
        let _ = writeln!(index, "{} observations, newest first.", pages.len());
        let _ = writeln!(index);
        for page in pages {
            let _ = writeln!(
                index,
                "- [{}](observations/{}.md) — retrieved {}, {}, {}",
                origin_label(&page.observation),
                page.observation.id,
                page.observation.retrieved_at.to_rfc3339(),
                page.observation.media_type,
                curation_label(page.curation.as_ref(), "observations/"),
            );
        }
    }
    files.insert("index.md".to_string(), index);

    for page in pages {
        files.insert(
            format!("observations/{}.md", page.observation.id),
            render_observation(page),
        );
    }

    files
}

fn render_observation(page: &ObservationPage) -> String {
    let observation = &page.observation;
    let mut text = String::new();
    let _ = writeln!(text, "# {}", origin_label(observation));
    let _ = writeln!(text);
    let _ = writeln!(text, "{GENERATED_NOTE}");
    let _ = writeln!(text);
    let _ = writeln!(text, "- Observation: `{}`", observation.id);
    let _ = writeln!(
        text,
        "- Retrieved: {}",
        observation.retrieved_at.to_rfc3339()
    );
    let _ = writeln!(text, "- Media type: {}", observation.media_type);
    let _ = writeln!(
        text,
        "- Raw body: `{}` ({} bytes)",
        observation.body_sha256, observation.body_bytes
    );
    let _ = writeln!(
        text,
        "- Curation: {}",
        curation_label(page.curation.as_ref(), "")
    );
    let _ = writeln!(text);
    match &page.latest {
        Some(extraction) => {
            let _ = writeln!(
                text,
                "## Extracted text (version {}, {})",
                extraction.version, extraction.extractor
            );
            let _ = writeln!(text);
            let _ = writeln!(text, "{}", extraction.extracted_text);
        }
        None => {
            let _ = writeln!(text, "No text extraction has been derived yet.");
        }
    }
    text
}

fn origin_label(observation: &SourceObservation) -> String {
    observation
        .origin
        .url
        .clone()
        .or_else(|| observation.origin.local_path.clone())
        .unwrap_or_else(|| observation.id.to_string())
}

/// The observation's effective curation state, as the index and page show
/// it. Curated-out material stays in the mirror with its state named —
/// the mirror browses everything; default *retrieval* is where curation
/// hides things. `link_prefix` is the path from the rendering file to the
/// `observations/` directory: `"observations/"` from the index, `""` from
/// a page inside that directory.
fn curation_label(assertion: Option<&CurationAssertion>, link_prefix: &str) -> String {
    match assertion {
        None => "active".to_string(),
        Some(assertion) => match (assertion.kind, assertion.target_observation_id) {
            (CurationAssertionKind::Stale, _) => "stale".to_string(),
            (CurationAssertionKind::SupersededBy, Some(target)) => {
                format!("superseded by [{target}]({link_prefix}{target}.md)")
            }
            (CurationAssertionKind::DuplicateOf, Some(target)) => {
                format!("duplicate of [{target}]({link_prefix}{target}.md)")
            }
            // The store rejects targetless relationship assertions, so this
            // arm only guards a future storage defect.
            (kind, None) => kind.to_string(),
        },
    }
}

/// Wipe the mirror directory and write the rendered files.
async fn write_mirror(
    root: &std::path::Path,
    files: &BTreeMap<String, String>,
) -> Result<u32, GatewayError> {
    match tokio::fs::remove_dir_all(root).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(GatewayError::ValidationError(format!(
                "mirror directory could not be cleared: {error}"
            )));
        }
    }

    for (relative, content) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
    }

    u32::try_from(files.len())
        .map_err(|_| GatewayError::ValidationError("mirror file count overflows u32".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use swage_storage::{
        CurationAssertionStore, NewCurationAssertion, NewSourceObservation, NewSpace,
        ObservationOrigin, ObservationStore, SpaceStore, SqliteStorage,
    };

    use crate::test_support::SwageHomeGuard;

    // resolve_root reads the config hierarchy, so filesystem tests pin it to
    // a temp directory. SWAGE_HOME is process-wide state and cargo runs tests
    // on parallel threads, so the pin must be the crate's serialising guard —
    // never a bare set_var.
    fn pin_root() -> SwageHomeGuard {
        SwageHomeGuard::new()
    }

    fn fixed_time(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("fixture timestamp")
            .with_timezone(&Utc)
    }

    async fn seeded_storage() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage
            .create(NewSpace {
                name: "research".into(),
                slug: "research".into(),
            })
            .await
            .unwrap();

        let current = storage
            .record_observation(NewSourceObservation {
                space_id: space.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/kettles-2026".into()),
                    local_path: None,
                },
                retrieved_at: fixed_time("2026-08-02T10:00:00+00:00"),
                body_sha256: "aa11".into(),
                body_bytes: 64,
                media_type: "text/html".into(),
            })
            .await
            .unwrap();
        storage
            .add_extraction(current.id, "The 2026 guidance.".into(), "html-to-text v1".into())
            .await
            .unwrap();

        let outdated = storage
            .record_observation(NewSourceObservation {
                space_id: space.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/kettles-2019".into()),
                    local_path: None,
                },
                retrieved_at: fixed_time("2026-08-01T10:00:00+00:00"),
                body_sha256: "bb22".into(),
                body_bytes: 64,
                media_type: "text/html".into(),
            })
            .await
            .unwrap();
        storage
            .add_curation_assertion(NewCurationAssertion {
                space_id: space.id,
                observation_id: outdated.id,
                kind: CurationAssertionKind::SupersededBy,
                target_observation_id: Some(current.id),
            })
            .await
            .unwrap();

        (storage, space.id)
    }

    /// Read every file under the mirror back as relative path → bytes.
    fn read_tree(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(
            root: &std::path::Path,
            dir: &std::path::Path,
            into: &mut BTreeMap<String, Vec<u8>>,
        ) {
            for entry in std::fs::read_dir(dir).expect("mirror directory reads") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    walk(root, &path, into);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .expect("path under root")
                        .to_string_lossy()
                        .into_owned();
                    into.insert(relative, std::fs::read(&path).expect("mirror file reads"));
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(root, root, &mut files);
        files
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleted_mirror_regenerates_byte_identically() {
        let _root = pin_root();
        let (storage, space_id) = seeded_storage().await;

        let first = regenerate_for_space(&storage, space_id, "research")
            .await
            .unwrap();
        let root = PathBuf::from(&first.mirror_root);
        let first_tree = read_tree(&root);
        assert_eq!(first.files_written as usize, first_tree.len());

        std::fs::remove_dir_all(&root).expect("the mirror is disposable");

        let second = regenerate_for_space(&storage, space_id, "research")
            .await
            .unwrap();
        assert_eq!(second.mirror_root, first.mirror_root);
        assert_eq!(
            read_tree(&root),
            first_tree,
            "equal rows must render equal bytes — the mirror holds no state of its own"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn regeneration_replaces_the_directory_whole() {
        let _root = pin_root();
        let (storage, space_id) = seeded_storage().await;

        let report = regenerate_for_space(&storage, space_id, "research")
            .await
            .unwrap();
        let root = PathBuf::from(&report.mirror_root);

        // A file the database knows nothing about — a hand edit, or debris
        // from an observation since absorbed elsewhere.
        std::fs::write(root.join("stray.md"), "not from the database").unwrap();

        regenerate_for_space(&storage, space_id, "research")
            .await
            .unwrap();
        assert!(
            !root.join("stray.md").exists(),
            "the mirror is a projection: anything the rows do not produce goes"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mirror_names_curation_state_without_hiding_material() {
        let _root = pin_root();
        let (storage, space_id) = seeded_storage().await;

        let report = regenerate_for_space(&storage, space_id, "research")
            .await
            .unwrap();
        let root = PathBuf::from(&report.mirror_root);
        let index = std::fs::read_to_string(root.join("index.md")).unwrap();

        assert!(index.contains("kettles-2026"));
        assert!(
            index.contains("kettles-2019") && index.contains("superseded by"),
            "curated-out material stays browsable with its state named"
        );
        assert!(index.contains("active"));

        // Cross-references resolve from where they are written: the index
        // links into observations/, while a page beside its sibling must not
        // repeat the directory.
        let superseded_page = std::fs::read_dir(root.join("observations"))
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .find(|page| page.contains("superseded by"))
            .expect("the superseded observation has a page");
        assert!(
            !superseded_page.contains("(observations/"),
            "a page's sibling link must be relative to observations/, not to the mirror root"
        );
    }

    #[test]
    fn empty_space_renders_an_index_saying_so() {
        let files = render_mirror("research", &[]);
        assert_eq!(files.len(), 1);
        assert!(files["index.md"].contains("No observations have been captured yet."));
    }
}

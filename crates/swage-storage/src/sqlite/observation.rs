use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    ExtractionDerivation, KnowledgeSearchHit, NewSourceObservation, ObservationOrigin,
    SourceObservation,
};
use crate::sqlite::SqliteStorage;
use crate::store::ObservationStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

const OBSERVATION_COLUMNS: &str = "id, space_id, origin_url, origin_local_path, retrieved_at,
     body_sha256, body_bytes, media_type, created_at";

fn row_to_observation(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceObservation> {
    Ok(SourceObservation {
        id: parse_uuid(&row.get::<_, String>(0)?)?,
        space_id: parse_uuid(&row.get::<_, String>(1)?)?,
        origin: ObservationOrigin {
            url: row.get(2)?,
            local_path: row.get(3)?,
        },
        retrieved_at: parse_dt(&row.get::<_, String>(4)?)?,
        body_sha256: row.get(5)?,
        body_bytes: row.get::<_, i64>(6)?.try_into().unwrap_or(0),
        media_type: row.get(7)?,
        created_at: parse_dt(&row.get::<_, String>(8)?)?,
    })
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

// ---------------------------------------------------------------------------
// ObservationStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ObservationStore for SqliteStorage {
    async fn record_observation(
        &self,
        observation: NewSourceObservation,
    ) -> Result<SourceObservation, StorageError> {
        if observation.origin.url.is_none() && observation.origin.local_path.is_none() {
            return Err(StorageError::ConstraintViolation(
                "an observation's origin needs a URL or a local path".into(),
            ));
        }

        self.with_conn(move |conn| {
            // Identical bodies within a space deduplicate to the existing
            // observation so evidence identity never forks: two captures of
            // the same bytes are one piece of evidence, whatever fetched them.
            if let Some(existing) = conn
                .query_row(
                    &format!(
                        "SELECT {OBSERVATION_COLUMNS} FROM source_observations
                         WHERE space_id = ?1 AND body_sha256 = ?2"
                    ),
                    params![observation.space_id.to_string(), &observation.body_sha256],
                    row_to_observation,
                )
                .optional()?
            {
                return Ok(existing);
            }

            let id = Uuid::new_v4();
            let now = Utc::now();
            conn.execute(
                "INSERT INTO source_observations (
                     id, space_id, origin_url, origin_local_path, retrieved_at,
                     body_sha256, body_bytes, media_type, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.to_string(),
                    observation.space_id.to_string(),
                    observation.origin.url,
                    observation.origin.local_path,
                    observation.retrieved_at.to_rfc3339(),
                    observation.body_sha256,
                    i64::try_from(observation.body_bytes).unwrap_or(i64::MAX),
                    observation.media_type,
                    now.to_rfc3339(),
                ],
            )?;

            Ok(SourceObservation {
                id,
                space_id: observation.space_id,
                origin: observation.origin,
                retrieved_at: observation.retrieved_at,
                body_sha256: observation.body_sha256,
                body_bytes: observation.body_bytes,
                media_type: observation.media_type,
                created_at: now,
            })
        })
        .await
    }

    async fn get_observation(&self, id: Uuid) -> Result<Option<SourceObservation>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {OBSERVATION_COLUMNS} FROM source_observations WHERE id = ?1"),
                params![id.to_string()],
                row_to_observation,
            )
            .optional()
        })
        .await
    }

    async fn list_observations_by_space(
        &self,
        space_id: Uuid,
    ) -> Result<Vec<SourceObservation>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {OBSERVATION_COLUMNS} FROM source_observations
                 WHERE space_id = ?1 ORDER BY retrieved_at DESC"
            ))?;
            let rows = stmt.query_map(params![space_id.to_string()], row_to_observation)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn add_extraction(
        &self,
        observation_id: Uuid,
        extracted_text: String,
        extractor: String,
    ) -> Result<ExtractionDerivation, StorageError> {
        // Pre-flight so a missing observation is a NotFound, not a bare FK error.
        if self.get_observation(observation_id).await?.is_none() {
            return Err(StorageError::NotFound {
                entity: "SourceObservation".into(),
                id: observation_id.to_string(),
            });
        }

        self.with_conn(move |conn| {
            // The store assigns versions; callers never pick one. MAX+1 under
            // the connection mutex is race-free, and the UNIQUE constraint
            // backs it up.
            let version: i64 = conn.query_row(
                "SELECT COALESCE(MAX(version), 0) + 1
                 FROM extraction_derivations WHERE observation_id = ?1",
                params![observation_id.to_string()],
                |row| row.get(0),
            )?;

            let id = Uuid::new_v4();
            let now = Utc::now();
            conn.execute(
                "INSERT INTO extraction_derivations (
                     id, observation_id, version, extracted_text, extractor, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.to_string(),
                    observation_id.to_string(),
                    version,
                    extracted_text,
                    extractor,
                    now.to_rfc3339(),
                ],
            )?;

            Ok(ExtractionDerivation {
                id,
                observation_id,
                version: u32::try_from(version).unwrap_or(u32::MAX),
                extracted_text,
                extractor,
                created_at: now,
            })
        })
        .await
    }

    async fn get_extraction(
        &self,
        observation_id: Uuid,
        version: u32,
    ) -> Result<Option<ExtractionDerivation>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, observation_id, version, extracted_text, extractor, created_at
                 FROM extraction_derivations
                 WHERE observation_id = ?1 AND version = ?2",
                params![observation_id.to_string(), version],
                |row| {
                    Ok(ExtractionDerivation {
                        id: parse_uuid(&row.get::<_, String>(0)?)?,
                        observation_id: parse_uuid(&row.get::<_, String>(1)?)?,
                        version: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                        extracted_text: row.get(3)?,
                        extractor: row.get(4)?,
                        created_at: parse_dt(&row.get::<_, String>(5)?)?,
                    })
                },
            )
            .optional()
        })
        .await
    }

    async fn latest_extraction(
        &self,
        observation_id: Uuid,
    ) -> Result<Option<ExtractionDerivation>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, observation_id, version, extracted_text, extractor, created_at
                 FROM extraction_derivations
                 WHERE observation_id = ?1
                 ORDER BY version DESC LIMIT 1",
                params![observation_id.to_string()],
                |row| {
                    Ok(ExtractionDerivation {
                        id: parse_uuid(&row.get::<_, String>(0)?)?,
                        observation_id: parse_uuid(&row.get::<_, String>(1)?)?,
                        version: u32::try_from(row.get::<_, i64>(2)?).unwrap_or(u32::MAX),
                        extracted_text: row.get(3)?,
                        extractor: row.get(4)?,
                        created_at: parse_dt(&row.get::<_, String>(5)?)?,
                    })
                },
            )
            .optional()
        })
        .await
    }

    async fn search_extractions(
        &self,
        space_id: Uuid,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeSearchHit>, StorageError> {
        let query = query.to_owned();
        self.with_conn(move |conn| {
            // Search only each observation's newest derivation: superseded
            // extractions stay addressable (citations pin them) but drop out
            // of default retrieval. bm25 ascends, so best matches come first.
            //
            // Quote each term independently so FTS5 implicitly requires every
            // term without requiring them to form one contiguous phrase. This
            // also keeps operators and punctuation in user input out of the
            // query grammar.
            let fts_query = query
                .split_whitespace()
                .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" ");
            let mut stmt = conn.prepare(
                "SELECT o.id, d.version, o.origin_url, o.origin_local_path, o.retrieved_at,
                        snippet(extraction_fts, 0, '', '', '…', 24) AS passage,
                        d.extracted_text
                 FROM extraction_fts
                 JOIN extraction_derivations d ON d.rowid = extraction_fts.rowid
                 JOIN source_observations o ON o.id = d.observation_id
                 WHERE extraction_fts MATCH ?1
                   AND o.space_id = ?2
                   AND d.version = (
                       SELECT MAX(version) FROM extraction_derivations
                       WHERE observation_id = d.observation_id
                   )
                   -- Assertions are append-only; the most recently inserted
                   -- assertion for an observation is its effective state.
                   AND NOT EXISTS (
                       SELECT 1 FROM knowledge_curation_assertions a
                       WHERE a.observation_id = o.id
                         AND a.rowid = (
                             SELECT MAX(latest.rowid)
                             FROM knowledge_curation_assertions latest
                             WHERE latest.observation_id = o.id
                         )
                         AND a.kind IN ('duplicate-of', 'superseded-by', 'stale')
                   )
                 ORDER BY bm25(extraction_fts)
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(
                params![
                    fts_query,
                    space_id.to_string(),
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |row| {
                    let observation_id = parse_uuid(&row.get::<_, String>(0)?)?;
                    let version = u32::try_from(row.get::<_, i64>(1)?).unwrap_or(u32::MAX);
                    let snippet: String = row.get(5)?;
                    let full_text: String = row.get(6)?;
                    // snippet() marks continuation with '…' at the ends; strip
                    // it so the stored passage is text that literally occurs in
                    // the derivation, then locate its byte offset there.
                    let passage = snippet.trim_matches('…').to_string();
                    let snippet_offset = full_text.find(&passage);
                    Ok(KnowledgeSearchHit {
                        observation_id,
                        derivation_version: version,
                        origin: ObservationOrigin {
                            url: row.get(2)?,
                            local_path: row.get(3)?,
                        },
                        retrieved_at: parse_dt(&row.get::<_, String>(4)?)?,
                        snippet: passage,
                        snippet_offset,
                    })
                },
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CurationAssertionKind, NewCurationAssertion, NewSpace};
    use crate::store::{CurationAssertionStore, SpaceStore};

    async fn storage_with_space() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage
            .create(NewSpace {
                name: "research".into(),
                slug: "research".into(),
            })
            .await
            .unwrap();
        (storage, space.id)
    }

    fn new_observation(space_id: Uuid, url: &str, body_sha256: &str) -> NewSourceObservation {
        NewSourceObservation {
            space_id,
            origin: ObservationOrigin {
                url: Some(url.into()),
                local_path: None,
            },
            retrieved_at: Utc::now(),
            body_sha256: body_sha256.into(),
            body_bytes: 1024,
            media_type: "text/html".into(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn record_and_get_observation() {
        let (storage, space_id) = storage_with_space().await;

        let recorded = storage
            .record_observation(new_observation(space_id, "https://example.org/a", "aa11"))
            .await
            .unwrap();

        let fetched = storage
            .get_observation(recorded.id)
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(fetched.body_sha256, "aa11");
        assert_eq!(fetched.origin.url.as_deref(), Some("https://example.org/a"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn identical_body_in_same_space_deduplicates() {
        let (storage, space_id) = storage_with_space().await;

        let first = storage
            .record_observation(new_observation(space_id, "https://example.org/a", "aa11"))
            .await
            .unwrap();
        // Same bytes fetched again — different URL, same body hash.
        let second = storage
            .record_observation(new_observation(
                space_id,
                "https://mirror.example/a",
                "aa11",
            ))
            .await
            .unwrap();

        assert_eq!(
            first.id, second.id,
            "one body, one evidence identity — the original observation returns"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn originless_observation_is_rejected() {
        let (storage, space_id) = storage_with_space().await;

        let mut observation = new_observation(space_id, "unused", "bb22");
        observation.origin = ObservationOrigin {
            url: None,
            local_path: None,
        };

        let result = storage.record_observation(observation).await;
        assert!(matches!(result, Err(StorageError::ConstraintViolation(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extraction_versions_ascend_and_never_mutate() {
        let (storage, space_id) = storage_with_space().await;
        let observation = storage
            .record_observation(new_observation(space_id, "https://example.org/a", "aa11"))
            .await
            .unwrap();

        let v1 = storage
            .add_extraction(observation.id, "first pass".into(), "extractor-a".into())
            .await
            .unwrap();
        let v2 = storage
            .add_extraction(observation.id, "better pass".into(), "extractor-b".into())
            .await
            .unwrap();

        assert_eq!(v1.version, 1);
        assert_eq!(v2.version, 2);

        // The superseded derivation is untouched — a citation pinned to
        // (observation, 1, offsets) still resolves to the same text.
        let pinned = storage
            .get_extraction(observation.id, 1)
            .await
            .unwrap()
            .expect("version 1 should survive re-extraction");
        assert_eq!(pinned.extracted_text, "first pass");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn latest_extraction_returns_newest_version_or_none() {
        let (storage, space_id) = storage_with_space().await;
        let observation = storage
            .record_observation(new_observation(space_id, "https://example.org/a", "aa11"))
            .await
            .unwrap();

        assert!(
            storage
                .latest_extraction(observation.id)
                .await
                .unwrap()
                .is_none(),
            "an unextracted observation has no latest derivation"
        );

        storage
            .add_extraction(observation.id, "first pass".into(), "extractor-a".into())
            .await
            .unwrap();
        storage
            .add_extraction(observation.id, "better pass".into(), "extractor-b".into())
            .await
            .unwrap();

        let latest = storage
            .latest_extraction(observation.id)
            .await
            .unwrap()
            .expect("two derivations exist");
        assert_eq!(latest.version, 2);
        assert_eq!(latest.extracted_text, "better pass");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extraction_for_unknown_observation_is_not_found() {
        let (storage, _space_id) = storage_with_space().await;

        let result = storage
            .add_extraction(Uuid::new_v4(), "text".into(), "extractor".into())
            .await;
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_finds_newest_derivation_and_locates_snippet() {
        let (storage, space_id) = storage_with_space().await;
        let observation = storage
            .record_observation(new_observation(
                space_id,
                "https://example.org/iron",
                "cc33",
            ))
            .await
            .unwrap();

        storage
            .add_extraction(
                observation.id,
                "cast iron is strong in compression".into(),
                "extractor-a".into(),
            )
            .await
            .unwrap();
        storage
            .add_extraction(
                observation.id,
                "cast iron is strong in compression but weak in tension".into(),
                "extractor-b".into(),
            )
            .await
            .unwrap();

        let hits = storage
            .search_extractions(space_id, "tension", 10)
            .await
            .unwrap();

        assert_eq!(hits.len(), 1, "one observation matches");
        let hit = &hits[0];
        assert_eq!(hit.observation_id, observation.id);
        assert_eq!(
            hit.derivation_version, 2,
            "search reads the newest derivation only"
        );
        // The stored offset re-locates the snippet in the pinned derivation —
        // the property citation validation depends on.
        let derivation = storage
            .get_extraction(observation.id, hit.derivation_version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            &derivation.extracted_text
                [hit.snippet_offset.unwrap()..hit.snippet_offset.unwrap() + hit.snippet.len()],
            hit.snippet
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_matches_non_contiguous_terms_and_locates_snippet() {
        let (storage, space_id) = storage_with_space().await;
        let observation = storage
            .record_observation(new_observation(
                space_id,
                "https://example.org/bees",
                "multi-word-query",
            ))
            .await
            .unwrap();
        let extracted_text = "The waggle dance communicates direction, while angular measurement error is reduced by averaging.";
        storage
            .add_extraction(observation.id, extracted_text.into(), "extractor".into())
            .await
            .unwrap();

        let hits = storage
            .search_extractions(space_id, "waggle dance angular error averaging", 10)
            .await
            .unwrap();

        assert_eq!(hits.len(), 1, "all separated query terms match");
        let hit = &hits[0];
        assert_eq!(hit.observation_id, observation.id);
        assert!(extracted_text.contains(&hit.snippet));
        let offset = hit.snippet_offset.expect("snippet has a stored offset");
        assert_eq!(
            &extracted_text[offset..offset + hit.snippet.len()],
            hit.snippet
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_still_matches_a_single_term() {
        let (storage, space_id) = storage_with_space().await;
        let observation = storage
            .record_observation(new_observation(
                space_id,
                "https://example.org/metals",
                "single-term-query",
            ))
            .await
            .unwrap();
        storage
            .add_extraction(
                observation.id,
                "Bronze casting preserves fine detail.".into(),
                "extractor".into(),
            )
            .await
            .unwrap();

        let hits = storage
            .search_extractions(space_id, "bronze", 10)
            .await
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation_id, observation.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_keeps_observation_retired_when_latest_assertion_is_stale() {
        let (storage, space_id) = storage_with_space().await;
        let observation = storage
            .record_observation(new_observation(
                space_id,
                "https://example.org/current",
                "latest-state-subject",
            ))
            .await
            .unwrap();
        let preferred = storage
            .record_observation(new_observation(
                space_id,
                "https://example.org/preferred",
                "latest-state-target",
            ))
            .await
            .unwrap();
        storage
            .add_extraction(
                observation.id,
                "distinctive effective state passage".into(),
                "extractor".into(),
            )
            .await
            .unwrap();

        storage
            .add_curation_assertion(NewCurationAssertion {
                space_id,
                observation_id: observation.id,
                kind: CurationAssertionKind::SupersededBy,
                target_observation_id: Some(preferred.id),
            })
            .await
            .unwrap();
        assert!(
            storage
                .search_extractions(space_id, "distinctive effective state", 10)
                .await
                .unwrap()
                .is_empty(),
            "the first assertion retires the observation from default retrieval"
        );

        storage
            .add_curation_assertion(NewCurationAssertion {
                space_id,
                observation_id: observation.id,
                kind: CurationAssertionKind::Stale,
                target_observation_id: None,
            })
            .await
            .unwrap();
        assert!(
            storage
                .search_extractions(space_id, "distinctive effective state", 10)
                .await
                .unwrap()
                .is_empty(),
            "a stale assertion also retires the observation from default retrieval"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_is_space_scoped() {
        let (storage, space_id) = storage_with_space().await;
        let other_space = storage
            .create(NewSpace {
                name: "other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();

        let observation = storage
            .record_observation(new_observation(space_id, "https://example.org/a", "dd44"))
            .await
            .unwrap();
        storage
            .add_extraction(observation.id, "molten bronze technique".into(), "x".into())
            .await
            .unwrap();

        let hits = storage
            .search_extractions(other_space.id, "bronze", 10)
            .await
            .unwrap();
        assert!(hits.is_empty(), "another space's evidence is invisible");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_treats_query_as_text_not_syntax() {
        let (storage, space_id) = storage_with_space().await;
        let literal_match = storage
            .record_observation(new_observation(space_id, "https://example.org/a", "ee55"))
            .await
            .unwrap();
        storage
            .add_extraction(
                literal_match.id,
                "A corpus may contain the word OR and an unclosed quotation mark.".into(),
                "x".into(),
            )
            .await
            .unwrap();
        let operator_broadened_match = storage
            .record_observation(new_observation(space_id, "https://example.org/b", "ff66"))
            .await
            .unwrap();
        storage
            .add_extraction(
                operator_broadened_match.id,
                "A corpus alone must not satisfy every alternative.".into(),
                "x".into(),
            )
            .await
            .unwrap();

        // Operators and malformed syntax are literal terms: OR cannot broaden
        // the match, and the unmatched quote cannot make MATCH fail to parse.
        let hits = storage
            .search_extractions(space_id, "corpus OR \"unclosed", 10)
            .await
            .expect("malformed query syntax is matched as text, not executed");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation_id, literal_match.id);
    }
}

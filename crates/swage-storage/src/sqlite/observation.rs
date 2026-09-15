use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    ExtractionDerivation, KnowledgeSearchHit, NewSourceObservation, ObservationOrigin,
    SourceCapture, SourceObservation,
};
use crate::sqlite::SqliteStorage;
use crate::store::ObservationStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

const OBSERVATION_COLUMNS: &str = "id, topic_id, origin_url, origin_local_path, retrieved_at,
     body_sha256, body_bytes, media_type, capture_scope, created_at";

fn row_to_observation(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceObservation> {
    Ok(SourceObservation {
        id: parse_uuid(&row.get::<_, String>(0)?)?,
        topic_id: parse_uuid(&row.get::<_, String>(1)?)?,
        origin: ObservationOrigin {
            url: row.get(2)?,
            local_path: row.get(3)?,
        },
        retrieved_at: parse_dt(&row.get::<_, String>(4)?)?,
        body_sha256: row.get(5)?,
        body_bytes: row.get::<_, i64>(6)?.try_into().unwrap_or(0),
        media_type: row.get(7)?,
        capture_scope: row.get(8)?,
        created_at: parse_dt(&row.get::<_, String>(9)?)?,
    })
}

fn row_to_capture(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceCapture> {
    Ok(SourceCapture {
        observation_id: parse_uuid(&row.get::<_, String>(0)?)?,
        topic_id: parse_uuid(&row.get::<_, String>(1)?)?,
        origin: ObservationOrigin {
            url: row.get(2)?,
            local_path: row.get(3)?,
        },
        capture_scope: row.get(4)?,
        captured_at: parse_dt(&row.get::<_, String>(5)?)?,
    })
}

fn record_source_capture(
    conn: &rusqlite::Connection,
    observation_id: Uuid,
    observation: &NewSourceObservation,
) -> rusqlite::Result<()> {
    let Some(scope) = observation.capture_scope.as_deref() else {
        return Ok(());
    };
    conn.execute(
        "INSERT OR IGNORE INTO source_captures (
             id, observation_id, topic_id, origin_url, origin_local_path,
             capture_scope, captured_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            Uuid::new_v4().to_string(),
            observation_id.to_string(),
            observation.topic_id.to_string(),
            observation.origin.url,
            observation.origin.local_path,
            scope,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
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
            // A capture scope is retry identity: the same unit of work
            // (a workflow's journalled agent call, re-executed after an
            // interruption) re-capturing the same origin is the SAME
            // observational act, even when the live body differs between
            // attempts, so it deduplicates to the first capture. A lost
            // race between two concurrent same-scope captures costs a
            // duplicate row — the pre-scope status quo — never a gap.
            if let Some(scope) = observation.capture_scope.as_deref()
                && let Some(existing) = conn
                    .query_row(
                        "SELECT o.id, o.topic_id, o.origin_url, o.origin_local_path,
                                    o.retrieved_at, o.body_sha256, o.body_bytes, o.media_type,
                                    o.capture_scope, o.created_at
                             FROM source_captures c
                             JOIN source_observations o ON o.id = c.observation_id
                             WHERE c.topic_id = ?1 AND c.capture_scope = ?2
                               AND c.origin_url IS ?3 AND c.origin_local_path IS ?4",
                        params![
                            observation.topic_id.to_string(),
                            scope,
                            observation.origin.url,
                            observation.origin.local_path,
                        ],
                        row_to_observation,
                    )
                    .optional()?
            {
                return Ok(existing);
            }

            // Identical bodies within a topic deduplicate to the existing
            // observation so evidence identity never forks: two captures of
            // the same bytes are one piece of evidence, whatever fetched them.
            if let Some(existing) = conn
                .query_row(
                    &format!(
                        "SELECT {OBSERVATION_COLUMNS} FROM source_observations
                         WHERE topic_id = ?1 AND body_sha256 = ?2"
                    ),
                    params![observation.topic_id.to_string(), &observation.body_sha256],
                    row_to_observation,
                )
                .optional()?
            {
                record_source_capture(conn, existing.id, &observation)?;
                return Ok(existing);
            }

            let id = Uuid::new_v4();
            let now = Utc::now();
            conn.execute(
                "INSERT INTO source_observations (
                     id, topic_id, origin_url, origin_local_path, retrieved_at,
                     body_sha256, body_bytes, media_type, capture_scope, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id.to_string(),
                    observation.topic_id.to_string(),
                    observation.origin.url,
                    observation.origin.local_path,
                    observation.retrieved_at.to_rfc3339(),
                    observation.body_sha256,
                    i64::try_from(observation.body_bytes).unwrap_or(i64::MAX),
                    observation.media_type,
                    observation.capture_scope,
                    now.to_rfc3339(),
                ],
            )?;

            record_source_capture(conn, id, &observation)?;

            Ok(SourceObservation {
                id,
                topic_id: observation.topic_id,
                origin: observation.origin,
                retrieved_at: observation.retrieved_at,
                body_sha256: observation.body_sha256,
                body_bytes: observation.body_bytes,
                media_type: observation.media_type,
                capture_scope: observation.capture_scope,
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

    async fn list_observations_by_topic(
        &self,
        topic_id: Uuid,
    ) -> Result<Vec<SourceObservation>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {OBSERVATION_COLUMNS} FROM source_observations
                 WHERE topic_id = ?1 ORDER BY retrieved_at DESC"
            ))?;
            let rows = stmt.query_map(params![topic_id.to_string()], row_to_observation)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn list_source_captures_by_scopes(
        &self,
        topic_id: Uuid,
        capture_scopes: Vec<String>,
    ) -> Result<Vec<SourceCapture>, StorageError> {
        if capture_scopes.is_empty() {
            return Ok(Vec::new());
        }
        self.with_conn(move |conn| {
            let placeholders = std::iter::repeat_n("?", capture_scopes.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT observation_id, topic_id, origin_url, origin_local_path,
                        capture_scope, captured_at FROM source_captures
                 WHERE topic_id = ?1 AND capture_scope IN ({placeholders})
                 ORDER BY captured_at DESC"
            );
            let mut parameters = Vec::with_capacity(capture_scopes.len() + 1);
            parameters.push(topic_id.to_string());
            parameters.extend(capture_scopes);
            let mut statement = conn.prepare(&query)?;
            let rows =
                statement.query_map(rusqlite::params_from_iter(parameters), row_to_capture)?;
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
        topic_id: Uuid,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeSearchHit>, StorageError> {
        let query = query.to_owned();
        self.with_conn(move |conn| {
            // Search only each observation's newest derivation: superseded
            // extractions stay addressable (citations pin them) but drop out
            // of default retrieval. bm25 ascends, so best matches come first.
            //
            // Quote each term independently so FTS5 first requires every term
            // without requiring them to form one contiguous phrase. If that
            // precise search finds nothing, retry with any term; both forms
            // keep operators and punctuation in user input out of the grammar.
            let terms = query
                .split_whitespace()
                .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
                .collect::<Vec<_>>();
            if terms.is_empty() {
                return Ok(Vec::new());
            }
            let all_terms_query = terms.join(" ");
            let any_term_query = terms.join(" OR ");
            let mut stmt = conn.prepare(
                "SELECT o.id, d.version, o.origin_url, o.origin_local_path, o.retrieved_at,
                        snippet(extraction_fts, 0, '', '', '…', 24) AS passage,
                        d.extracted_text
                 FROM extraction_fts
                 JOIN extraction_derivations d ON d.rowid = extraction_fts.rowid
                 JOIN source_observations o ON o.id = d.observation_id
                 WHERE extraction_fts MATCH ?1
                   AND o.topic_id = ?2
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

            let mut search = |fts_query: &str| {
                let rows = stmt.query_map(
                    params![
                        fts_query,
                        topic_id.to_string(),
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
            };

            let hits = search(&all_terms_query)?;
            if hits.is_empty() {
                search(&any_term_query)
            } else {
                Ok(hits)
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CurationAssertionKind, NewCurationAssertion, NewTopic};
    use crate::store::{CurationAssertionStore, TopicStore};

    async fn storage_with_topic() -> (SqliteStorage, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "research".into(),
                slug: "research".into(),
            })
            .await
            .unwrap();
        (storage, topic.id)
    }

    fn new_observation(topic_id: Uuid, url: &str, body_sha256: &str) -> NewSourceObservation {
        NewSourceObservation {
            topic_id,
            origin: ObservationOrigin {
                url: Some(url.into()),
                local_path: None,
            },
            retrieved_at: Utc::now(),
            body_sha256: body_sha256.into(),
            body_bytes: 1024,
            media_type: "text/html".into(),
            capture_scope: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn record_and_get_observation() {
        let (storage, topic_id) = storage_with_topic().await;

        let recorded = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "aa11"))
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
    async fn identical_body_in_same_topic_deduplicates() {
        let (storage, topic_id) = storage_with_topic().await;

        let first = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "aa11"))
            .await
            .unwrap();
        // Same bytes fetched again — different URL, same body hash.
        let second = storage
            .record_observation(new_observation(
                topic_id,
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
    async fn same_scope_and_origin_deduplicates_across_differing_bodies() {
        let (storage, topic_id) = storage_with_topic().await;

        let mut first = new_observation(topic_id, "https://example.org/a", "aa11");
        first.capture_scope = Some("call-1".into());
        let first = storage.record_observation(first).await.unwrap();

        // The same journalled call re-runs after an interruption and fetches
        // the same page again; the live body differs between attempts.
        let mut retried = new_observation(topic_id, "https://example.org/a", "cc33");
        retried.capture_scope = Some("call-1".into());
        let retried = storage.record_observation(retried).await.unwrap();

        assert_eq!(
            first.id, retried.id,
            "one capture per origin per scope — the retry returns the first attempt's observation"
        );
        assert_eq!(
            storage
                .list_observations_by_topic(topic_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn same_scope_different_origin_records_separately() {
        let (storage, topic_id) = storage_with_topic().await;

        let mut first = new_observation(topic_id, "https://example.org/a", "aa11");
        first.capture_scope = Some("call-1".into());
        let first = storage.record_observation(first).await.unwrap();

        let mut second = new_observation(topic_id, "https://example.org/b", "bb22");
        second.capture_scope = Some("call-1".into());
        let second = storage.record_observation(second).await.unwrap();

        assert_ne!(
            first.id, second.id,
            "a scope bounds retries of one capture, not everything the call records"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn different_scopes_re_observe_the_same_origin() {
        let (storage, topic_id) = storage_with_topic().await;

        let mut first = new_observation(topic_id, "https://example.org/a", "aa11");
        first.capture_scope = Some("call-1".into());
        let first = storage.record_observation(first).await.unwrap();

        // A later sweep captures the same origin with changed content: a
        // legitimate re-observation, not a retry.
        let mut later = new_observation(topic_id, "https://example.org/a", "dd44");
        later.capture_scope = Some("call-2".into());
        let later = storage.record_observation(later).await.unwrap();

        assert_ne!(first.id, later.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn scopeless_captures_keep_body_only_dedup() {
        let (storage, topic_id) = storage_with_topic().await;

        let first = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "aa11"))
            .await
            .unwrap();
        let second = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "ee55"))
            .await
            .unwrap();

        assert_ne!(
            first.id, second.id,
            "without a scope, differing bodies of one origin stay distinct observations"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn originless_observation_is_rejected() {
        let (storage, topic_id) = storage_with_topic().await;

        let mut observation = new_observation(topic_id, "unused", "bb22");
        observation.origin = ObservationOrigin {
            url: None,
            local_path: None,
        };

        let result = storage.record_observation(observation).await;
        assert!(matches!(result, Err(StorageError::ConstraintViolation(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extraction_versions_ascend_and_never_mutate() {
        let (storage, topic_id) = storage_with_topic().await;
        let observation = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "aa11"))
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
        let (storage, topic_id) = storage_with_topic().await;
        let observation = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "aa11"))
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
        let (storage, _topic_id) = storage_with_topic().await;

        let result = storage
            .add_extraction(Uuid::new_v4(), "text".into(), "extractor".into())
            .await;
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_finds_newest_derivation_and_locates_snippet() {
        let (storage, topic_id) = storage_with_topic().await;
        let observation = storage
            .record_observation(new_observation(
                topic_id,
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
            .search_extractions(topic_id, "tension", 10)
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
        let (storage, topic_id) = storage_with_topic().await;
        let observation = storage
            .record_observation(new_observation(
                topic_id,
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
            .search_extractions(topic_id, "waggle dance angular error averaging", 10)
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
        let (storage, topic_id) = storage_with_topic().await;
        let observation = storage
            .record_observation(new_observation(
                topic_id,
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
            .search_extractions(topic_id, "bronze", 10)
            .await
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation_id, observation.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_falls_back_to_ranked_any_term_only_after_no_all_term_match() {
        let (storage, topic_id) = storage_with_topic().await;
        let strong_match = storage
            .record_observation(new_observation(
                topic_id,
                "https://example.org/strong",
                "fallback-strong",
            ))
            .await
            .unwrap();
        storage
            .add_extraction(
                strong_match.id,
                "Honeybee waggle dances communicate direction.".into(),
                "extractor".into(),
            )
            .await
            .unwrap();
        let partial_match = storage
            .record_observation(new_observation(
                topic_id,
                "https://example.org/partial",
                "fallback-partial",
            ))
            .await
            .unwrap();
        storage
            .add_extraction(
                partial_match.id,
                "Honeybee behaviour under noise.".into(),
                "extractor".into(),
            )
            .await
            .unwrap();

        let precise_hits = storage
            .search_extractions(topic_id, "honeybee direction", 10)
            .await
            .unwrap();
        assert_eq!(precise_hits.len(), 1, "partial matches stay excluded");
        assert_eq!(precise_hits[0].observation_id, strong_match.id);

        let fallback_hits = storage
            .search_extractions(topic_id, "honeybee absent", 10)
            .await
            .unwrap();
        assert_eq!(fallback_hits.len(), 2, "any-term fallback returns matches");
        assert_eq!(fallback_hits[0].observation_id, partial_match.id);
        assert_eq!(fallback_hits[1].observation_id, strong_match.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_with_only_whitetopic_returns_no_hits() {
        let (storage, topic_id) = storage_with_topic().await;

        let hits = storage
            .search_extractions(topic_id, " \t\n ", 10)
            .await
            .expect("empty term list does not reach the FTS5 parser");

        assert!(hits.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_keeps_observation_retired_when_latest_assertion_is_stale() {
        let (storage, topic_id) = storage_with_topic().await;
        let observation = storage
            .record_observation(new_observation(
                topic_id,
                "https://example.org/current",
                "latest-state-subject",
            ))
            .await
            .unwrap();
        let preferred = storage
            .record_observation(new_observation(
                topic_id,
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
                topic_id,
                observation_id: observation.id,
                kind: CurationAssertionKind::SupersededBy,
                target_observation_id: Some(preferred.id),
            })
            .await
            .unwrap();
        assert!(
            storage
                .search_extractions(topic_id, "distinctive effective state", 10)
                .await
                .unwrap()
                .is_empty(),
            "the first assertion retires the observation from default retrieval"
        );

        storage
            .add_curation_assertion(NewCurationAssertion {
                topic_id,
                observation_id: observation.id,
                kind: CurationAssertionKind::Stale,
                target_observation_id: None,
            })
            .await
            .unwrap();
        assert!(
            storage
                .search_extractions(topic_id, "distinctive effective state", 10)
                .await
                .unwrap()
                .is_empty(),
            "a stale assertion also retires the observation from default retrieval"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_is_topic_scoped() {
        let (storage, topic_id) = storage_with_topic().await;
        let other_topic = storage
            .create(NewTopic {
                name: "other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();

        let observation = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "dd44"))
            .await
            .unwrap();
        storage
            .add_extraction(observation.id, "molten bronze technique".into(), "x".into())
            .await
            .unwrap();

        let hits = storage
            .search_extractions(other_topic.id, "bronze", 10)
            .await
            .unwrap();
        assert!(hits.is_empty(), "another topic's evidence is invisible");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_treats_query_as_text_not_syntax() {
        let (storage, topic_id) = storage_with_topic().await;
        let literal_match = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "ee55"))
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
            .record_observation(new_observation(topic_id, "https://example.org/b", "ff66"))
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
            .search_extractions(topic_id, "corpus OR \"unclosed", 10)
            .await
            .expect("malformed query syntax is matched as text, not executed");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation_id, literal_match.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_fallback_treats_query_as_text_not_syntax() {
        let (storage, topic_id) = storage_with_topic().await;
        let literal_match = storage
            .record_observation(new_observation(topic_id, "https://example.org/a", "gg77"))
            .await
            .unwrap();
        storage
            .add_extraction(
                literal_match.id,
                "This example contains the literal word OR.".into(),
                "x".into(),
            )
            .await
            .unwrap();
        let operator_broadened_match = storage
            .record_observation(new_observation(topic_id, "https://example.org/b", "hh88"))
            .await
            .unwrap();
        storage
            .add_extraction(
                operator_broadened_match.id,
                "An unrelated alternative must remain excluded.".into(),
                "x".into(),
            )
            .await
            .unwrap();

        let hits = storage
            .search_extractions(topic_id, "absent OR \"unclosed missing", 10)
            .await
            .expect("fallback syntax is matched as text, not executed");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].observation_id, literal_match.id);
    }
}

use async_trait::async_trait;
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{CurationAssertion, CurationAssertionKind, NewCurationAssertion};
use crate::sqlite::SqliteStorage;
use crate::store::{CurationAssertionStore, ObservationStore};

#[async_trait]
impl CurationAssertionStore for SqliteStorage {
    async fn add_curation_assertion(
        &self,
        assertion: NewCurationAssertion,
    ) -> Result<CurationAssertion, StorageError> {
        let observation = self
            .get_observation(assertion.observation_id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "SourceObservation".into(),
                id: assertion.observation_id.to_string(),
            })?;
        if observation.topic_id != assertion.topic_id {
            return Err(StorageError::ConstraintViolation(
                "the observation does not belong to the assertion's topic".into(),
            ));
        }

        match (assertion.kind, assertion.target_observation_id) {
            (CurationAssertionKind::Stale, Some(_)) => {
                return Err(StorageError::ConstraintViolation(
                    "a stale assertion must not have a target observation".into(),
                ));
            }
            (CurationAssertionKind::DuplicateOf | CurationAssertionKind::SupersededBy, None) => {
                return Err(StorageError::ConstraintViolation(
                    "relationship assertions need a target observation".into(),
                ));
            }
            _ => {}
        }

        if let Some(target_id) = assertion.target_observation_id {
            if target_id == assertion.observation_id {
                return Err(StorageError::ConstraintViolation(
                    "an observation cannot target itself".into(),
                ));
            }
            let target =
                self.get_observation(target_id)
                    .await?
                    .ok_or_else(|| StorageError::NotFound {
                        entity: "SourceObservation".into(),
                        id: target_id.to_string(),
                    })?;
            if target.topic_id != assertion.topic_id {
                return Err(StorageError::ConstraintViolation(
                    "the target observation does not belong to the assertion's topic".into(),
                ));
            }
        }

        self.with_conn(move |conn| {
            let record = CurationAssertion {
                id: Uuid::new_v4(),
                topic_id: assertion.topic_id,
                observation_id: assertion.observation_id,
                kind: assertion.kind,
                target_observation_id: assertion.target_observation_id,
                created_at: Utc::now(),
            };
            conn.execute(
                "INSERT INTO knowledge_curation_assertions (
                     id, topic_id, observation_id, kind, target_observation_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.id.to_string(),
                    record.topic_id.to_string(),
                    record.observation_id.to_string(),
                    record.kind.to_string(),
                    record.target_observation_id.map(|id| id.to_string()),
                    record.created_at.to_rfc3339(),
                ],
            )?;
            Ok(record)
        })
        .await
    }

    async fn effective_curation_by_topic(
        &self,
        topic_id: Uuid,
    ) -> Result<Vec<CurationAssertion>, StorageError> {
        self.with_conn(move |conn| {
            // Newest assertion per observation, by the same latest-rowid rule
            // the search filter treats as effective state.
            let mut stmt = conn.prepare(
                "SELECT a.id, a.topic_id, a.observation_id, a.kind,
                        a.target_observation_id, a.created_at
                 FROM knowledge_curation_assertions a
                 WHERE a.topic_id = ?1
                   AND a.rowid = (
                       SELECT MAX(latest.rowid)
                       FROM knowledge_curation_assertions latest
                       WHERE latest.observation_id = a.observation_id
                   )
                 ORDER BY a.observation_id",
            )?;
            let rows = stmt.query_map(params![topic_id.to_string()], |row| {
                let parse_uuid = |value: String| {
                    value.parse::<Uuid>().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                };
                Ok(CurationAssertion {
                    id: parse_uuid(row.get::<_, String>(0)?)?,
                    topic_id: parse_uuid(row.get::<_, String>(1)?)?,
                    observation_id: parse_uuid(row.get::<_, String>(2)?)?,
                    kind: row.get::<_, String>(3)?.parse().map_err(|error: String| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            error.into(),
                        )
                    })?,
                    target_observation_id: row
                        .get::<_, Option<String>>(4)?
                        .map(parse_uuid)
                        .transpose()?,
                    created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?
                        .with_timezone(&Utc),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewSourceObservation, NewTopic, ObservationOrigin};
    use crate::store::{ObservationStore, TopicStore};

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

    async fn observation(
        storage: &SqliteStorage,
        topic_id: Uuid,
        label: &str,
    ) -> crate::model::SourceObservation {
        storage
            .record_observation(NewSourceObservation {
                topic_id,
                origin: ObservationOrigin {
                    url: Some(format!("https://example.test/{label}")),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: format!("hash-{label}"),
                body_bytes: 20,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn effective_curation_is_the_newest_assertion_per_observation() {
        let (storage, topic_id) = storage_with_topic().await;
        let subject = observation(&storage, topic_id, "subject").await;
        let target = observation(&storage, topic_id, "target").await;

        storage
            .add_curation_assertion(NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::Stale,
                target_observation_id: None,
            })
            .await
            .unwrap();
        storage
            .add_curation_assertion(NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::SupersededBy,
                target_observation_id: Some(target.id),
            })
            .await
            .unwrap();

        let effective = storage.effective_curation_by_topic(topic_id).await.unwrap();

        // Only the asserted observation appears, and only its newest
        // judgement: the earlier stale assertion is history, not state.
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].observation_id, subject.id);
        assert_eq!(effective[0].kind, CurationAssertionKind::SupersededBy);
        assert_eq!(effective[0].target_observation_id, Some(target.id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accepts_all_assertion_kinds() {
        let (storage, topic_id) = storage_with_topic().await;
        let subject = observation(&storage, topic_id, "subject").await;
        let target = observation(&storage, topic_id, "target").await;

        for kind in [
            CurationAssertionKind::DuplicateOf,
            CurationAssertionKind::SupersededBy,
        ] {
            storage
                .add_curation_assertion(NewCurationAssertion {
                    topic_id,
                    observation_id: subject.id,
                    kind,
                    target_observation_id: Some(target.id),
                })
                .await
                .unwrap();
        }
        storage
            .add_curation_assertion(NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::Stale,
                target_observation_id: None,
            })
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn validates_relationship_targets_and_topic_membership() {
        let (storage, topic_id) = storage_with_topic().await;
        let subject = observation(&storage, topic_id, "subject").await;
        let other_topic = storage
            .create(NewTopic {
                name: "other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();
        let foreign = observation(&storage, other_topic.id, "foreign").await;

        for invalid in [
            NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::DuplicateOf,
                target_observation_id: None,
            },
            NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::Stale,
                target_observation_id: Some(subject.id),
            },
            NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::SupersededBy,
                target_observation_id: Some(foreign.id),
            },
        ] {
            assert!(matches!(
                storage.add_curation_assertion(invalid).await,
                Err(StorageError::ConstraintViolation(_))
            ));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn superseded_observation_is_hidden_but_evidence_remains_unchanged() {
        let (storage, topic_id) = storage_with_topic().await;
        let subject = observation(&storage, topic_id, "subject").await;
        let replacement = observation(&storage, topic_id, "replacement").await;
        let derivation = storage
            .add_extraction(
                subject.id,
                "a uniquely superseded passage".into(),
                "test extractor".into(),
            )
            .await
            .unwrap();

        assert_eq!(
            storage
                .search_extractions(topic_id, "uniquely superseded", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        storage
            .add_curation_assertion(NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::SupersededBy,
                target_observation_id: Some(replacement.id),
            })
            .await
            .unwrap();

        assert!(
            storage
                .search_extractions(topic_id, "uniquely superseded", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            storage
                .get_observation(subject.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            subject.id
        );
        assert_eq!(
            storage
                .get_observation(subject.id)
                .await
                .unwrap()
                .unwrap()
                .body_sha256,
            subject.body_sha256
        );
        let stored_derivation = storage
            .get_extraction(subject.id, derivation.version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored_derivation.id, derivation.id);
        assert_eq!(stored_derivation.extracted_text, derivation.extracted_text);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn assertion_rows_reject_update_and_delete() {
        let (storage, topic_id) = storage_with_topic().await;
        let subject = observation(&storage, topic_id, "subject").await;
        let assertion = storage
            .add_curation_assertion(NewCurationAssertion {
                topic_id,
                observation_id: subject.id,
                kind: CurationAssertionKind::Stale,
                target_observation_id: None,
            })
            .await
            .unwrap();

        for sql in [
            "UPDATE knowledge_curation_assertions SET kind = 'stale' WHERE id = ?1",
            "DELETE FROM knowledge_curation_assertions WHERE id = ?1",
        ] {
            assert!(
                storage
                    .with_conn(|conn| conn.execute(sql, params![assertion.id.to_string()]))
                    .await
                    .is_err()
            );
        }
    }
}

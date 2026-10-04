use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{KnowledgeCitation, NewKnowledgeCitation};
use crate::sqlite::SqliteStorage;
use crate::store::{CitationStore, ObservationStore};

fn parse_uuid(value: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_dt(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn usize_to_sql_integer(value: usize) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn sql_integer_to_usize(column: usize, value: i64) -> rusqlite::Result<usize> {
    usize::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn sql_integer_to_u32(column: usize, value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

#[async_trait]
impl CitationStore for SqliteStorage {
    async fn add_citation(
        &self,
        citation: NewKnowledgeCitation,
    ) -> Result<KnowledgeCitation, StorageError> {
        if citation.quoted_text.is_empty() {
            return Err(StorageError::ConstraintViolation(
                "a citation quote must not be empty".into(),
            ));
        }

        let observation = self
            .get_observation(citation.observation_id)
            .await?
            .ok_or_else(|| StorageError::NotFound {
                entity: "SourceObservation".into(),
                id: citation.observation_id.to_string(),
            })?;
        if observation.topic_id != citation.topic_id {
            return Err(StorageError::ConstraintViolation(
                "the observation does not belong to the citation's topic".into(),
            ));
        }

        let NewKnowledgeCitation {
            topic_id,
            observation_id,
            derivation_version,
            quoted_text,
        } = citation;
        self.with_conn(move |conn| {
            let (extracted_text, extractor) = conn
                .query_row(
                    "SELECT extracted_text, extractor FROM extraction_derivations
                     WHERE observation_id = ?1 AND version = ?2",
                    params![observation_id.to_string(), derivation_version],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .ok_or_else(|| {
                    rusqlite::Error::ToSqlConversionFailure(
                        format!(
                            "extraction derivation version {derivation_version} does not exist for observation {observation_id}"
                        )
                        .into(),
                    )
                })?;
            // Text the model supplied is never the ground a citation stands
            // on: only platform-derived text locates a quote.
            if extractor == crate::model::AGENT_SUPPLIED_EXTRACTOR {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    "the pinned derivation is model-authored and is not citable; cite a \
                     platform-derived version"
                        .into(),
                ));
            }
            let start_offset = extracted_text.find(&quoted_text).ok_or_else(|| {
                rusqlite::Error::ToSqlConversionFailure(
                    "the quoted text does not occur in the pinned derivation".into(),
                )
            })?;
            let end_offset = start_offset + quoted_text.len();
            let record = KnowledgeCitation {
                id: Uuid::new_v4(),
                topic_id,
                observation_id,
                derivation_version,
                start_offset,
                end_offset,
                quoted_text,
                created_at: Utc::now(),
            };
            conn.execute(
                "INSERT INTO knowledge_citations (
                     id, topic_id, observation_id, derivation_version,
                     start_offset, end_offset, quoted_text, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.id.to_string(),
                    record.topic_id.to_string(),
                    record.observation_id.to_string(),
                    record.derivation_version,
                    usize_to_sql_integer(record.start_offset)?,
                    usize_to_sql_integer(record.end_offset)?,
                    record.quoted_text,
                    record.created_at.to_rfc3339(),
                ],
            )?;
            Ok(record)
        })
        .await
    }

    async fn get_citation(&self, id: Uuid) -> Result<Option<KnowledgeCitation>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, topic_id, observation_id, derivation_version,
                        start_offset, end_offset, quoted_text, created_at
                 FROM knowledge_citations WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok(KnowledgeCitation {
                        id: parse_uuid(&row.get::<_, String>(0)?)?,
                        topic_id: parse_uuid(&row.get::<_, String>(1)?)?,
                        observation_id: parse_uuid(&row.get::<_, String>(2)?)?,
                        derivation_version: sql_integer_to_u32(3, row.get(3)?)?,
                        start_offset: sql_integer_to_usize(4, row.get(4)?)?,
                        end_offset: sql_integer_to_usize(5, row.get(5)?)?,
                        quoted_text: row.get(6)?,
                        created_at: parse_dt(&row.get::<_, String>(7)?)?,
                    })
                },
            )
            .optional()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewSourceObservation, NewTopic, ObservationOrigin};
    use crate::store::{ObservationStore, TopicStore};

    async fn evidence() -> (SqliteStorage, Uuid, Uuid, u32) {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
                name: "research".into(),
                slug: "research".into(),
            })
            .await
            .unwrap();
        let observation = storage
            .record_observation(NewSourceObservation {
                topic_id: topic.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "citation-body".into(),
                body_bytes: 64,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        let derivation = storage
            .add_extraction(
                observation.id,
                "prefix — exact quoted evidence — suffix".into(),
                "test".into(),
            )
            .await
            .unwrap();
        (storage, topic.id, observation.id, derivation.version)
    }

    async fn citation_count(storage: &SqliteStorage) -> i64 {
        storage
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM knowledge_citations", [], |row| {
                    row.get(0)
                })
            })
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locates_exact_quote_and_pins_derivation_version() {
        let (storage, topic_id, observation_id, version) = evidence().await;
        let quote = "exact quoted evidence";
        let citation = storage
            .add_citation(NewKnowledgeCitation {
                topic_id,
                observation_id,
                derivation_version: version,
                quoted_text: quote.into(),
            })
            .await
            .unwrap();

        assert_eq!(citation.derivation_version, version);
        assert_eq!(citation.start_offset, "prefix — ".len());
        assert_eq!(citation.end_offset, citation.start_offset + quote.len());
        assert_eq!(
            storage.get_citation(citation.id).await.unwrap(),
            Some(citation)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refuses_to_cite_text_the_model_supplied() {
        let (storage, topic_id, observation_id, _platform_version) = evidence().await;
        let before = citation_count(&storage).await;
        let supplied = storage
            .add_extraction(
                observation_id,
                "words the gatherer typed in".into(),
                crate::model::AGENT_SUPPLIED_EXTRACTOR.into(),
            )
            .await
            .unwrap();
        assert!(!supplied.is_citable());

        let error = storage
            .add_citation(NewKnowledgeCitation {
                topic_id,
                observation_id,
                derivation_version: supplied.version,
                quoted_text: "words the gatherer typed in".into(),
            })
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("not citable"),
            "a model-authored derivation is refused by name: {error}"
        );
        assert_eq!(citation_count(&storage).await, before, "nothing inserted");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refuses_unlocatable_empty_and_missing_version_without_inserting() {
        let (storage, topic_id, observation_id, version) = evidence().await;
        for (quoted_text, derivation_version) in [
            ("fabricated evidence", version),
            ("Exact quoted evidence", version),
            ("", version),
            ("exact quoted evidence", version + 1),
        ] {
            assert!(
                storage
                    .add_citation(NewKnowledgeCitation {
                        topic_id,
                        observation_id,
                        derivation_version,
                        quoted_text: quoted_text.into(),
                    })
                    .await
                    .is_err()
            );
        }
        assert_eq!(citation_count(&storage).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refuses_foreign_topic_and_mismatched_observation_without_inserting() {
        let (storage, topic_id, observation_id, version) = evidence().await;
        let other_topic = storage
            .create(NewTopic {
                name: "other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();
        let other_observation = storage
            .record_observation(NewSourceObservation {
                topic_id,
                origin: ObservationOrigin {
                    url: Some("https://other.test".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "other-body".into(),
                body_bytes: 5,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        for (candidate_topic, candidate_observation) in [
            (other_topic.id, observation_id),
            (topic_id, other_observation.id),
        ] {
            assert!(
                storage
                    .add_citation(NewKnowledgeCitation {
                        topic_id: candidate_topic,
                        observation_id: candidate_observation,
                        derivation_version: version,
                        quoted_text: "exact quoted evidence".into(),
                    })
                    .await
                    .is_err()
            );
        }
        assert_eq!(citation_count(&storage).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn citation_rows_reject_update_and_delete() {
        let (storage, topic_id, observation_id, version) = evidence().await;
        let citation = storage
            .add_citation(NewKnowledgeCitation {
                topic_id,
                observation_id,
                derivation_version: version,
                quoted_text: "exact quoted evidence".into(),
            })
            .await
            .unwrap();
        for sql in [
            "UPDATE knowledge_citations SET quoted_text = 'changed' WHERE id = ?1",
            "DELETE FROM knowledge_citations WHERE id = ?1",
        ] {
            assert!(
                storage
                    .with_conn(|conn| conn.execute(sql, params![citation.id.to_string()]))
                    .await
                    .is_err()
            );
        }
    }
}

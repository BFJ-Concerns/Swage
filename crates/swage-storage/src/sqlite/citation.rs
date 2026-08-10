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
        if observation.space_id != citation.space_id {
            return Err(StorageError::ConstraintViolation(
                "the observation does not belong to the citation's space".into(),
            ));
        }

        let NewKnowledgeCitation {
            space_id,
            observation_id,
            derivation_version,
            quoted_text,
        } = citation;
        self.with_conn(move |conn| {
            let extracted_text = conn
                .query_row(
                    "SELECT extracted_text FROM extraction_derivations
                     WHERE observation_id = ?1 AND version = ?2",
                    params![observation_id.to_string(), derivation_version],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
            let start_offset = extracted_text.find(&quoted_text).ok_or_else(|| {
                rusqlite::Error::ToSqlConversionFailure(
                    "the quoted text does not occur in the pinned derivation".into(),
                )
            })?;
            let end_offset = start_offset + quoted_text.len();
            let record = KnowledgeCitation {
                id: Uuid::new_v4(),
                space_id,
                observation_id,
                derivation_version,
                start_offset,
                end_offset,
                quoted_text,
                created_at: Utc::now(),
            };
            conn.execute(
                "INSERT INTO knowledge_citations (
                     id, space_id, observation_id, derivation_version,
                     start_offset, end_offset, quoted_text, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.id.to_string(),
                    record.space_id.to_string(),
                    record.observation_id.to_string(),
                    record.derivation_version,
                    i64::try_from(record.start_offset).unwrap_or(i64::MAX),
                    i64::try_from(record.end_offset).unwrap_or(i64::MAX),
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
                "SELECT id, space_id, observation_id, derivation_version,
                        start_offset, end_offset, quoted_text, created_at
                 FROM knowledge_citations WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok(KnowledgeCitation {
                        id: parse_uuid(&row.get::<_, String>(0)?)?,
                        space_id: parse_uuid(&row.get::<_, String>(1)?)?,
                        observation_id: parse_uuid(&row.get::<_, String>(2)?)?,
                        derivation_version: u32::try_from(row.get::<_, i64>(3)?)
                            .unwrap_or(u32::MAX),
                        start_offset: usize::try_from(row.get::<_, i64>(4)?).unwrap_or(usize::MAX),
                        end_offset: usize::try_from(row.get::<_, i64>(5)?).unwrap_or(usize::MAX),
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
    use crate::model::{NewSourceObservation, NewSpace, ObservationOrigin};
    use crate::store::{ObservationStore, SpaceStore};

    async fn evidence() -> (SqliteStorage, Uuid, Uuid, u32) {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage
            .create(NewSpace {
                name: "research".into(),
                slug: "research".into(),
            })
            .await
            .unwrap();
        let observation = storage
            .record_observation(NewSourceObservation {
                space_id: space.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "citation-body".into(),
                body_bytes: 64,
                media_type: "text/plain".into(),
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
        (storage, space.id, observation.id, derivation.version)
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
        let (storage, space_id, observation_id, version) = evidence().await;
        let quote = "exact quoted evidence";
        let citation = storage
            .add_citation(NewKnowledgeCitation {
                space_id,
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
    async fn refuses_unlocatable_empty_and_missing_version_without_inserting() {
        let (storage, space_id, observation_id, version) = evidence().await;
        for (quoted_text, derivation_version) in [
            ("fabricated evidence", version),
            ("Exact quoted evidence", version),
            ("", version),
            ("exact quoted evidence", version + 1),
        ] {
            assert!(
                storage
                    .add_citation(NewKnowledgeCitation {
                        space_id,
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
    async fn refuses_foreign_space_and_mismatched_observation_without_inserting() {
        let (storage, space_id, observation_id, version) = evidence().await;
        let other_space = storage
            .create(NewSpace {
                name: "other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();
        let other_observation = storage
            .record_observation(NewSourceObservation {
                space_id,
                origin: ObservationOrigin {
                    url: Some("https://other.test".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: "other-body".into(),
                body_bytes: 5,
                media_type: "text/plain".into(),
            })
            .await
            .unwrap();
        for (candidate_space, candidate_observation) in [
            (other_space.id, observation_id),
            (space_id, other_observation.id),
        ] {
            assert!(
                storage
                    .add_citation(NewKnowledgeCitation {
                        space_id: candidate_space,
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
        let (storage, space_id, observation_id, version) = evidence().await;
        let citation = storage
            .add_citation(NewKnowledgeCitation {
                space_id,
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

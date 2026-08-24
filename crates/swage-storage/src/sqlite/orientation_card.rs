use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    NewOrientationCard, OrientationCard, OrientationCardLifecycle, OrientationCardLifecycleKind,
};
use crate::sqlite::SqliteStorage;
use crate::store::{ObservationStore, OrientationCardStore};

fn uuid(value: &str) -> rusqlite::Result<Uuid> {
    value.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn datetime(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn load_evidence(conn: &rusqlite::Connection, card_id: Uuid) -> rusqlite::Result<Vec<Uuid>> {
    let mut statement = conn.prepare("SELECT observation_id FROM orientation_card_observations WHERE card_id = ?1 ORDER BY rowid")?;
    statement
        .query_map(params![card_id.to_string()], |row| {
            uuid(&row.get::<_, String>(0)?)
        })?
        .collect()
}

fn load_card(
    conn: &rusqlite::Connection,
    card_id: Uuid,
) -> rusqlite::Result<Option<OrientationCard>> {
    let row = conn.query_row(
        "SELECT id, space_id, title, content, authored_by, created_at FROM orientation_cards WHERE id = ?1",
        params![card_id.to_string()],
        |row| Ok((uuid(&row.get::<_, String>(0)?)?, uuid(&row.get::<_, String>(1)?)?, row.get(2)?, row.get(3)?, row.get(4)?, datetime(&row.get::<_, String>(5)?)?)),
    ).optional()?;
    row.map(|(id, space_id, title, content, authored_by, created_at)| {
        Ok(OrientationCard {
            id,
            space_id,
            title,
            content,
            authored_by,
            observation_ids: load_evidence(conn, id)?,
            created_at,
        })
    })
    .transpose()
}

#[async_trait]
impl OrientationCardStore for SqliteStorage {
    async fn create_orientation_card(
        &self,
        card: NewOrientationCard,
        session_id: Option<Uuid>,
    ) -> Result<OrientationCard, StorageError> {
        if card.title.trim().is_empty()
            || card.content.trim().is_empty()
            || card.authored_by.trim().is_empty()
        {
            return Err(StorageError::ConstraintViolation(
                "orientation card title, content, and author must not be empty".into(),
            ));
        }
        if card.observation_ids.is_empty() {
            return Err(StorageError::ConstraintViolation(
                "an orientation card needs at least one source observation".into(),
            ));
        }
        let mut observations = Vec::with_capacity(card.observation_ids.len());
        for observation_id in &card.observation_ids {
            let observation = self
                .get_observation(*observation_id)
                .await?
                .ok_or_else(|| StorageError::NotFound {
                    entity: "SourceObservation".into(),
                    id: observation_id.to_string(),
                })?;
            if observation.space_id != card.space_id {
                return Err(StorageError::ConstraintViolation(
                    "all orientation-card evidence must belong to the card's space".into(),
                ));
            }
            if !observations.contains(observation_id) {
                observations.push(*observation_id);
            }
        }
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let id = Uuid::new_v4();
            let anchor_id = Uuid::new_v4();
            let now = Utc::now();
            transaction.execute("INSERT INTO orientation_cards (id, space_id, title, content, authored_by, evidence_anchor_id, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![id.to_string(), card.space_id.to_string(), card.title, card.content, card.authored_by, anchor_id.to_string(), now.to_rfc3339()])?;
            for (index, observation_id) in observations.iter().enumerate() {
                let link_id = if index == 0 { anchor_id } else { Uuid::new_v4() };
                transaction.execute("INSERT INTO orientation_card_observations (id, card_id, space_id, observation_id) VALUES (?1,?2,?3,?4)", params![link_id.to_string(), id.to_string(), card.space_id.to_string(), observation_id.to_string()])?;
            }
            transaction.execute("INSERT INTO audit_events (id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at) VALUES (?1,?2,'orientation_card',?3,'created',?4,?5,?6)", params![Uuid::new_v4().to_string(), session_id.map(|id| id.to_string()), id.to_string(), format!("{} authored an orientation card", card.authored_by), serde_json::json!({"actor": card.authored_by, "observation_ids": observations}).to_string(), now.to_rfc3339()])?;
            transaction.commit()?;
            Ok(OrientationCard { id, space_id: card.space_id, title: card.title, content: card.content, authored_by: card.authored_by, observation_ids: observations, created_at: now })
        }).await
    }

    async fn find_orientation_cards(
        &self,
        space_id: Uuid,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<OrientationCard>, StorageError> {
        let query = query
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        self.with_conn(move |conn| {
            let sql = if query.is_some() {
                "SELECT c.id FROM orientation_card_fts f JOIN orientation_cards c ON c.rowid=f.rowid LEFT JOIN orientation_card_lifecycle l ON l.card_id=c.id WHERE c.space_id=?1 AND l.id IS NULL AND orientation_card_fts MATCH ?2 ORDER BY bm25(orientation_card_fts), c.created_at DESC LIMIT ?3"
            } else {
                "SELECT c.id FROM orientation_cards c LEFT JOIN orientation_card_lifecycle l ON l.card_id=c.id WHERE c.space_id=?1 AND l.id IS NULL ORDER BY c.created_at DESC LIMIT ?3"
            };
            let mut statement = conn.prepare(sql)?;
            let ids = if let Some(query) = query {
                statement.query_map(params![space_id.to_string(), query, limit as i64], |row| uuid(&row.get::<_, String>(0)?))?.collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                statement.query_map(params![space_id.to_string(), rusqlite::types::Null, limit as i64], |row| uuid(&row.get::<_, String>(0)?))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            ids.into_iter().map(|id| load_card(conn, id).map(Option::unwrap)).collect()
        }).await
    }

    async fn supersede_orientation_card(
        &self,
        expected_space_id: Uuid,
        card_id: Uuid,
        replacement_card_id: Uuid,
        actor: String,
        session_id: Option<Uuid>,
    ) -> Result<OrientationCardLifecycle, StorageError> {
        self.append_orientation_lifecycle(
            expected_space_id,
            card_id,
            Some(replacement_card_id),
            actor,
            session_id,
        )
        .await
    }

    async fn retire_orientation_card(
        &self,
        expected_space_id: Uuid,
        card_id: Uuid,
        actor: String,
        session_id: Option<Uuid>,
    ) -> Result<OrientationCardLifecycle, StorageError> {
        self.append_orientation_lifecycle(expected_space_id, card_id, None, actor, session_id)
            .await
    }
}

impl SqliteStorage {
    async fn append_orientation_lifecycle(
        &self,
        expected_space_id: Uuid,
        card_id: Uuid,
        replacement_card_id: Option<Uuid>,
        actor: String,
        session_id: Option<Uuid>,
    ) -> Result<OrientationCardLifecycle, StorageError> {
        if actor.trim().is_empty() {
            return Err(StorageError::ConstraintViolation(
                "orientation-card lifecycle actor must not be empty".into(),
            ));
        }
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let card = load_card(&transaction, card_id)?.ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
            if card.space_id != expected_space_id { return Err(rusqlite::Error::ToSqlConversionFailure("orientation card must belong to the calling space".into())); }
            if transaction.query_row("SELECT 1 FROM orientation_card_lifecycle WHERE card_id=?1", params![card_id.to_string()], |_| Ok(())).optional()?.is_some() {
                return Err(rusqlite::Error::ToSqlConversionFailure("orientation card already has a terminal lifecycle record".into()));
            }
            if let Some(replacement_id) = replacement_card_id {
                let replacement = load_card(&transaction, replacement_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                if replacement.space_id != expected_space_id { return Err(rusqlite::Error::ToSqlConversionFailure("replacement card must belong to the calling space".into())); }
                if transaction.query_row("SELECT 1 FROM orientation_card_lifecycle WHERE card_id=?1", params![replacement_id.to_string()], |_| Ok(())).optional()?.is_some() { return Err(rusqlite::Error::ToSqlConversionFailure("replacement card must be active".into())); }
            }
            let record = OrientationCardLifecycle { id: Uuid::new_v4(), card_id, kind: if replacement_card_id.is_some() { OrientationCardLifecycleKind::Superseded } else { OrientationCardLifecycleKind::Retired }, replacement_card_id, actor, created_at: Utc::now() };
            let action = match record.kind { OrientationCardLifecycleKind::Superseded => "superseded", OrientationCardLifecycleKind::Retired => "retired" };
            transaction.execute("INSERT INTO orientation_card_lifecycle (id,card_id,action,replacement_card_id,actor,created_at) VALUES (?1,?2,?3,?4,?5,?6)", params![record.id.to_string(), card_id.to_string(), action, replacement_card_id.map(|id| id.to_string()), record.actor, record.created_at.to_rfc3339()])?;
            transaction.execute("INSERT INTO audit_events (id,session_id,entity_kind,entity_id,event_kind,summary,metadata_json,created_at) VALUES (?1,?2,'orientation_card',?3,?4,?5,?6,?7)", params![Uuid::new_v4().to_string(), session_id.map(|id| id.to_string()), card_id.to_string(), action, format!("{} {action} an orientation card", record.actor), serde_json::json!({"actor": record.actor, "replacement_card_id": replacement_card_id}).to_string(), record.created_at.to_rfc3339()])?;
            transaction.commit()?;
            Ok(record)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SpaceStore;
    use crate::{NewSourceObservation, NewSpace, ObservationOrigin};

    async fn fixture() -> (SqliteStorage, Uuid, Uuid) {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage
            .create(NewSpace {
                name: "Orientation".into(),
                slug: "orientation".into(),
            })
            .await
            .unwrap();
        let observation = storage
            .record_observation(NewSourceObservation {
                space_id: space.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/evidence".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: Uuid::new_v4().to_string(),
                body_bytes: 30,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        storage
            .add_extraction(
                observation.id,
                "underlying evidence about quokkas".into(),
                "test".into(),
            )
            .await
            .unwrap();
        (storage, space.id, observation.id)
    }

    async fn card(
        storage: &SqliteStorage,
        space_id: Uuid,
        observation_id: Uuid,
        title: &str,
    ) -> OrientationCard {
        storage
            .create_orientation_card(
                NewOrientationCard {
                    space_id,
                    title: title.into(),
                    content: "Marsupial corpus orientation".into(),
                    authored_by: "agent-alpha".into(),
                    observation_ids: vec![observation_id],
                },
                None,
            )
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evidence_is_required_and_must_share_the_space() {
        let (storage, space_id, observation_id) = fixture().await;
        let empty = storage
            .create_orientation_card(
                NewOrientationCard {
                    space_id,
                    title: "Title".into(),
                    content: "Content".into(),
                    authored_by: "agent".into(),
                    observation_ids: vec![],
                },
                None,
            )
            .await;
        assert!(matches!(empty, Err(StorageError::ConstraintViolation(_))));
        let other = storage
            .create(NewSpace {
                name: "Other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();
        let cross_space = storage
            .create_orientation_card(
                NewOrientationCard {
                    space_id: other.id,
                    title: "Title".into(),
                    content: "Content".into(),
                    authored_by: "agent".into(),
                    observation_ids: vec![observation_id],
                },
                None,
            )
            .await;
        assert!(matches!(
            cross_space,
            Err(StorageError::ConstraintViolation(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn schema_binds_an_evidence_anchor_to_its_card_and_space() {
        let (storage, space_id, observation_id) = fixture().await;
        let second_observation = storage
            .record_observation(NewSourceObservation {
                space_id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/second-evidence".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: Uuid::new_v4().to_string(),
                body_bytes: 31,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        let existing = storage
            .create_orientation_card(
                NewOrientationCard {
                    space_id,
                    title: "Existing card".into(),
                    content: "Existing orientation".into(),
                    authored_by: "agent".into(),
                    observation_ids: vec![observation_id, second_observation.id],
                },
                None,
            )
            .await
            .unwrap();
        let unrelated_link_id = storage
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT o.id FROM orientation_card_observations o JOIN orientation_cards c ON c.id=o.card_id WHERE o.card_id=?1 AND o.id<>c.evidence_anchor_id",
                    params![existing.id.to_string()],
                    |row| row.get::<_, String>(0),
                )
            })
            .await
            .unwrap();
        let result = storage.with_conn(move |conn| {
            conn.execute("INSERT INTO orientation_cards (id,space_id,title,content,authored_by,evidence_anchor_id,created_at) VALUES (?1,?2,'title','content','actor',?3,?4)", params![Uuid::new_v4().to_string(), space_id.to_string(), unrelated_link_id, Utc::now().to_rfc3339()])?;
            Ok(())
        }).await;
        assert!(matches!(result, Err(StorageError::ConstraintViolation(_))));

        let other = storage
            .create(NewSpace {
                name: "Other evidence space".into(),
                slug: "other-evidence-space".into(),
            })
            .await
            .unwrap();
        let other_observation = storage
            .record_observation(NewSourceObservation {
                space_id: other.id,
                origin: ObservationOrigin {
                    url: Some("https://example.test/cross-space-evidence".into()),
                    local_path: None,
                },
                retrieved_at: Utc::now(),
                body_sha256: Uuid::new_v4().to_string(),
                body_bytes: 32,
                media_type: "text/plain".into(),
                capture_scope: None,
            })
            .await
            .unwrap();
        let cross_space = storage.with_conn(move |conn| {
            conn.execute("INSERT INTO orientation_card_observations (id,card_id,space_id,observation_id) VALUES (?1,?2,?3,?4)", params![Uuid::new_v4().to_string(), existing.id.to_string(), space_id.to_string(), other_observation.id.to_string()])?;
            Ok(())
        }).await;
        assert!(matches!(
            cross_space,
            Err(StorageError::ConstraintViolation(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trigger_indexes_cards_and_relation_has_no_citation_fields() {
        let (storage, space_id, observation_id) = fixture().await;
        let created = card(&storage, space_id, observation_id, "Quokka field guide").await;
        storage.with_conn(move |conn| {
            let found: String = conn.query_row("SELECT c.id FROM orientation_card_fts f JOIN orientation_cards c ON c.rowid=f.rowid WHERE orientation_card_fts MATCH 'quokka'", [], |row| row.get(0))?;
            assert_eq!(found, created.id.to_string());
            let columns = conn.prepare("PRAGMA table_info(orientation_card_observations)")?.query_map([], |row| row.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(columns, vec!["id", "card_id", "space_id", "observation_id"]);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn orientation_rows_are_append_only() {
        let (storage, space_id, observation_id) = fixture().await;
        let created = card(&storage, space_id, observation_id, "Immutable card").await;
        let lifecycle = storage
            .retire_orientation_card(space_id, created.id, "agent-beta".into(), None)
            .await
            .unwrap();

        storage
            .with_conn(move |conn| {
                let evidence_id: String = conn.query_row(
                    "SELECT id FROM orientation_card_observations WHERE card_id=?1",
                    params![created.id.to_string()],
                    |row| row.get(0),
                )?;
                let card_id = created.id.to_string();
                let lifecycle_id = lifecycle.id.to_string();
                for (statement, id, trigger_message) in [
                    (
                        "UPDATE orientation_cards SET title='rewritten' WHERE id=?1",
                        card_id.as_str(),
                        "orientation cards are immutable",
                    ),
                    (
                        "DELETE FROM orientation_cards WHERE id=?1",
                        card_id.as_str(),
                        "orientation cards are append-only",
                    ),
                    (
                        "UPDATE orientation_card_observations SET observation_id=observation_id WHERE id=?1",
                        evidence_id.as_str(),
                        "orientation card evidence is immutable",
                    ),
                    (
                        "DELETE FROM orientation_card_observations WHERE id=?1",
                        evidence_id.as_str(),
                        "orientation card evidence is append-only",
                    ),
                    (
                        "UPDATE orientation_card_lifecycle SET actor='rewritten' WHERE id=?1",
                        lifecycle_id.as_str(),
                        "orientation card lifecycle is append-only",
                    ),
                    (
                        "DELETE FROM orientation_card_lifecycle WHERE id=?1",
                        lifecycle_id.as_str(),
                        "orientation card lifecycle is append-only",
                    ),
                ] {
                    let error = conn.execute(statement, params![id]).unwrap_err();
                    assert!(
                        error.to_string().contains(trigger_message),
                        "expected {trigger_message:?}, got {error}"
                    );
                }

                let card_title: String = conn.query_row(
                    "SELECT title FROM orientation_cards WHERE id=?1",
                    params![card_id],
                    |row| row.get(0),
                )?;
                let evidence_observation: String = conn.query_row(
                    "SELECT observation_id FROM orientation_card_observations WHERE id=?1",
                    params![evidence_id],
                    |row| row.get(0),
                )?;
                let lifecycle_actor: String = conn.query_row(
                    "SELECT actor FROM orientation_card_lifecycle WHERE id=?1",
                    params![lifecycle_id],
                    |row| row.get(0),
                )?;
                assert_eq!(card_title, "Immutable card");
                assert_eq!(evidence_observation, observation_id.to_string());
                assert_eq!(lifecycle_actor, "agent-beta");
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queryless_and_fts_find_only_active_cards_while_evidence_stays_searchable() {
        let (storage, space_id, observation_id) = fixture().await;
        let first = card(&storage, space_id, observation_id, "Quokka field guide").await;
        let replacement = card(&storage, space_id, observation_id, "Updated quokka guide").await;
        storage
            .supersede_orientation_card(
                space_id,
                first.id,
                replacement.id,
                "agent-beta".into(),
                None,
            )
            .await
            .unwrap();
        let active = storage
            .find_orientation_cards(space_id, None, 20)
            .await
            .unwrap();
        assert_eq!(
            active.iter().map(|card| card.id).collect::<Vec<_>>(),
            vec![replacement.id]
        );
        assert_eq!(
            storage
                .find_orientation_cards(space_id, Some("quokka"), 20)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            storage
                .search_extractions(space_id, "underlying", 10)
                .await
                .unwrap()[0]
                .observation_id,
            observation_id
        );
        storage
            .retire_orientation_card(space_id, replacement.id, "agent-beta".into(), None)
            .await
            .unwrap();
        assert!(
            storage
                .find_orientation_cards(space_id, None, 20)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            storage
                .search_extractions(space_id, "quokkas", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            storage
                .retire_orientation_card(space_id, replacement.id, "agent-beta".into(), None)
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn authorship_and_lifecycle_are_present_in_the_unified_audit() {
        use crate::store::AuditStore;
        let (storage, space_id, observation_id) = fixture().await;
        let created = card(&storage, space_id, observation_id, "Audit card").await;
        storage
            .retire_orientation_card(space_id, created.id, "agent-beta".into(), None)
            .await
            .unwrap();
        let events = storage.list_audit_events().await.unwrap();
        assert_eq!(events.len(), 2);
        let authored = events
            .iter()
            .find(|event| event.event_kind == "created")
            .unwrap();
        let retired = events
            .iter()
            .find(|event| event.event_kind == "retired")
            .unwrap();
        assert_eq!(authored.metadata["actor"], "agent-alpha");
        assert_eq!(retired.metadata["actor"], "agent-beta");
    }
}

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
                transaction.execute("INSERT INTO orientation_card_observations (id, card_id, observation_id) VALUES (?1,?2,?3)", params![link_id.to_string(), id.to_string(), observation_id.to_string()])?;
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
        card_id: Uuid,
        replacement_card_id: Uuid,
        actor: String,
        session_id: Option<Uuid>,
    ) -> Result<OrientationCardLifecycle, StorageError> {
        self.append_orientation_lifecycle(card_id, Some(replacement_card_id), actor, session_id)
            .await
    }

    async fn retire_orientation_card(
        &self,
        card_id: Uuid,
        actor: String,
        session_id: Option<Uuid>,
    ) -> Result<OrientationCardLifecycle, StorageError> {
        self.append_orientation_lifecycle(card_id, None, actor, session_id)
            .await
    }
}

impl SqliteStorage {
    async fn append_orientation_lifecycle(
        &self,
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
            let card = load_card(conn, card_id)?.ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
            if conn.query_row("SELECT 1 FROM orientation_card_lifecycle WHERE card_id=?1", params![card_id.to_string()], |_| Ok(())).optional()?.is_some() {
                return Err(rusqlite::Error::ToSqlConversionFailure("orientation card already has a terminal lifecycle record".into()));
            }
            if let Some(replacement_id) = replacement_card_id {
                let replacement = load_card(conn, replacement_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                if replacement.space_id != card.space_id { return Err(rusqlite::Error::ToSqlConversionFailure("replacement card must belong to the same space".into())); }
                if conn.query_row("SELECT 1 FROM orientation_card_lifecycle WHERE card_id=?1", params![replacement_id.to_string()], |_| Ok(())).optional()?.is_some() { return Err(rusqlite::Error::ToSqlConversionFailure("replacement card must be active".into())); }
            }
            let transaction = conn.unchecked_transaction()?;
            let record = OrientationCardLifecycle { id: Uuid::new_v4(), card_id, kind: if replacement_card_id.is_some() { OrientationCardLifecycleKind::Superseded } else { OrientationCardLifecycleKind::Retired }, replacement_card_id, actor, created_at: Utc::now() };
            let action = match record.kind { OrientationCardLifecycleKind::Superseded => "superseded", OrientationCardLifecycleKind::Retired => "retired" };
            transaction.execute("INSERT INTO orientation_card_lifecycle (id,card_id,action,replacement_card_id,actor,created_at) VALUES (?1,?2,?3,?4,?5,?6)", params![record.id.to_string(), card_id.to_string(), action, replacement_card_id.map(|id| id.to_string()), record.actor, record.created_at.to_rfc3339()])?;
            transaction.execute("INSERT INTO audit_events (id,session_id,entity_kind,entity_id,event_kind,summary,metadata_json,created_at) VALUES (?1,?2,'orientation_card',?3,?4,?5,?6,?7)", params![Uuid::new_v4().to_string(), session_id.map(|id| id.to_string()), card_id.to_string(), action, format!("{} {action} an orientation card", record.actor), serde_json::json!({"actor": record.actor, "replacement_card_id": replacement_card_id}).to_string(), record.created_at.to_rfc3339()])?;
            transaction.commit()?;
            Ok(record)
        }).await
    }
}

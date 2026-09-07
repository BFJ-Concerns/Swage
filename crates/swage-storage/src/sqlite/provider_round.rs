//! SQLite implementation of ProviderRoundStore.
//!
//! One row per provider request inside a turn. The row is written when the
//! request goes out and settled when the round ends, so a request still
//! outstanding is a row with no `completed_at` — the fact that tells a hung
//! provider request from a tool loop that is making progress. A round ends
//! either because a response arrived or because the request became
//! unanswerable; both are ends, and both are stamped.

use async_trait::async_trait;
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::ProviderRoundRecord;
use crate::sqlite::SqliteStorage;
use crate::store::ProviderRoundStore;

const ROUND_COLUMNS: &str = "id, turn_id, round, started_at, completed_at, outcome_json";

fn row_to_round(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderRoundRecord> {
    let id: String = row.get(0)?;
    let turn_id: String = row.get(1)?;
    let started: String = row.get(3)?;
    let completed: Option<String> = row.get(4)?;
    let outcome: String = row.get(5)?;
    let parse_time = |value: String, index: usize| {
        chrono::DateTime::parse_from_rfc3339(&value)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
    };
    let parse_uuid = |value: String, index: usize| {
        Uuid::parse_str(&value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    };
    Ok(ProviderRoundRecord {
        id: parse_uuid(id, 0)?,
        turn_id: parse_uuid(turn_id, 1)?,
        round: row.get(2)?,
        started_at: parse_time(started, 3)?,
        completed_at: completed.map(|value| parse_time(value, 4)).transpose()?,
        outcome: serde_json::from_str(&outcome).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
    })
}

#[async_trait]
impl ProviderRoundStore for SqliteStorage {
    async fn open_provider_round(
        &self,
        turn_id: Uuid,
        round: i64,
        outcome: serde_json::Value,
    ) -> Result<ProviderRoundRecord, StorageError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let outcome = serde_json::to_string(&outcome)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(move |conn| {
            // A replayed round re-opens the same (turn, round) rather than
            // forking the record; the original start time stands.
            conn.execute(
                "INSERT INTO provider_rounds (id, turn_id, round, started_at, outcome_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (turn_id, round) DO NOTHING",
                params![
                    id.to_string(),
                    turn_id.to_string(),
                    round,
                    now.to_rfc3339(),
                    outcome
                ],
            )?;
            conn.query_row(
                &format!(
                    "SELECT {ROUND_COLUMNS} FROM provider_rounds WHERE turn_id=?1 AND round=?2"
                ),
                params![turn_id.to_string(), round],
                row_to_round,
            )
        })
        .await
    }

    async fn settle_provider_round(
        &self,
        turn_id: Uuid,
        round: i64,
        outcome: serde_json::Value,
    ) -> Result<(), StorageError> {
        let now = Utc::now();
        let outcome = serde_json::to_string(&outcome)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE provider_rounds SET completed_at=?3, outcome_json=?4
                 WHERE turn_id=?1 AND round=?2",
                params![turn_id.to_string(), round, now.to_rfc3339(), outcome],
            )?;
            Ok(())
        })
        .await
    }

    async fn interrupt_outstanding_provider_rounds(
        &self,
        turn_id: Uuid,
        outcome: serde_json::Value,
    ) -> Result<(), StorageError> {
        let now = Utc::now();
        let outcome = serde_json::to_string(&outcome)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(move |conn| {
            // The round ends here. No response arrived, but the request
            // stopped being answerable at this moment and that is an
            // observed time, not a guess at when a response would have come:
            // the outcome says which kind of end it was.
            conn.execute(
                "UPDATE provider_rounds SET completed_at=?2, outcome_json=?3
                 WHERE turn_id=?1 AND completed_at IS NULL",
                params![turn_id.to_string(), now.to_rfc3339(), outcome],
            )?;
            Ok(())
        })
        .await
    }

    async fn list_provider_rounds_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ProviderRoundRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ROUND_COLUMNS} FROM provider_rounds WHERE turn_id=?1 ORDER BY round ASC"
            ))?;
            let rows = stmt.query_map(params![turn_id.to_string()], row_to_round)?;
            rows.collect()
        })
        .await
    }
}

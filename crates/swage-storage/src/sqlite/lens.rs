use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;

use swage_core::StorageError;

use crate::model::{AppLens, LensPreference};
use crate::sqlite::SqliteStorage;
use crate::store::LensStore;

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn row_to_lens_preference(row: &rusqlite::Row<'_>) -> rusqlite::Result<LensPreference> {
    let active_lens: String = row.get(0)?;
    let updated_at: String = row.get(1)?;

    Ok(LensPreference {
        active_lens: active_lens.parse::<AppLens>().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            )
        })?,
        updated_at: parse_dt(&updated_at)?,
    })
}

#[async_trait]
impl LensStore for SqliteStorage {
    async fn get_active_lens(&self) -> Result<LensPreference, StorageError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT active_lens, updated_at
                 FROM lens_preferences
                 WHERE singleton_key = 1",
                [],
                row_to_lens_preference,
            )
        })
        .await
    }

    async fn set_active_lens(&self, lens: AppLens) -> Result<LensPreference, StorageError> {
        self.with_conn(|conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();

            conn.execute(
                "INSERT INTO lens_preferences (singleton_key, active_lens, updated_at)
                 VALUES (1, ?1, ?2)
                 ON CONFLICT(singleton_key) DO UPDATE
                 SET active_lens = excluded.active_lens,
                     updated_at = excluded.updated_at",
                params![lens.to_string(), now_str],
            )?;

            Ok(LensPreference {
                active_lens: lens,
                updated_at: now,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::store::LensStore;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn lens_preference_defaults_to_assistant() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");

        let preference = storage.get_active_lens().await.expect("default lens");
        assert_eq!(preference.active_lens, AppLens::Assistant);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lens_preference_round_trips_updates() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");

        let updated = storage
            .set_active_lens(AppLens::Builder)
            .await
            .expect("set builder lens");
        assert_eq!(updated.active_lens, AppLens::Builder);

        let reloaded = storage.get_active_lens().await.expect("reload lens");
        assert_eq!(reloaded.active_lens, AppLens::Builder);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_stored_lens_returns_explicit_error() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");

        storage
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE lens_preferences
                     SET active_lens = 'unknown_lens'
                     WHERE singleton_key = 1",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("corrupt row");

        let error = storage
            .get_active_lens()
            .await
            .expect_err("invalid stored lens should fail");
        let message = error.to_string();
        assert!(
            message.contains("unknown app lens"),
            "expected explicit parse error, got: {message}"
        );
    }
}

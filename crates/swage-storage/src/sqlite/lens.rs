use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{AppLens, LensPreference, LensRecord, LensSourceKind, NewLensRecord};
use crate::sqlite::SqliteStorage;
use crate::store::LensStore;

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn parse_vec(column: usize, value: String) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn serialise_vec(value: &[String]) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| {
        StorageError::ConstraintViolation(format!("failed to serialise lens record: {error}"))
    })
}

fn parse_lens(column: usize, value: String) -> rusqlite::Result<AppLens> {
    value.parse::<AppLens>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })
}

fn parse_source_kind(column: usize, value: String) -> rusqlite::Result<LensSourceKind> {
    value.parse::<LensSourceKind>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })
}

fn row_to_lens_preference(row: &rusqlite::Row<'_>) -> rusqlite::Result<LensPreference> {
    let space_id: String = row.get(0)?;
    let active_lens: String = row.get(1)?;
    let updated_at: String = row.get(2)?;

    Ok(LensPreference {
        space_id: space_id.parse::<Uuid>().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        active_lens: parse_lens(1, active_lens)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

fn row_to_lens_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<LensRecord> {
    let id: String = row.get(0)?;
    let title: String = row.get(1)?;
    let summary: String = row.get(2)?;
    let default_surface: String = row.get(3)?;
    let ui_modules_json: String = row.get(4)?;
    let prominent_teams_json: String = row.get(5)?;
    let source_kind: String = row.get(6)?;
    let created_at: String = row.get(7)?;
    let updated_at: String = row.get(8)?;

    Ok(LensRecord {
        id: parse_lens(0, id)?,
        title,
        summary,
        default_surface,
        ui_modules: parse_vec(4, ui_modules_json)?,
        prominent_team_ids: parse_vec(5, prominent_teams_json)?,
        source_kind: parse_source_kind(6, source_kind)?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

#[async_trait]
impl LensStore for SqliteStorage {
    async fn get_active_lens(&self, space_id: Uuid) -> Result<LensPreference, StorageError> {
        let existing = self
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT space_id, active_lens, updated_at
                     FROM lens_preferences
                     WHERE space_id = ?1",
                    params![space_id.to_string()],
                    row_to_lens_preference,
                )
                .optional()
            })
            .await?;
        match existing {
            Some(preference) => Ok(preference),
            None => self.set_active_lens(space_id, AppLens::assistant()).await,
        }
    }

    async fn set_active_lens(
        &self,
        space_id: Uuid,
        lens: AppLens,
    ) -> Result<LensPreference, StorageError> {
        if self.get_lens(&lens).await?.is_none() {
            return Err(StorageError::NotFound {
                entity: "lens".to_string(),
                id: lens.to_string(),
            });
        }
        self.with_conn(|conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO lens_preferences (space_id, active_lens, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(space_id) DO UPDATE
                 SET active_lens = excluded.active_lens,
                     updated_at = excluded.updated_at",
                params![space_id.to_string(), lens.to_string(), now_str],
            )?;

            Ok(LensPreference {
                space_id,
                active_lens: lens,
                updated_at: now,
            })
        })
        .await
    }

    async fn list_lenses(&self) -> Result<Vec<LensRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, title, summary, default_surface, ui_modules_json,
                        prominent_teams_json, source_kind, created_at, updated_at
                 FROM lenses
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = statement.query_map([], row_to_lens_record)?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
    }

    async fn get_lens(&self, lens: &AppLens) -> Result<Option<LensRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, title, summary, default_surface, ui_modules_json,
                        prominent_teams_json, source_kind, created_at, updated_at
                 FROM lenses
                 WHERE id = ?1",
            )?;
            let mut rows = statement.query(params![lens.to_string()])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            Ok(Some(row_to_lens_record(row)?))
        })
        .await
    }

    async fn create_lens(&self, lens: NewLensRecord) -> Result<LensRecord, StorageError> {
        let ui_modules_json = serialise_vec(&lens.ui_modules)?;
        let prominent_teams_json = serialise_vec(&lens.prominent_team_ids)?;
        self.with_conn(|conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO lenses (
                    id, title, summary, default_surface, ui_modules_json,
                    prominent_teams_json, source_kind, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    lens.id.to_string(),
                    lens.title,
                    lens.summary,
                    lens.default_surface,
                    ui_modules_json,
                    prominent_teams_json,
                    lens.source_kind.to_string(),
                    now_str,
                    now_str
                ],
            )?;

            Ok(LensRecord {
                id: lens.id,
                title: lens.title,
                summary: lens.summary,
                default_surface: lens.default_surface,
                ui_modules: lens.ui_modules,
                prominent_team_ids: lens.prominent_team_ids,
                source_kind: lens.source_kind,
                created_at: now,
                updated_at: now,
            })
        })
        .await
    }

    async fn update_lens(
        &self,
        lens_id: &AppLens,
        title: String,
        summary: String,
        default_surface: String,
        ui_modules: Vec<String>,
        prominent_team_ids: Vec<String>,
    ) -> Result<LensRecord, StorageError> {
        let Some(existing) = self.get_lens(lens_id).await? else {
            return Err(StorageError::NotFound {
                entity: "lens".to_string(),
                id: lens_id.to_string(),
            });
        };
        let ui_modules_json = serialise_vec(&ui_modules)?;
        let prominent_teams_json = serialise_vec(&prominent_team_ids)?;
        self.with_conn(|conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "UPDATE lenses
                 SET title = ?2,
                     summary = ?3,
                     default_surface = ?4,
                     ui_modules_json = ?5,
                     prominent_teams_json = ?6,
                     updated_at = ?7
                 WHERE id = ?1",
                params![
                    lens_id.to_string(),
                    title,
                    summary,
                    default_surface,
                    ui_modules_json,
                    prominent_teams_json,
                    now_str
                ],
            )?;

            Ok(LensRecord {
                id: lens_id.clone(),
                title,
                summary,
                default_surface,
                ui_modules,
                prominent_team_ids,
                source_kind: existing.source_kind,
                created_at: existing.created_at,
                updated_at: now,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{LensStore, SpaceStore};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn lens_preference_defaults_to_assistant() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");
        let space = storage
            .get_or_create_default()
            .await
            .expect("default space");

        let preference = storage
            .get_active_lens(space.id)
            .await
            .expect("default lens");
        assert_eq!(preference.space_id, space.id);
        assert_eq!(preference.active_lens, AppLens::assistant());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn built_in_lenses_exist_in_catalog() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");

        let lenses = storage.list_lenses().await.expect("list lenses");
        assert!(lenses.iter().any(|lens| lens.id == AppLens::assistant()));
        assert!(lenses.iter().any(|lens| lens.id == AppLens::builder()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lens_preference_round_trips_updates() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");
        let space = storage
            .get_or_create_default()
            .await
            .expect("default space");

        let updated = storage
            .set_active_lens(space.id, AppLens::builder())
            .await
            .expect("set builder lens");
        assert_eq!(updated.active_lens, AppLens::builder());

        let reloaded = storage
            .get_active_lens(space.id)
            .await
            .expect("reload lens");
        assert_eq!(reloaded.active_lens, AppLens::builder());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_update_authored_lens_round_trip() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");
        let created = storage
            .create_lens(NewLensRecord {
                id: AppLens::new("research").expect("valid lens id"),
                title: "Research lens".to_string(),
                summary: "Surfacing investigation-oriented modules.".to_string(),
                default_surface: "conversation".to_string(),
                ui_modules: vec!["conversation".to_string(), "operator".to_string()],
                prominent_team_ids: vec!["research-lead".to_string()],
                source_kind: LensSourceKind::Authored,
            })
            .await
            .expect("create lens");
        assert_eq!(created.source_kind, LensSourceKind::Authored);

        let updated = storage
            .update_lens(
                &created.id,
                "Research space".to_string(),
                "Adds a stronger research focus.".to_string(),
                "operator".to_string(),
                vec!["operator".to_string(), "conversation".to_string()],
                vec!["research-lead".to_string(), "reviewer".to_string()],
            )
            .await
            .expect("update lens");
        assert_eq!(updated.title, "Research space");
        assert_eq!(updated.default_surface, "operator");
        assert_eq!(updated.prominent_team_ids.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_stored_lens_returns_explicit_error() {
        let storage = SqliteStorage::in_memory().expect("in-memory db");
        let space = storage
            .get_or_create_default()
            .await
            .expect("default space");
        storage
            .get_active_lens(space.id)
            .await
            .expect("create preference");

        storage
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE lens_preferences
                     SET active_lens = 'unknown_lens!'
                     WHERE space_id = ?1",
                    params![space.id.to_string()],
                )?;
                Ok(())
            })
            .await
            .expect("corrupt row");

        let error = storage
            .get_active_lens(space.id)
            .await
            .expect_err("invalid stored lens should fail");
        let message = error.to_string();
        assert!(
            message.contains("unknown app lens"),
            "expected explicit parse error, got: {message}"
        );
    }
}

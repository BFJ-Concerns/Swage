//! SQLite implementation of the plugin install record: revisions and
//! enablement per scope.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{NewPluginRevision, PluginEnablementRecord, PluginRevisionRecord};
use crate::sqlite::SqliteStorage;
use crate::store::PluginStore;

/// The enablement row's scope for the install itself; a topic's scope is its
/// id.
const INSTALL_SCOPE: &str = "install";

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn parse_json(s: &str) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(s).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

const REVISION_COLUMNS: &str = "id, plugin_name, revision, version, description, manifest, \
     installed_from, install_dir, installed_at, active, provenance";

fn row_to_revision(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginRevisionRecord> {
    let id: String = row.get(0)?;
    let revision: i64 = row.get(2)?;
    let manifest: String = row.get(5)?;
    let installed_at: String = row.get(8)?;
    let active: i64 = row.get(9)?;
    Ok(PluginRevisionRecord {
        id: parse_uuid(&id)?,
        plugin_name: row.get(1)?,
        revision: u32::try_from(revision).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        version: row.get(3)?,
        description: row.get(4)?,
        manifest: parse_json(&manifest)?,
        installed_from: row.get(6)?,
        provenance: row.get(10)?,
        install_dir: row.get(7)?,
        installed_at: parse_dt(&installed_at)?,
        active: active == 1,
    })
}

fn row_to_enablement(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginEnablementRecord> {
    let scope: String = row.get(1)?;
    let enabled: i64 = row.get(2)?;
    let changed_at: String = row.get(3)?;
    Ok(PluginEnablementRecord {
        plugin_name: row.get(0)?,
        topic_id: if scope == INSTALL_SCOPE {
            None
        } else {
            Some(parse_uuid(&scope)?)
        },
        enabled: enabled == 1,
        changed_at: parse_dt(&changed_at)?,
    })
}

fn scope_key(topic_id: Option<Uuid>) -> String {
    topic_id.map_or_else(|| INSTALL_SCOPE.to_owned(), |id| id.to_string())
}

#[async_trait]
impl PluginStore for SqliteStorage {
    async fn record_plugin_revision(
        &self,
        revision: NewPluginRevision,
    ) -> Result<PluginRevisionRecord, StorageError> {
        let id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "UPDATE plugin_revisions SET active = 0 WHERE plugin_name = ?1 AND active = 1",
                params![revision.plugin_name],
            )?;
            transaction.execute(
                "INSERT INTO plugin_revisions (id, plugin_name, revision, version, description, \
                 manifest, installed_from, install_dir, installed_at, active, provenance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)",
                params![
                    id.to_string(),
                    revision.plugin_name,
                    i64::from(revision.revision),
                    revision.version,
                    revision.description,
                    revision.manifest.to_string(),
                    revision.installed_from,
                    revision.install_dir,
                    now,
                    revision.provenance,
                ],
            )?;
            let record = transaction.query_row(
                &format!("SELECT {REVISION_COLUMNS} FROM plugin_revisions WHERE id = ?1"),
                params![id.to_string()],
                row_to_revision,
            )?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    async fn list_plugin_revisions(&self) -> Result<Vec<PluginRevisionRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {REVISION_COLUMNS} FROM plugin_revisions ORDER BY plugin_name, revision"
            ))?;
            statement
                .query_map([], row_to_revision)?
                .collect::<Result<Vec<_>, _>>()
        })
        .await
    }

    async fn set_plugin_enablement(
        &self,
        plugin_name: &str,
        topic_id: Option<Uuid>,
        enabled: bool,
    ) -> Result<PluginEnablementRecord, StorageError> {
        let plugin_name = plugin_name.to_owned();
        let scope = scope_key(topic_id);
        let now = Utc::now().to_rfc3339();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO plugin_enablement (plugin_name, scope, enabled, changed_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (plugin_name, scope)
                 DO UPDATE SET enabled = excluded.enabled, changed_at = excluded.changed_at",
                params![plugin_name, scope, i64::from(enabled), now],
            )?;
            conn.query_row(
                "SELECT plugin_name, scope, enabled, changed_at FROM plugin_enablement
                 WHERE plugin_name = ?1 AND scope = ?2",
                params![plugin_name, scope],
                row_to_enablement,
            )
            .optional()?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)
        })
        .await
    }

    async fn list_plugin_enablement(&self) -> Result<Vec<PluginEnablementRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT plugin_name, scope, enabled, changed_at FROM plugin_enablement
                 ORDER BY plugin_name, scope <> 'install', scope",
            )?;
            statement
                .query_map([], row_to_enablement)?
                .collect::<Result<Vec<_>, _>>()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(name: &str, revision: u32) -> NewPluginRevision {
        NewPluginRevision {
            plugin_name: name.into(),
            revision,
            version: format!("1.{revision}"),
            description: "A plugin.".into(),
            manifest: serde_json::json!({ "name": name, "provides": { "tools": [] } }),
            installed_from: "/src/plugin".into(),
            provenance: "installed".into(),
            install_dir: format!("/root/plugins/{name}/{revision}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_revision_becomes_the_active_one_and_earlier_ones_stay_recorded() {
        let storage = SqliteStorage::in_memory().unwrap();
        let first = storage
            .record_plugin_revision(revision("echo", 1))
            .await
            .unwrap();
        assert!(first.active);
        assert_eq!(first.revision, 1);
        let second = storage
            .record_plugin_revision(revision("echo", 2))
            .await
            .unwrap();
        assert!(second.active);
        storage
            .record_plugin_revision(revision("other", 1))
            .await
            .unwrap();

        let listed = storage.list_plugin_revisions().await.unwrap();
        let names: Vec<(String, u32, bool)> = listed
            .iter()
            .map(|record| (record.plugin_name.clone(), record.revision, record.active))
            .collect();
        assert_eq!(
            names,
            vec![
                ("echo".into(), 1, false),
                ("echo".into(), 2, true),
                ("other".into(), 1, true),
            ]
        );
        assert_eq!(listed[1].manifest["name"], "echo");
        assert_eq!(listed[1].install_dir, "/root/plugins/echo/2");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_repeated_revision_number_is_refused() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .record_plugin_revision(revision("echo", 1))
            .await
            .unwrap();
        let error = storage
            .record_plugin_revision(revision("echo", 1))
            .await
            .expect_err("refused");
        assert!(
            error.to_string().contains("UNIQUE") || error.to_string().contains("constraint"),
            "{error}"
        );
        let listed = storage.list_plugin_revisions().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            listed[0].active,
            "the refused write left the active row in place"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enablement_is_one_answer_per_scope_replaced_in_place() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = Uuid::new_v4();
        let install = storage
            .set_plugin_enablement("echo", None, true)
            .await
            .unwrap();
        assert_eq!(install.topic_id, None);
        assert!(install.enabled);
        storage
            .set_plugin_enablement("echo", Some(topic), true)
            .await
            .unwrap();
        let replaced = storage
            .set_plugin_enablement("echo", Some(topic), false)
            .await
            .unwrap();
        assert_eq!(replaced.topic_id, Some(topic));
        assert!(!replaced.enabled);

        let listed = storage.list_plugin_enablement().await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].topic_id, None);
        assert_eq!(listed[1].topic_id, Some(topic));
        assert!(!listed[1].enabled);
    }
}

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{ChannelLink, NewChannelLink};
use crate::sqlite::SqliteStorage;
use crate::store::ChannelLinkStore;

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

fn parse_json(json: Option<&str>) -> rusqlite::Result<Option<serde_json::Value>> {
    json.map(|value| {
        serde_json::from_str(value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    })
    .transpose()
}

fn serialise_json(value: &Option<serde_json::Value>) -> Result<Option<String>, rusqlite::Error> {
    value
        .as_ref()
        .map(|value| {
            serde_json::to_string(value)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
        })
        .transpose()
}

fn row_to_channel_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelLink> {
    let project_id: String = row.get(3)?;
    let session_id: Option<String> = row.get(5)?;
    let linked_at: String = row.get(7)?;
    let updated_at: String = row.get(8)?;

    Ok(ChannelLink {
        integration: row.get(0)?,
        account: row.get(1)?,
        conversation: row.get(2)?,
        project_id: parse_uuid(&project_id)?,
        agent_id: row.get(4)?,
        session_id: session_id.as_deref().map(parse_uuid).transpose()?,
        home_target: parse_json(row.get::<_, Option<String>>(6)?.as_deref())?,
        linked_at: parse_dt(&linked_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

#[async_trait]
impl ChannelLinkStore for SqliteStorage {
    async fn upsert_channel_link(&self, link: NewChannelLink) -> Result<ChannelLink, StorageError> {
        let home_target_json = serialise_json(&link.home_target)
            .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        self.with_conn(move |conn| {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            conn.execute(
                "INSERT INTO channel_links
                 (integration, account, conversation, project_id, agent_id, session_id, home_target_json, linked_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                 ON CONFLICT(integration, account, conversation) DO UPDATE SET
                     project_id = excluded.project_id,
                     agent_id = excluded.agent_id,
                     session_id = excluded.session_id,
                     home_target_json = excluded.home_target_json,
                     updated_at = excluded.updated_at",
                params![
                    &link.integration,
                    &link.account,
                    &link.conversation,
                    link.project_id.to_string(),
                    &link.agent_id,
                    link.session_id.map(|value| value.to_string()),
                    &home_target_json,
                    &now_str,
                ],
            )?;

            let linked_at: String = conn.query_row(
                "SELECT linked_at FROM channel_links
                 WHERE integration = ?1 AND account = ?2 AND conversation = ?3",
                params![&link.integration, &link.account, &link.conversation],
                |row| row.get(0),
            )?;

            Ok(ChannelLink {
                integration: link.integration,
                account: link.account,
                conversation: link.conversation,
                project_id: link.project_id,
                agent_id: link.agent_id,
                session_id: link.session_id,
                home_target: link.home_target,
                linked_at: parse_dt(&linked_at)?,
                updated_at: now,
            })
        })
        .await
    }

    async fn get_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<ChannelLink>, StorageError> {
        let integration = integration.to_string();
        let account = account.to_string();
        let conversation = conversation.to_string();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT integration, account, conversation, project_id, agent_id, session_id, home_target_json, linked_at, updated_at
                 FROM channel_links
                 WHERE integration = ?1 AND account = ?2 AND conversation = ?3",
                params![integration, account, conversation],
                row_to_channel_link,
            )
            .optional()
        })
        .await
    }

    async fn list_channel_links(&self) -> Result<Vec<ChannelLink>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT integration, account, conversation, project_id, agent_id, session_id, home_target_json, linked_at, updated_at
                 FROM channel_links
                 ORDER BY linked_at ASC, integration ASC, account ASC, conversation ASC",
            )?;
            let rows = stmt.query_map([], row_to_channel_link)?;
            rows.collect()
        })
        .await
    }

    async fn delete_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<bool, StorageError> {
        let integration = integration.to_string();
        let account = account.to_string();
        let conversation = conversation.to_string();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM channel_links
                 WHERE integration = ?1 AND account = ?2 AND conversation = ?3",
                params![integration, account, conversation],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::model::NewProject;
    use crate::model::NewSession;
    use crate::store::{ChannelLinkStore, ProjectStore, SessionStore};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn channel_link_round_trips_and_updates_bound_session() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage
            .create(NewProject {
                name: "channel-project".into(),
                slug: "channel-project".into(),
                workspace_path: None,
            })
            .await
            .unwrap();
        let first = storage
            .upsert_channel_link(NewChannelLink {
                integration: "dev_dm".into(),
                account: "workspace-a".into(),
                conversation: "alice".into(),
                project_id: project.id,
                agent_id: "gateway".into(),
                session_id: None,
                home_target: Some(serde_json::json!({
                    "kind": "local_storage"
                })),
            })
            .await
            .unwrap();
        assert_eq!(first.session_id, None);

        let session_id = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".into(),
                metadata: None,
            })
            .await
            .unwrap()
            .id;
        let updated = storage
            .upsert_channel_link(NewChannelLink {
                integration: "dev_dm".into(),
                account: "workspace-a".into(),
                conversation: "alice".into(),
                project_id: project.id,
                agent_id: "gateway".into(),
                session_id: Some(session_id),
                home_target: Some(serde_json::json!({
                    "kind": "local_storage"
                })),
            })
            .await
            .unwrap();
        assert_eq!(updated.session_id, Some(session_id));

        let fetched = storage
            .get_channel_link("dev_dm", "workspace-a", "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.session_id, Some(session_id));
        assert_eq!(fetched.linked_at, first.linked_at);
    }
}

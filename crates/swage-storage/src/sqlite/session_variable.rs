use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::sqlite::SqliteStorage;
use crate::{SessionVariable, SessionVariableStore};

fn parse_dt(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionVariable> {
    let session_id: String = row.get(0)?;
    Ok(SessionVariable {
        session_id: session_id.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        name: row.get(1)?,
        value: row.get(2)?,
        created_at: parse_dt(row.get(3)?)?,
        updated_at: parse_dt(row.get(4)?)?,
    })
}

#[async_trait]
impl SessionVariableStore for SqliteStorage {
    async fn set_session_variable(
        &self,
        session_id: Uuid,
        name: &str,
        value: &str,
    ) -> Result<SessionVariable, StorageError> {
        let name = name.to_owned();
        let value = value.to_owned();
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute("INSERT INTO session_variables (session_id, name, value, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4) ON CONFLICT(session_id, name) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", params![session_id.to_string(), name, value, now])?;
            conn.query_row("SELECT session_id, name, value, created_at, updated_at FROM session_variables WHERE session_id = ?1 AND name = ?2", params![session_id.to_string(), name], row)
        }).await
    }

    async fn list_session_variables(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<SessionVariable>, StorageError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare("SELECT session_id, name, value, created_at, updated_at FROM session_variables WHERE session_id = ?1 ORDER BY name")?;
            statement.query_map([session_id.to_string()], row)?.collect()
        }).await
    }

    async fn delete_session_variable(
        &self,
        session_id: Uuid,
        name: &str,
    ) -> Result<bool, StorageError> {
        let name = name.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM session_variables WHERE session_id = ?1 AND name = ?2",
                params![session_id.to_string(), name],
            )? > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewSession, NewSpace, SessionStore, SpaceSlug, SpaceStore};

    async fn session(storage: &SqliteStorage, slug: &str) -> Uuid {
        let space = storage
            .create(NewSpace {
                name: slug.into(),
                slug: SpaceSlug::new(slug).unwrap(),
            })
            .await
            .unwrap();
        storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "lead".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn variables_are_isolated_replaced_sorted_and_removed() {
        let storage = SqliteStorage::in_memory().unwrap();
        let first = session(&storage, "first").await;
        let second = session(&storage, "second").await;
        storage
            .set_session_variable(first, "zeta", "old")
            .await
            .unwrap();
        storage
            .set_session_variable(first, "alpha", "one")
            .await
            .unwrap();
        storage
            .set_session_variable(first, "zeta", "new")
            .await
            .unwrap();
        storage
            .set_session_variable(second, "zeta", "other")
            .await
            .unwrap();

        let variables = storage.list_session_variables(first).await.unwrap();
        assert_eq!(
            variables
                .iter()
                .map(|item| (item.name.as_str(), item.value.as_str()))
                .collect::<Vec<_>>(),
            vec![("alpha", "one"), ("zeta", "new")]
        );
        assert!(
            storage
                .delete_session_variable(first, "alpha")
                .await
                .unwrap()
        );
        assert!(
            !storage
                .delete_session_variable(first, "missing")
                .await
                .unwrap()
        );
        assert_eq!(
            storage.list_session_variables(second).await.unwrap()[0].value,
            "other"
        );
    }
}

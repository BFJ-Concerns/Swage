use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use swage_core::StorageError;

use super::SqliteStorage;
use crate::{NewTaskDefinition, TaskDefinition, TaskDefinitionStore};

const COLUMNS: &str = "id, instructions, model_id, tool_permissions_json, output_schema_json, output_max_retries, created_at, updated_at";

fn json(value: Option<&serde_json::Value>) -> Result<Option<String>, StorageError> {
    value
        .map(|v| {
            serde_json::to_string(v).map_err(|e| StorageError::Serialisation {
                message: e.to_string(),
            })
        })
        .transpose()
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskDefinition> {
    let permissions: Option<String> = row.get(3)?;
    let schema: Option<String> = row.get(4)?;
    let parse_json = |index, value: String| {
        serde_json::from_str(&value).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
    };
    let parse_dt = |index, value: String| {
        DateTime::parse_from_rfc3339(&value)
            .map(|v| v.with_timezone(&Utc))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
    };
    Ok(TaskDefinition {
        id: row.get(0)?,
        instructions: row.get(1)?,
        model_id: row.get(2)?,
        tool_permissions: permissions.map(|v| parse_json(3, v)).transpose()?,
        output_schema: schema.map(|v| parse_json(4, v)).transpose()?,
        output_max_retries: row.get(5)?,
        created_at: parse_dt(6, row.get(6)?)?,
        updated_at: parse_dt(7, row.get(7)?)?,
    })
}

#[async_trait]
impl TaskDefinitionStore for SqliteStorage {
    async fn upsert_task_definition(
        &self,
        definition: NewTaskDefinition,
    ) -> Result<TaskDefinition, StorageError> {
        let permissions = json(definition.tool_permissions.as_ref())?;
        let schema = json(definition.output_schema.as_ref())?;
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            conn.execute("INSERT INTO task_definitions (id,instructions,model_id,tool_permissions_json,output_schema_json,output_max_retries,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7) ON CONFLICT(id) DO UPDATE SET instructions=excluded.instructions,model_id=excluded.model_id,tool_permissions_json=excluded.tool_permissions_json,output_schema_json=excluded.output_schema_json,output_max_retries=excluded.output_max_retries,updated_at=excluded.updated_at", params![definition.id, definition.instructions, definition.model_id, permissions, schema, definition.output_max_retries, now])?;
            conn.query_row(&format!("SELECT {COLUMNS} FROM task_definitions WHERE id=?1"), params![definition.id], row)
        }).await
    }

    async fn get_task_definition(&self, id: &str) -> Result<Option<TaskDefinition>, StorageError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM task_definitions WHERE id=?1"),
                params![id],
                row,
            )
            .optional()
        })
        .await
    }

    async fn list_task_definitions(&self) -> Result<Vec<TaskDefinition>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM task_definitions ORDER BY id"
            ))?;
            stmt.query_map([], row)?.collect()
        })
        .await
    }

    async fn delete_task_definition(&self, id: &str) -> Result<bool, StorageError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute("DELETE FROM task_definitions WHERE id=?1", params![id])? > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread")]
    async fn crud_re_resolves_whole_definition() {
        let store = SqliteStorage::in_memory().unwrap();
        let mut input = NewTaskDefinition {
            id: "review".into(),
            instructions: "first".into(),
            model_id: None,
            tool_permissions: None,
            output_schema: None,
            output_max_retries: None,
        };
        store.upsert_task_definition(input.clone()).await.unwrap();
        input.instructions = "edited".into();
        store.upsert_task_definition(input).await.unwrap();
        assert_eq!(
            store
                .get_task_definition("review")
                .await
                .unwrap()
                .unwrap()
                .instructions,
            "edited"
        );
        assert!(store.delete_task_definition("review").await.unwrap());
        assert!(store.get_task_definition("review").await.unwrap().is_none());
    }
}

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{AgentDefinition, NewAgentDefinition};
use crate::sqlite::SqliteStorage;
use crate::store::AgentDefinitionStore;

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

const DEFINITION_COLUMNS: &str = "id, space_id, display_name, description, instructions,
     model_id, tool_permissions_json, created_at, updated_at";

fn row_to_definition(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentDefinition> {
    let space_id: Option<String> = row.get(1)?;
    let tool_permissions: Option<String> = row.get(6)?;
    Ok(AgentDefinition {
        id: row.get(0)?,
        space_id: space_id.map(|s| parse_uuid(&s)).transpose()?,
        display_name: row.get(2)?,
        description: row.get(3)?,
        instructions: row.get(4)?,
        model_id: row.get(5)?,
        tool_permissions: tool_permissions
            .map(|json| {
                serde_json::from_str(&json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        created_at: parse_dt(&row.get::<_, String>(7)?)?,
        updated_at: parse_dt(&row.get::<_, String>(8)?)?,
    })
}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn serialise_permissions(
    permissions: Option<&serde_json::Value>,
) -> Result<Option<String>, StorageError> {
    permissions
        .map(|value| {
            serde_json::to_string(value).map_err(|error| StorageError::Serialisation {
                message: error.to_string(),
            })
        })
        .transpose()
}

// ---------------------------------------------------------------------------
// AgentDefinitionStore
// ---------------------------------------------------------------------------

#[async_trait]
impl AgentDefinitionStore for SqliteStorage {
    async fn upsert_agent_definition(
        &self,
        definition: NewAgentDefinition,
    ) -> Result<AgentDefinition, StorageError> {
        let permissions_json = serialise_permissions(definition.tool_permissions.as_ref())?;
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            // SQLite treats NULLs as distinct in the composite primary key, so
            // global definitions rely on the partial unique index; upserting
            // through both constraints needs the two-step form.
            let existing: Option<String> = conn
                .query_row(
                    "SELECT created_at FROM agent_definitions
                     WHERE id = ?1 AND space_id IS ?2",
                    params![
                        definition.id,
                        definition.space_id.map(|id| id.to_string())
                    ],
                    |row| row.get(0),
                )
                .optional()?;

            match existing {
                Some(created_at) => {
                    conn.execute(
                        "UPDATE agent_definitions
                         SET display_name = ?3, description = ?4, instructions = ?5,
                             model_id = ?6, tool_permissions_json = ?7, updated_at = ?8
                         WHERE id = ?1 AND space_id IS ?2",
                        params![
                            definition.id,
                            definition.space_id.map(|id| id.to_string()),
                            definition.display_name,
                            definition.description,
                            definition.instructions,
                            definition.model_id,
                            permissions_json,
                            now,
                        ],
                    )?;
                    let _ = created_at;
                }
                None => {
                    conn.execute(
                        "INSERT INTO agent_definitions (
                             id, space_id, display_name, description, instructions,
                             model_id, tool_permissions_json, created_at, updated_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                        params![
                            definition.id,
                            definition.space_id.map(|id| id.to_string()),
                            definition.display_name,
                            definition.description,
                            definition.instructions,
                            definition.model_id,
                            permissions_json,
                            now,
                        ],
                    )?;
                }
            }

            conn.query_row(
                &format!(
                    "SELECT {DEFINITION_COLUMNS} FROM agent_definitions
                     WHERE id = ?1 AND space_id IS ?2"
                ),
                params![
                    definition.id,
                    definition.space_id.map(|id| id.to_string())
                ],
                row_to_definition,
            )
        })
        .await
    }

    async fn resolve_agent_definition(
        &self,
        agent_id: &str,
        space_id: Uuid,
    ) -> Result<Option<AgentDefinition>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            // Space-scoped shadows global: order puts the space row first and
            // takes one.
            conn.query_row(
                &format!(
                    "SELECT {DEFINITION_COLUMNS} FROM agent_definitions
                     WHERE id = ?1 AND (space_id = ?2 OR space_id IS NULL)
                     ORDER BY space_id IS NULL
                     LIMIT 1"
                ),
                params![agent_id, space_id.to_string()],
                row_to_definition,
            )
            .optional()
        })
        .await
    }

    async fn list_agent_definitions(
        &self,
        space_id: Option<Uuid>,
    ) -> Result<Vec<AgentDefinition>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {DEFINITION_COLUMNS} FROM agent_definitions
                 WHERE space_id IS ?1 ORDER BY id"
            ))?;
            let rows = stmt.query_map(
                params![space_id.map(|id| id.to_string())],
                row_to_definition,
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn delete_agent_definition(
        &self,
        agent_id: &str,
        space_id: Option<Uuid>,
    ) -> Result<bool, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM agent_definitions WHERE id = ?1 AND space_id IS ?2",
                params![agent_id, space_id.map(|id| id.to_string())],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NewSpace;
    use crate::store::SpaceStore;

    fn definition(id: &str, space_id: Option<Uuid>, instructions: &str) -> NewAgentDefinition {
        NewAgentDefinition {
            id: id.into(),
            space_id,
            display_name: id.into(),
            description: String::new(),
            instructions: instructions.into(),
            model_id: None,
            tool_permissions: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_creates_then_updates_in_place() {
        let storage = SqliteStorage::in_memory().unwrap();

        let created = storage
            .upsert_agent_definition(definition("researcher", None, "gather evidence"))
            .await
            .unwrap();
        assert_eq!(created.instructions, "gather evidence");

        let updated = storage
            .upsert_agent_definition(definition("researcher", None, "gather and cite evidence"))
            .await
            .unwrap();
        assert_eq!(updated.instructions, "gather and cite evidence");
        assert_eq!(
            created.created_at, updated.created_at,
            "an update keeps the creation time"
        );

        let all = storage.list_agent_definitions(None).await.unwrap();
        assert_eq!(all.len(), 1, "upsert must not duplicate");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn space_scoped_definition_shadows_global() {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage
            .create(NewSpace {
                name: "research".into(),
                slug: "research".into(),
            })
            .await
            .unwrap();

        storage
            .upsert_agent_definition(definition("assistant", None, "global character"))
            .await
            .unwrap();
        storage
            .upsert_agent_definition(definition(
                "assistant",
                Some(space.id),
                "space character",
            ))
            .await
            .unwrap();

        let resolved = storage
            .resolve_agent_definition("assistant", space.id)
            .await
            .unwrap()
            .expect("should resolve");
        assert_eq!(resolved.instructions, "space character");

        // A different space falls through to the global definition.
        let other = storage
            .create(NewSpace {
                name: "other".into(),
                slug: "other".into(),
            })
            .await
            .unwrap();
        let fallback = storage
            .resolve_agent_definition("assistant", other.id)
            .await
            .unwrap()
            .expect("global should apply");
        assert_eq!(fallback.instructions, "global character");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_agent_resolves_to_none() {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage.get_or_create_default().await.unwrap();

        let resolved = storage
            .resolve_agent_definition("nobody", space.id)
            .await
            .unwrap();
        assert!(resolved.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn permissions_overlay_round_trips() {
        let storage = SqliteStorage::in_memory().unwrap();
        let mut new = definition("scoped", None, "");
        new.tool_permissions = Some(serde_json::json!({
            "allow": ["knowledge_search"],
            "deny": ["shell"],
            "bundles": {}
        }));

        let stored = storage.upsert_agent_definition(new).await.unwrap();
        let overlay = stored.tool_permissions.expect("overlay stored");
        assert_eq!(overlay["allow"][0], "knowledge_search");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_removes_only_the_addressed_definition() {
        let storage = SqliteStorage::in_memory().unwrap();
        let space = storage.get_or_create_default().await.unwrap();

        storage
            .upsert_agent_definition(definition("assistant", None, "global"))
            .await
            .unwrap();
        storage
            .upsert_agent_definition(definition("assistant", Some(space.id), "scoped"))
            .await
            .unwrap();

        let deleted = storage
            .delete_agent_definition("assistant", Some(space.id))
            .await
            .unwrap();
        assert!(deleted);

        let resolved = storage
            .resolve_agent_definition("assistant", space.id)
            .await
            .unwrap()
            .expect("global survives");
        assert_eq!(resolved.instructions, "global");
    }
}

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

const DEFINITION_COLUMNS: &str = "id, topic_id, display_name, description, instructions,
     model_id, tool_permissions_json, skills_json, output_schema_json,
     output_max_retries, created_at, updated_at";

fn row_to_definition(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentDefinition> {
    let topic_id: Option<String> = row.get(1)?;
    let tool_permissions: Option<String> = row.get(6)?;
    let skills: String = row.get(7)?;
    let output_schema: Option<String> = row.get(8)?;
    Ok(AgentDefinition {
        id: row.get(0)?,
        topic_id: topic_id.map(|s| parse_uuid(&s)).transpose()?,
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
        skills: serde_json::from_str(&skills).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        output_schema: output_schema
            .map(|json| {
                serde_json::from_str(&json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        output_max_retries: row.get(9)?,
        created_at: parse_dt(&row.get::<_, String>(10)?)?,
        updated_at: parse_dt(&row.get::<_, String>(11)?)?,
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
        let skills_json = serde_json::to_string(&definition.skills).map_err(|error| {
            StorageError::Serialisation {
                message: error.to_string(),
            }
        })?;
        let output_schema_json = serialise_permissions(definition.output_schema.as_ref())?;
        self.with_conn(move |conn| {
            let now = Utc::now().to_rfc3339();
            // SQLite treats NULLs as distinct in the composite primary key, so
            // global definitions rely on the partial unique index; upserting
            // through both constraints needs the two-step form.
            let existing: Option<String> = conn
                .query_row(
                    "SELECT created_at FROM agent_definitions
                     WHERE id = ?1 AND topic_id IS ?2",
                    params![definition.id, definition.topic_id.map(|id| id.to_string())],
                    |row| row.get(0),
                )
                .optional()?;

            match existing {
                Some(created_at) => {
                    conn.execute(
                        "UPDATE agent_definitions
                         SET display_name = ?3, description = ?4, instructions = ?5,
                             model_id = ?6, tool_permissions_json = ?7, skills_json = ?8,
                             output_schema_json = ?9, output_max_retries = ?10, updated_at = ?11
                         WHERE id = ?1 AND topic_id IS ?2",
                        params![
                            definition.id,
                            definition.topic_id.map(|id| id.to_string()),
                            definition.display_name,
                            definition.description,
                            definition.instructions,
                            definition.model_id,
                            permissions_json,
                            skills_json,
                            output_schema_json,
                            definition.output_max_retries,
                            now,
                        ],
                    )?;
                    let _ = created_at;
                }
                None => {
                    conn.execute(
                        "INSERT INTO agent_definitions (
                             id, topic_id, display_name, description, instructions,
                             model_id, tool_permissions_json, skills_json, output_schema_json,
                             output_max_retries, created_at, updated_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                        params![
                            definition.id,
                            definition.topic_id.map(|id| id.to_string()),
                            definition.display_name,
                            definition.description,
                            definition.instructions,
                            definition.model_id,
                            permissions_json,
                            skills_json,
                            output_schema_json,
                            definition.output_max_retries,
                            now,
                        ],
                    )?;
                }
            }

            conn.query_row(
                &format!(
                    "SELECT {DEFINITION_COLUMNS} FROM agent_definitions
                     WHERE id = ?1 AND topic_id IS ?2"
                ),
                params![definition.id, definition.topic_id.map(|id| id.to_string())],
                row_to_definition,
            )
        })
        .await
    }

    async fn resolve_agent_definition(
        &self,
        agent_id: &str,
        topic_id: Uuid,
    ) -> Result<Option<AgentDefinition>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            // Topic-scoped shadows global: order puts the topic row first and
            // takes one.
            conn.query_row(
                &format!(
                    "SELECT {DEFINITION_COLUMNS} FROM agent_definitions
                     WHERE id = ?1 AND (topic_id = ?2 OR topic_id IS NULL)
                     ORDER BY topic_id IS NULL
                     LIMIT 1"
                ),
                params![agent_id, topic_id.to_string()],
                row_to_definition,
            )
            .optional()
        })
        .await
    }

    async fn list_agent_definitions(
        &self,
        topic_id: Option<Uuid>,
    ) -> Result<Vec<AgentDefinition>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {DEFINITION_COLUMNS} FROM agent_definitions
                 WHERE topic_id IS ?1 ORDER BY id"
            ))?;
            let rows = stmt.query_map(
                params![topic_id.map(|id| id.to_string())],
                row_to_definition,
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
    }

    async fn delete_agent_definition(
        &self,
        agent_id: &str,
        topic_id: Option<Uuid>,
    ) -> Result<bool, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM agent_definitions WHERE id = ?1 AND topic_id IS ?2",
                params![agent_id, topic_id.map(|id| id.to_string())],
            )?;
            Ok(affected > 0)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NewTopic;
    use crate::store::TopicStore;

    fn definition(id: &str, topic_id: Option<Uuid>, instructions: &str) -> NewAgentDefinition {
        NewAgentDefinition {
            id: id.into(),
            topic_id,
            display_name: id.into(),
            description: String::new(),
            instructions: instructions.into(),
            model_id: None,
            tool_permissions: None,
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
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
    async fn topic_scoped_definition_shadows_global() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage
            .create(NewTopic {
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
            .upsert_agent_definition(definition("assistant", Some(topic.id), "topic character"))
            .await
            .unwrap();

        let resolved = storage
            .resolve_agent_definition("assistant", topic.id)
            .await
            .unwrap()
            .expect("should resolve");
        assert_eq!(resolved.instructions, "topic character");

        // A different topic falls through to the global definition.
        let other = storage
            .create(NewTopic {
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
        let topic = storage.get_or_create_default().await.unwrap();

        let resolved = storage
            .resolve_agent_definition("nobody", topic.id)
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
    async fn skills_and_output_contract_round_trip() {
        let storage = SqliteStorage::in_memory().unwrap();
        let mut new = definition("contracted", None, "return a finding");
        new.skills = vec!["source-checking".into(), "citation-format".into()];
        new.output_schema = Some(serde_json::json!({
            "type": "object",
            "required": ["finding"]
        }));
        new.output_max_retries = Some(1);

        let stored = storage.upsert_agent_definition(new).await.unwrap();
        assert_eq!(stored.skills, ["source-checking", "citation-format"]);
        assert_eq!(stored.output_schema.unwrap()["type"], "object");
        assert_eq!(stored.output_max_retries, Some(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_removes_only_the_addressed_definition() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage.get_or_create_default().await.unwrap();

        storage
            .upsert_agent_definition(definition("assistant", None, "global"))
            .await
            .unwrap();
        storage
            .upsert_agent_definition(definition("assistant", Some(topic.id), "scoped"))
            .await
            .unwrap();

        let deleted = storage
            .delete_agent_definition("assistant", Some(topic.id))
            .await
            .unwrap();
        assert!(deleted);

        let resolved = storage
            .resolve_agent_definition("assistant", topic.id)
            .await
            .unwrap()
            .expect("global survives");
        assert_eq!(resolved.instructions, "global");
    }
}

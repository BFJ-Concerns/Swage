use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{AgentMemoryRecord, NewAgentMemoryRecord};
use crate::sqlite::SqliteStorage;
use crate::store::AgentMemoryStore;

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_optional_uuid(value: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    value.as_deref().map(parse_uuid).transpose()
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

fn row_to_agent_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMemoryRecord> {
    let topology_run_id: Option<String> = row.get(4)?;
    let node_execution_id: Option<String> = row.get(5)?;
    let tags_json: String = row.get(10)?;
    let metadata_json: String = row.get(13)?;
    let created_at: String = row.get(14)?;
    let updated_at: String = row.get(15)?;

    Ok(AgentMemoryRecord {
        id: parse_uuid(&row.get::<_, String>(0)?)?,
        project_id: parse_uuid(&row.get::<_, String>(1)?)?,
        session_id: parse_uuid(&row.get::<_, String>(2)?)?,
        turn_id: parse_uuid(&row.get::<_, String>(3)?)?,
        topology_run_id: parse_optional_uuid(topology_run_id)?,
        node_execution_id: parse_optional_uuid(node_execution_id)?,
        agent_id: row.get(6)?,
        memory_kind: row.get(7)?,
        title: row.get(8)?,
        content: row.get(9)?,
        tags: serde_json::from_str(&tags_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        outcome_status: row.get(11)?,
        source_summary: row.get(12)?,
        metadata: serde_json::from_str(&metadata_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                13,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

#[async_trait]
impl AgentMemoryStore for SqliteStorage {
    async fn create_agent_memory(
        &self,
        record: NewAgentMemoryRecord,
    ) -> Result<AgentMemoryRecord, StorageError> {
        self.with_conn(move |conn| {
            let created_at = Utc::now();
            let timestamp = created_at.to_rfc3339();
            let tags_json = serde_json::to_string(&record.tags)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let metadata_json = serde_json::to_string(&record.metadata)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            conn.execute(
                "INSERT INTO agent_memories (
                    id, project_id, session_id, turn_id, topology_run_id, node_execution_id,
                    agent_id, memory_kind, title, content, tags_json, outcome_status,
                    source_summary, metadata_json, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    record.id.to_string(),
                    record.project_id.to_string(),
                    record.session_id.to_string(),
                    record.turn_id.to_string(),
                    record.topology_run_id.map(|id| id.to_string()),
                    record.node_execution_id.map(|id| id.to_string()),
                    record.agent_id,
                    record.memory_kind,
                    record.title,
                    record.content,
                    tags_json,
                    record.outcome_status,
                    record.source_summary,
                    metadata_json,
                    timestamp,
                    timestamp,
                ],
            )?;
            Ok(AgentMemoryRecord {
                id: record.id,
                project_id: record.project_id,
                session_id: record.session_id,
                turn_id: record.turn_id,
                topology_run_id: record.topology_run_id,
                node_execution_id: record.node_execution_id,
                agent_id: record.agent_id,
                memory_kind: record.memory_kind,
                title: record.title,
                content: record.content,
                tags: record.tags,
                outcome_status: record.outcome_status,
                source_summary: record.source_summary,
                metadata: record.metadata,
                created_at,
                updated_at: created_at,
            })
        })
        .await
    }

    async fn get_agent_memory(&self, id: Uuid) -> Result<Option<AgentMemoryRecord>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project_id, session_id, turn_id, topology_run_id, node_execution_id,
                        agent_id, memory_kind, title, content, tags_json, outcome_status,
                        source_summary, metadata_json, created_at, updated_at
                 FROM agent_memories
                 WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id.to_string()])?;
            rows.next()?.map(row_to_agent_memory).transpose()
        })
        .await
    }

    async fn list_agent_memories(
        &self,
        project_id: Uuid,
        agent_id: Option<&str>,
        session_id: Option<Uuid>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AgentMemoryRecord>, StorageError> {
        let project_id = project_id.to_string();
        let agent_id = agent_id.map(ToOwned::to_owned);
        let session_id = session_id.map(|id| id.to_string());
        let query = query.map(|value| format!("%{}%", value.to_lowercase()));
        let limit = limit.min(200) as i64;

        self.with_conn(move |conn| {
            let sql = "
                SELECT id, project_id, session_id, turn_id, topology_run_id, node_execution_id,
                       agent_id, memory_kind, title, content, tags_json, outcome_status,
                       source_summary, metadata_json, created_at, updated_at
                FROM agent_memories
                WHERE project_id = ?1
                  AND (?2 IS NULL OR agent_id = ?2)
                  AND (?3 IS NULL OR session_id = ?3)
                  AND (
                    ?4 IS NULL
                    OR lower(title) LIKE ?4
                    OR lower(content) LIKE ?4
                    OR lower(source_summary) LIKE ?4
                    OR lower(tags_json) LIKE ?4
                  )
                ORDER BY created_at DESC
                LIMIT ?5";
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(
                params![project_id, agent_id, session_id, query, limit],
                row_to_agent_memory,
            )?;
            rows.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use crate::sqlite::SqliteStorage;
    use crate::store::{AgentMemoryStore, ProjectStore, SessionStore, ThreadStore, TurnStore};
    use crate::{NewAgentMemoryRecord, NewSession, NewThread, NewTurn, TurnState};

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_memory_round_trips_and_filters_by_agent_and_query() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "agent-a".into(),
                metadata: None,
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &storage,
            NewThread {
                session_id: session.id,
                agent_id: "agent-a".into(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent-a".into(),
                state: TurnState::Completed,
                user_input: Some("remember".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();

        let first = storage
            .create_agent_memory(NewAgentMemoryRecord {
                id: Uuid::new_v4(),
                project_id: project.id,
                session_id: session.id,
                turn_id: turn.id,
                topology_run_id: None,
                node_execution_id: None,
                agent_id: "agent-a".into(),
                memory_kind: "insight".into(),
                title: "Build quirk".into(),
                content: "The builder needs the preview endpoint".into(),
                tags: vec!["builder".into(), "preview".into()],
                outcome_status: Some("succeeded".into()),
                source_summary: "turn completed".into(),
                metadata: json!({"confidence": "high"}),
            })
            .await
            .unwrap();
        storage
            .create_agent_memory(NewAgentMemoryRecord {
                id: Uuid::new_v4(),
                project_id: project.id,
                session_id: session.id,
                turn_id: turn.id,
                topology_run_id: None,
                node_execution_id: None,
                agent_id: "agent-b".into(),
                memory_kind: "outcome".into(),
                title: "Other agent".into(),
                content: "Should not leak into agent-a recall".into(),
                tags: vec!["other".into()],
                outcome_status: Some("failed".into()),
                source_summary: "turn completed".into(),
                metadata: json!({}),
            })
            .await
            .unwrap();

        let fetched = storage
            .get_agent_memory(first.id)
            .await
            .unwrap()
            .expect("record should exist");
        assert_eq!(fetched.title, "Build quirk");
        assert_eq!(fetched.tags, vec!["builder", "preview"]);

        let only_agent_a = storage
            .list_agent_memories(project.id, Some("agent-a"), None, Some("preview"), 10)
            .await
            .unwrap();
        assert_eq!(only_agent_a.len(), 1);
        assert_eq!(only_agent_a[0].id, first.id);
    }
}

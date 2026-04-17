use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::AuditEvent;
use crate::sqlite::SqliteStorage;
use crate::store::AuditStore;

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

fn row_to_audit_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    let session_id: Option<String> = row.get(1)?;
    let metadata_json: String = row.get(6)?;
    let created_at: String = row.get(7)?;

    Ok(AuditEvent {
        id: row.get(0)?,
        session_id: session_id.as_deref().map(parse_uuid).transpose()?,
        entity_kind: row.get(2)?,
        entity_id: row.get(3)?,
        event_kind: row.get(4)?,
        summary: row.get(5)?,
        metadata: serde_json::from_str(&metadata_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        created_at: parse_dt(&created_at)?,
    })
}

#[async_trait]
impl AuditStore for SqliteStorage {
    async fn list_audit_events(&self) -> Result<Vec<AuditEvent>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
                 FROM audit_events
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_audit_event)?;
            rows.collect()
        })
        .await
    }

    async fn list_audit_events_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<AuditEvent>, StorageError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
                 FROM audit_events
                 WHERE session_id = ?1
                 ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(params![session_id.to_string()], row_to_audit_event)?;
            rows.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::sqlite::SqliteStorage;
    use crate::store::{
        ApprovalStore, AuditStore, ProjectStore, SessionStore, ThreadStore, TurnStore,
    };
    use crate::{ApprovalKind, ApprovalState};
    use crate::{NewApprovalRequest, NewSession, NewThread, NewTurn, TurnState};
    use swage_core::ApprovalTimeoutPolicy;

    #[tokio::test(flavor = "multi_thread")]
    async fn audit_event_records_approval_state_transition() {
        let storage = SqliteStorage::in_memory().unwrap();
        let project = storage.get_or_create_default().await.unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "gateway".into(),
                metadata: None,
            })
            .await
            .unwrap();
        let thread = ThreadStore::create(
            &storage,
            NewThread {
                session_id: session.id,
                agent_id: "gateway".into(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".into(),
                state: TurnState::AwaitingApproval,
                user_input: Some("approve".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let approval = storage
            .create_approval(NewApprovalRequest {
                turn_id: turn.id,
                agent_id: "gateway".into(),
                kind: ApprovalKind::Tool,
                tool_name: Some("shell".into()),
                tool_arguments: None,
                hook_name: None,
                prompt: "approve".into(),
                context: None,
                timeout_at: Utc::now(),
                timeout_policy: ApprovalTimeoutPolicy::Reject,
            })
            .await
            .unwrap();

        storage
            .update_approval_state(approval.id, ApprovalState::Approved, Some("ok"))
            .await
            .unwrap();

        let events = storage
            .list_audit_events_by_session(session.id)
            .await
            .unwrap();
        assert!(
            events.iter().any(|event| {
                event.entity_kind == "approval"
                    && event.entity_id == approval.id.to_string()
                    && event.summary.contains("pending -> approved")
            }),
            "expected approval transition audit event, got: {events:?}"
        );
    }
}

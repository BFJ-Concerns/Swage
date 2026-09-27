use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{AuditEvent, NewAuditEvent};
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
    async fn create_audit_event(&self, event: NewAuditEvent) -> Result<AuditEvent, StorageError> {
        self.with_conn(move |conn| {
            let created_at = Utc::now();
            let created_at_str = created_at.to_rfc3339();
            let metadata_json = serde_json::to_string(&event.metadata)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            conn.execute(
                "INSERT INTO audit_events (
                    id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    event.id,
                    event.session_id.map(|id| id.to_string()),
                    event.entity_kind,
                    event.entity_id,
                    event.event_kind,
                    event.summary,
                    metadata_json,
                    created_at_str,
                ],
            )?;
            Ok(AuditEvent {
                id: event.id,
                session_id: event.session_id,
                entity_kind: event.entity_kind,
                entity_id: event.entity_id,
                event_kind: event.event_kind,
                summary: event.summary,
                metadata: event.metadata,
                created_at,
            })
        })
        .await
    }

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
        ApprovalStore, AuditStore, SessionStore, ThreadStore, TopicStore, TurnStore,
    };
    use crate::{ApprovalKind, ApprovalState, NewAuditEvent};
    use crate::{NewApprovalRequest, NewSession, NewThread, NewTurn, TurnState};
    use swage_core::ApprovalTimeoutPolicy;

    #[tokio::test(flavor = "multi_thread")]
    async fn audit_event_records_approval_state_transition() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                ..crate::fixtures::new_session(topic.id)
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

    #[tokio::test(flavor = "multi_thread")]
    async fn create_audit_event_round_trips_without_secret_value() {
        let storage = SqliteStorage::in_memory().unwrap();
        let event = storage
            .create_audit_event(NewAuditEvent {
                id: "secret-audit".into(),
                session_id: None,
                entity_kind: "secret".into(),
                entity_id: "secret-1".into(),
                event_kind: "access_failed".into(),
                summary: "secret access failed".into(),
                metadata: serde_json::json!({
                    "reason": "revoked",
                    "reference": "secret://managed/secret-1"
                }),
            })
            .await
            .unwrap();
        assert_eq!(event.entity_kind, "secret");
        let listed = storage.list_audit_events().await.unwrap();
        assert!(
            listed
                .iter()
                .any(|candidate| candidate.id == "secret-audit")
        );
        assert!(
            listed.iter().all(
                |candidate| candidate.metadata.to_string().contains("secret-1")
                    || !candidate.metadata.to_string().contains("sk-live-secret")
            ),
            "audit metadata should not leak raw secret values: {listed:?}"
        );
    }
}

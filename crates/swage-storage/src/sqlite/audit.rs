use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{AuditEvent, AuditEventFilter, NewAuditEvent};
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

pub(super) fn insert_audit_event(
    conn: &rusqlite::Connection,
    event: NewAuditEvent,
) -> rusqlite::Result<AuditEvent> {
    let created_at = Utc::now();
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
            created_at.to_rfc3339(),
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
}

#[async_trait]
impl AuditStore for SqliteStorage {
    async fn create_audit_event(&self, event: NewAuditEvent) -> Result<AuditEvent, StorageError> {
        self.with_conn(move |conn| insert_audit_event(conn, event))
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

    async fn list_audit_events_filtered(
        &self,
        filter: AuditEventFilter,
    ) -> Result<Vec<AuditEvent>, StorageError> {
        self.with_conn(move |conn| {
            // Each present filter is one predicate; the parameters are bound
            // positionally in the order the predicates were appended.
            let mut predicates: Vec<&str> = Vec::new();
            let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            if let Some(session_id) = filter.session_id {
                predicates.push("session_id = ?");
                values.push(Box::new(session_id.to_string()));
            }
            if let Some(entity_kind) = filter.entity_kind {
                predicates.push("entity_kind = ?");
                values.push(Box::new(entity_kind));
            }
            if let Some(entity_id) = filter.entity_id {
                predicates.push("entity_id = ?");
                values.push(Box::new(entity_id));
            }
            if let Some(event_kind) = filter.event_kind {
                predicates.push("event_kind = ?");
                values.push(Box::new(event_kind));
            }
            if let Some(before) = filter.before {
                // Strictly older, or the same instant and an id that sorts
                // earlier: the page order is (instant DESC, id DESC). Rows are
                // stamped by two producers in two spellings (the gateway's
                // RFC 3339 with an offset, the triggers' `…Z`), so the
                // comparison is on the parsed instant, never the text.
                predicates.push(
                    "(julianday(created_at) < julianday(?) \
                      OR (julianday(created_at) = julianday(?) AND id < ?))",
                );
                let stamp = before.created_at.to_rfc3339();
                values.push(Box::new(stamp.clone()));
                values.push(Box::new(stamp));
                values.push(Box::new(before.id));
            }
            values.push(Box::new(i64::try_from(filter.limit).unwrap_or(i64::MAX)));
            let where_clause = if predicates.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", predicates.join(" AND "))
            };
            let mut stmt = conn.prepare(&format!(
                "SELECT id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at
                 FROM audit_events
                 {where_clause}
                 ORDER BY julianday(created_at) DESC, id DESC
                 LIMIT ?"
            ))?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(values.iter().map(|value| value.as_ref())),
                row_to_audit_event,
            )?;
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
    use crate::{ApprovalKind, ApprovalState, AuditCursor, AuditEventFilter, NewAuditEvent};
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
    async fn the_record_is_read_by_session_entity_and_kind_in_bounded_pages() {
        let storage = SqliteStorage::in_memory().unwrap();
        let topic = storage.general_scope().await.unwrap();
        let session_a = storage
            .create_session(crate::fixtures::new_session(topic.id))
            .await
            .unwrap();
        let session_b = storage
            .create_session(crate::fixtures::new_session(topic.id))
            .await
            .unwrap();
        let event = |id: &str,
                     session: Option<uuid::Uuid>,
                     entity_kind: &str,
                     entity_id: &str,
                     kind: &str| {
            NewAuditEvent {
                id: id.into(),
                session_id: session,
                entity_kind: entity_kind.into(),
                entity_id: entity_id.into(),
                event_kind: kind.into(),
                summary: format!("{kind} on {entity_kind} {entity_id}"),
                metadata: serde_json::json!({ "id": id }),
            }
        };
        for new in [
            event("e1", Some(session_a.id), "turn", "t-1", "state_transition"),
            event(
                "e2",
                Some(session_a.id),
                "surface",
                "s-1",
                "surface_module_written",
            ),
            event(
                "e3",
                Some(session_b.id),
                "surface",
                "s-1",
                "surface_module_written",
            ),
            event("e4", None, "settings", "global", "configuration_change"),
        ] {
            storage.create_audit_event(new).await.unwrap();
        }
        let ids = |events: &[crate::AuditEvent]| {
            events
                .iter()
                .map(|event| event.id.clone())
                .collect::<Vec<_>>()
        };

        let by_kind = storage
            .list_audit_events_filtered(AuditEventFilter {
                event_kind: Some("surface_module_written".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ids(&by_kind), vec!["e3", "e2"], "newest first, by kind");

        let by_entity_and_session = storage
            .list_audit_events_filtered(AuditEventFilter {
                session_id: Some(session_a.id),
                entity_kind: Some("surface".into()),
                entity_id: Some("s-1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ids(&by_entity_and_session), vec!["e2"], "filters combine");

        let first_page = storage
            .list_audit_events_filtered(AuditEventFilter {
                limit: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ids(&first_page), vec!["e4", "e3"]);
        let second_page = storage
            .list_audit_events_filtered(AuditEventFilter {
                limit: 2,
                before: Some(AuditCursor {
                    created_at: first_page[1].created_at,
                    id: first_page[1].id.clone(),
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            ids(&second_page),
            vec!["e2", "e1"],
            "the cursor resumes after the exact event, whatever its timestamp shares"
        );
        assert!(
            second_page.iter().all(|event| event.id != first_page[1].id),
            "the boundary event is on one page only"
        );
    }

    /// The gateway stamps rows in RFC 3339 with an offset; the storage
    /// triggers stamp theirs as `…Z`. The page order and the cursor must read
    /// both as instants, or a trigger's row vanishes at a page boundary.
    #[tokio::test(flavor = "multi_thread")]
    async fn pages_are_ordered_by_instant_across_the_two_timestamp_spellings() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage
            .create_audit_event(NewAuditEvent {
                id: "gateway-stamped".into(),
                session_id: None,
                entity_kind: "turn".into(),
                entity_id: "t".into(),
                event_kind: "state_transition".into(),
                summary: "gateway".into(),
                metadata: serde_json::json!({}),
            })
            .await
            .unwrap();
        // A row as a trigger writes it, one second later, in the trigger's spelling.
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO audit_events (id, session_id, entity_kind, entity_id, event_kind, summary, metadata_json, created_at)
                     VALUES ('trigger-stamped', NULL, 'turn', 't', 'state_transition', 'trigger', '{}',
                             strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 second'))",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let first = storage
            .list_audit_events_filtered(AuditEventFilter {
                limit: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            first[0].id, "trigger-stamped",
            "the later instant comes first, whatever its spelling"
        );
        let next = storage
            .list_audit_events_filtered(AuditEventFilter {
                limit: 1,
                before: Some(AuditCursor {
                    created_at: first[0].created_at,
                    id: first[0].id.clone(),
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            next[0].id, "gateway-stamped",
            "the cursor crosses the spelling boundary"
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

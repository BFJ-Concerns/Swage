use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{ApprovalRequest, ApprovalState, NewApprovalRequest};
use crate::sqlite::SqliteStorage;
use crate::store::ApprovalStore;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Wraps a plain-string parse error so it satisfies `Box<dyn Error + Send + Sync>`.
#[derive(Debug)]
struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

fn parse_uuid(s: &str) -> rusqlite::Result<Uuid> {
    s.parse::<Uuid>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
}

fn parse_str<T: std::str::FromStr>(
    s: &str,
    col: usize,
) -> rusqlite::Result<T>
where
    T::Err: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static,
{
    s.parse::<T>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            Box::new(ParseError(format!("{e}"))),
        )
    })
}

fn parse_json(text: &str, col: usize) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            Box::new(ParseError(e.to_string())),
        )
    })
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// Column order for all SELECT queries in this module:
/// id(0), turn_id(1), agent_id(2), kind(3), tool_name(4), tool_arguments(5),
/// hook_name(6), prompt(7), context(8), state(9), timeout_at(10),
/// timeout_policy(11), decision_reason(12), created_at(13), decided_at(14)
fn row_to_approval(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalRequest> {
    let id: String = row.get(0)?;
    let turn_id: String = row.get(1)?;
    let agent_id: String = row.get(2)?;
    let kind_str: String = row.get(3)?;
    let tool_name: Option<String> = row.get(4)?;
    let tool_arguments_text: Option<String> = row.get(5)?;
    let hook_name: Option<String> = row.get(6)?;
    let prompt: String = row.get(7)?;
    let context_text: Option<String> = row.get(8)?;
    let state_str: String = row.get(9)?;
    let timeout_at_str: String = row.get(10)?;
    let timeout_policy_str: String = row.get(11)?;
    let decision_reason: Option<String> = row.get(12)?;
    let created_at_str: String = row.get(13)?;
    let decided_at_str: Option<String> = row.get(14)?;

    let tool_arguments = tool_arguments_text
        .as_deref()
        .map(|t| parse_json(t, 5))
        .transpose()?;

    let context = context_text
        .as_deref()
        .map(|t| parse_json(t, 8))
        .transpose()?;

    Ok(ApprovalRequest {
        id: parse_uuid(&id)?,
        turn_id: parse_uuid(&turn_id)?,
        agent_id,
        kind: parse_str(&kind_str, 3)?,
        tool_name,
        tool_arguments,
        hook_name,
        prompt,
        context,
        state: parse_str(&state_str, 9)?,
        timeout_at: parse_dt(&timeout_at_str)?,
        timeout_policy: parse_str(&timeout_policy_str, 11)?,
        decision_reason,
        created_at: parse_dt(&created_at_str)?,
        decided_at: decided_at_str.as_deref().map(parse_dt).transpose()?,
    })
}

// ---------------------------------------------------------------------------
// ApprovalStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ApprovalStore for SqliteStorage {
    async fn create_approval(
        &self,
        approval: NewApprovalRequest,
    ) -> Result<ApprovalRequest, StorageError> {
        self.with_conn(move |conn| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let kind_str = approval.kind.to_string();
            let state_str = ApprovalState::Pending.to_string();
            let timeout_at_str = approval.timeout_at.to_rfc3339();
            let timeout_policy_str = approval.timeout_policy.to_string();

            // Serialise optional JSON fields to text.
            let tool_arguments_text = approval
                .tool_arguments
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(ParseError(e.to_string()))))?;

            let context_text = approval
                .context
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(ParseError(e.to_string()))))?;

            conn.execute(
                "INSERT INTO approval_requests
                 (id, turn_id, agent_id, kind, tool_name, tool_arguments, hook_name,
                  prompt, context, state, timeout_at, timeout_policy, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    id.to_string(),
                    approval.turn_id.to_string(),
                    &approval.agent_id,
                    &kind_str,
                    &approval.tool_name,
                    &tool_arguments_text,
                    &approval.hook_name,
                    &approval.prompt,
                    &context_text,
                    &state_str,
                    &timeout_at_str,
                    &timeout_policy_str,
                    &now_str,
                ],
            )?;

            Ok(ApprovalRequest {
                id,
                turn_id: approval.turn_id,
                agent_id: approval.agent_id,
                kind: approval.kind,
                tool_name: approval.tool_name,
                tool_arguments: approval.tool_arguments,
                hook_name: approval.hook_name,
                prompt: approval.prompt,
                context: approval.context,
                state: ApprovalState::Pending,
                timeout_at: approval.timeout_at,
                timeout_policy: approval.timeout_policy,
                decision_reason: None,
                created_at: now,
                decided_at: None,
            })
        })
        .await
    }

    async fn get_approval(&self, id: Uuid) -> Result<Option<ApprovalRequest>, StorageError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT id, turn_id, agent_id, kind, tool_name, tool_arguments, hook_name,
                        prompt, context, state, timeout_at, timeout_policy,
                        decision_reason, created_at, decided_at
                 FROM approval_requests WHERE id = ?1",
                params![id.to_string()],
                row_to_approval,
            )
            .optional()
        })
        .await
    }

    async fn list_pending_by_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<ApprovalRequest>, StorageError> {
        let agent_id = agent_id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, turn_id, agent_id, kind, tool_name, tool_arguments, hook_name,
                        prompt, context, state, timeout_at, timeout_policy,
                        decision_reason, created_at, decided_at
                 FROM approval_requests
                 WHERE state = 'pending' AND agent_id = ?1
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![&agent_id], row_to_approval)?;
            rows.collect()
        })
        .await
    }

    async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, turn_id, agent_id, kind, tool_name, tool_arguments, hook_name,
                        prompt, context, state, timeout_at, timeout_policy,
                        decision_reason, created_at, decided_at
                 FROM approval_requests
                 WHERE state = 'pending'
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], row_to_approval)?;
            rows.collect()
        })
        .await
    }

    async fn update_approval_state(
        &self,
        id: Uuid,
        state: ApprovalState,
        reason: Option<&str>,
    ) -> Result<bool, StorageError> {
        let state_str = state.to_string();
        let reason = reason.map(|s| s.to_owned());
        self.with_conn(move |conn| {
            let now_str = Utc::now().to_rfc3339();
            // Only update rows that are still Pending — the WHERE clause on
            // state='pending' implements optimistic concurrency control.
            let rows_changed = conn.execute(
                "UPDATE approval_requests
                 SET state = ?1, decision_reason = ?2, decided_at = ?3
                 WHERE id = ?4 AND state = 'pending'",
                params![&state_str, &reason, &now_str, id.to_string()],
            )?;
            Ok(rows_changed > 0)
        })
        .await
    }

    async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError> {
        self.with_conn(|conn| {
            let rows_changed = conn.execute(
                "UPDATE approval_requests
                 SET state = 'expired'
                 WHERE state = 'pending'
                   AND turn_id NOT IN (SELECT id FROM turns)",
                [],
            )?;
            Ok(rows_changed as u64)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests (written first — implementation follows above)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rusqlite::params;
    use uuid::Uuid;

    use crate::model::{ApprovalKind, ApprovalState, NewApprovalRequest, NewProject, NewSession};
    use crate::sqlite::SqliteStorage;
    use crate::store::{ApprovalStore, ProjectStore, SessionStore};
    use swage_core::ApprovalTimeoutPolicy;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build a full project → session → thread → turn chain and return the
    /// storage instance plus the newly created turn's id.
    async fn make_turn(storage: &SqliteStorage) -> Uuid {
        let project = storage
            .create(NewProject { name: "p".into(), slug: "p".into(), workspace_path: None })
            .await
            .unwrap();
        let session = storage
            .create_session(NewSession {
                project_id: project.id,
                agent_id: "test-agent".to_string(),
            })
            .await
            .unwrap();

        let thread_id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO threads (id, session_id, agent_id, created_at, updated_at)
                     VALUES (?1, ?2, 'test-agent', ?3, ?3)",
                    params![thread_id.to_string(), session.id.to_string(), &now],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let turn_id = Uuid::new_v4();
        storage
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO turns (id, thread_id, agent_id, state, created_at)
                     VALUES (?1, ?2, 'test-agent', 'pending', ?3)",
                    params![turn_id.to_string(), thread_id.to_string(), &now],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        turn_id
    }

    fn tool_request(turn_id: Uuid) -> NewApprovalRequest {
        NewApprovalRequest {
            turn_id,
            agent_id: "test-agent".to_string(),
            kind: ApprovalKind::Tool,
            tool_name: Some("shell".to_string()),
            tool_arguments: Some(serde_json::json!({"cmd": "rm -rf /tmp/test"})),
            hook_name: None,
            prompt: "Approve shell command?".to_string(),
            context: None,
            timeout_at: Utc::now() + chrono::Duration::seconds(60),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_approval() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;

        let created = storage.create_approval(tool_request(turn_id)).await.unwrap();
        assert_eq!(created.turn_id, turn_id);
        assert_eq!(created.agent_id, "test-agent");
        assert_eq!(created.kind, ApprovalKind::Tool);
        assert_eq!(created.tool_name.as_deref(), Some("shell"));
        assert_eq!(created.state, ApprovalState::Pending);
        assert!(created.decided_at.is_none());
        assert!(created.decision_reason.is_none());

        let fetched = storage.get_approval(created.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.turn_id, turn_id);
        assert_eq!(fetched.state, ApprovalState::Pending);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_approval_returns_none_for_unknown_id() {
        let storage = SqliteStorage::in_memory().unwrap();
        let result = storage.get_approval(Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_pending_by_agent_filters_correctly() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;

        // Create two requests for agent-a and one for agent-b.
        let mut req_a = tool_request(turn_id);
        req_a.agent_id = "agent-a".to_string();
        storage.create_approval(req_a.clone()).await.unwrap();
        storage.create_approval(req_a).await.unwrap();

        let mut req_b = tool_request(turn_id);
        req_b.agent_id = "agent-b".to_string();
        storage.create_approval(req_b).await.unwrap();

        let a_pending = storage.list_pending_by_agent("agent-a").await.unwrap();
        assert_eq!(a_pending.len(), 2, "agent-a should have 2 pending requests");

        let b_pending = storage.list_pending_by_agent("agent-b").await.unwrap();
        assert_eq!(b_pending.len(), 1, "agent-b should have 1 pending request");

        let c_pending = storage.list_pending_by_agent("agent-c").await.unwrap();
        assert!(c_pending.is_empty(), "unknown agent should have no pending requests");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_all_pending_returns_all_pending() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;

        storage.create_approval(tool_request(turn_id)).await.unwrap();
        storage.create_approval(tool_request(turn_id)).await.unwrap();

        let all = storage.list_all_pending().await.unwrap();
        assert_eq!(all.len(), 2);
        for req in &all {
            assert_eq!(req.state, ApprovalState::Pending);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_approval_state_approves_pending_request() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;
        let req = storage.create_approval(tool_request(turn_id)).await.unwrap();

        let updated = storage
            .update_approval_state(req.id, ApprovalState::Approved, Some("lgtm"))
            .await
            .unwrap();
        assert!(updated, "should return true when transitioning from Pending");

        let fetched = storage.get_approval(req.id).await.unwrap().unwrap();
        assert_eq!(fetched.state, ApprovalState::Approved);
        assert_eq!(fetched.decision_reason.as_deref(), Some("lgtm"));
        assert!(fetched.decided_at.is_some(), "decided_at should be set after decision");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_approval_state_returns_false_on_concurrent_decision() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;
        let req = storage.create_approval(tool_request(turn_id)).await.unwrap();

        // First decision succeeds.
        let first = storage
            .update_approval_state(req.id, ApprovalState::Approved, None)
            .await
            .unwrap();
        assert!(first, "first decision should succeed");

        // Second decision on an already-decided request must return false.
        let second = storage
            .update_approval_state(req.id, ApprovalState::Rejected, Some("too late"))
            .await
            .unwrap();
        assert!(!second, "second decision should return false (already decided)");

        // State must still be Approved — the second attempt must not overwrite.
        let fetched = storage.get_approval(req.id).await.unwrap().unwrap();
        assert_eq!(fetched.state, ApprovalState::Approved);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_all_pending_excludes_decided_requests() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;

        let req = storage.create_approval(tool_request(turn_id)).await.unwrap();
        storage.create_approval(tool_request(turn_id)).await.unwrap(); // stays Pending

        // Decide the first.
        storage
            .update_approval_state(req.id, ApprovalState::Approved, None)
            .await
            .unwrap();

        let pending = storage.list_all_pending().await.unwrap();
        assert_eq!(pending.len(), 1, "decided request should not appear in list_all_pending");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_orphaned_approvals_marks_expired() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;

        // Create a pending request.
        let req = storage.create_approval(tool_request(turn_id)).await.unwrap();
        assert_eq!(req.state, ApprovalState::Pending);

        // Delete the parent turn — the FK cascade removes the row, but we
        // test cleanup_orphaned_approvals against the case where we manually
        // break the link by deleting via raw SQL ignoring FKs, then running cleanup.
        //
        // Instead, create a request with a turn_id that never existed, bypassing
        // the FK temporarily via a raw insert.
        let orphan_id = Uuid::new_v4();
        let ghost_turn_id = Uuid::new_v4(); // not in turns table
        let now = Utc::now().to_rfc3339();
        let far_future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        storage
            .with_conn(|conn| {
                // Temporarily disable FK enforcement to insert a dangling row.
                conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
                conn.execute(
                    "INSERT INTO approval_requests
                     (id, turn_id, agent_id, kind, prompt, state, timeout_at, timeout_policy, created_at)
                     VALUES (?1,?2,'orphan-agent','tool','?',  'pending',?3,'reject',?4)",
                    params![orphan_id.to_string(), ghost_turn_id.to_string(), far_future, now],
                )?;
                conn.execute_batch("PRAGMA foreign_keys=ON;")?;
                Ok(())
            })
            .await
            .unwrap();

        // cleanup_orphaned_approvals should mark the dangling row as Expired.
        let count = storage.cleanup_orphaned_approvals().await.unwrap();
        assert_eq!(count, 1, "one orphaned approval should be expired");

        let orphan = storage.get_approval(orphan_id).await.unwrap().unwrap();
        assert_eq!(orphan.state, ApprovalState::Expired);

        // The non-orphaned request should remain Pending.
        let still_pending = storage.get_approval(req.id).await.unwrap().unwrap();
        assert_eq!(still_pending.state, ApprovalState::Pending);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_arguments_round_trip_as_json() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;

        let args = serde_json::json!({"cmd": "ls", "flags": ["-l", "-a"]});
        let mut req = tool_request(turn_id);
        req.tool_arguments = Some(args.clone());

        let created = storage.create_approval(req).await.unwrap();
        assert_eq!(created.tool_arguments, Some(args.clone()));

        let fetched = storage.get_approval(created.id).await.unwrap().unwrap();
        assert_eq!(fetched.tool_arguments, Some(args));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hook_approval_request_stores_hook_name() {
        let storage = SqliteStorage::in_memory().unwrap();
        let turn_id = make_turn(&storage).await;

        let req = NewApprovalRequest {
            turn_id,
            agent_id: "test-agent".to_string(),
            kind: ApprovalKind::Hook,
            tool_name: None,
            tool_arguments: None,
            hook_name: Some("pre_exec".to_string()),
            prompt: "Approve hook?".to_string(),
            context: Some(serde_json::json!({"step": "init"})),
            timeout_at: Utc::now() + chrono::Duration::seconds(30),
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        };

        let created = storage.create_approval(req).await.unwrap();
        assert_eq!(created.kind, ApprovalKind::Hook);
        assert_eq!(created.hook_name.as_deref(), Some("pre_exec"));
        assert!(created.tool_name.is_none());

        let fetched = storage.get_approval(created.id).await.unwrap().unwrap();
        assert_eq!(fetched.hook_name.as_deref(), Some("pre_exec"));
        assert_eq!(
            fetched.context,
            Some(serde_json::json!({"step": "init"}))
        );
    }
}

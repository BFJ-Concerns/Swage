//! The recent-sessions list: every scope's sessions ordered by their latest
//! user or assistant message, with automated sessions — schedule runs,
//! workflow workers, delegated children — grouped beneath the conversation
//! or schedule that caused them rather than listed beside it.
//!
//! Grouping follows each session's recorded origin to its root: a delegated
//! child to its parent session, a workflow worker to its run and the run to
//! the session that dispatched it (through any parent runs), a schedule run
//! to its schedule. A root that is a conversation is shown even when it is
//! outside the listing — archived while a child is live — so a child never
//! surfaces as a top-level entry.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_protocol::client::conversation::{
    RecentSession, RecentSessionsEntry, RecentSessionsResponse, RecentSessionsSource,
    SessionOrigin as WireSessionOrigin,
};
use swage_storage::{SessionActivity, SessionListing, SessionOrigin};

use crate::state::AppState;

/// Origin chains are short (a worker's delegated child of a worker is three
/// hops); anything deeper is a cycle in the recorded origins, not a lineage.
const MAX_ORIGIN_HOPS: usize = 16;

/// Where an automated session's lineage ends.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Root {
    Conversation(Uuid),
    Schedule(Uuid),
    WorkflowRun(Uuid),
}

pub async fn list_recent_sessions(
    state: &AppState,
    listing: SessionListing,
) -> Result<RecentSessionsResponse, GatewayError> {
    let scopes: HashMap<Uuid, (Option<Uuid>, String)> = state
        .topic_store
        .list_scopes()
        .await?
        .into_iter()
        .map(|topic| {
            let name = if topic.is_general {
                "General".to_string()
            } else {
                topic.name.clone()
            };
            (topic.id, (topic.client_topic_id(), name))
        })
        .collect();
    let running: HashSet<Uuid> = state.active_turns.lock().await.keys().copied().collect();
    let project = |activity: &SessionActivity| -> RecentSession {
        let (topic_id, topic_name) = scopes
            .get(&activity.session.topic_id)
            .cloned()
            .unwrap_or((None, "General".to_string()));
        RecentSession {
            id: activity.session.id,
            topic_id,
            topic_name,
            agent_id: activity.session.agent_id.clone(),
            origin: wire_origin(&activity.session.origin),
            created_at: activity.session.created_at,
            last_activity: activity.last_activity,
            message_preview: activity.latest_message.clone(),
            has_interrupted_turn: activity.has_interrupted_turn,
            has_running_turn: running.contains(&activity.session.id),
            archived_at: activity.session.archived_at,
        }
    };

    let listed = state.storage.list_recent_sessions(listing).await?;

    // Conversations keep their own entry; everything automated is filed
    // under its root, in listing order (newest first) within the group.
    let mut sources: Vec<Root> = Vec::new();
    let mut conversations: HashMap<Uuid, RecentSession> = HashMap::new();
    let mut groups: HashMap<Root, Vec<RecentSession>> = HashMap::new();
    for activity in &listed {
        let root = root_of(state, &activity.session.id, &activity.session.origin).await?;
        if !groups.contains_key(&root) {
            sources.push(root.clone());
            groups.insert(root.clone(), Vec::new());
        }
        match (&root, &activity.session.origin) {
            (Root::Conversation(id), SessionOrigin::Direct) if *id == activity.session.id => {
                conversations.insert(*id, project(activity));
            }
            _ => groups
                .get_mut(&root)
                .expect("group inserted above")
                .push(project(activity)),
        }
    }

    let mut entries = Vec::with_capacity(sources.len());
    for root in sources {
        let automated = groups.remove(&root).unwrap_or_default();
        let source = match root {
            Root::Conversation(id) => {
                let session = match conversations.remove(&id) {
                    Some(session) => session,
                    None => match state.storage.get_session_activity(id).await? {
                        Some(activity) => project(&activity),
                        // The root conversation was deleted after its
                        // children were opened; the children stand alone
                        // rather than vanish.
                        None => {
                            entries.extend(automated.into_iter().map(|session| {
                                RecentSessionsEntry {
                                    last_activity: session.last_activity,
                                    source: RecentSessionsSource::Conversation { session },
                                    automated: Vec::new(),
                                }
                            }));
                            continue;
                        }
                    },
                };
                RecentSessionsSource::Conversation { session }
            }
            Root::Schedule(id) => RecentSessionsSource::Schedule {
                id,
                message: state
                    .storage
                    .get_schedule(id)
                    .await?
                    .map(|schedule| schedule.message)
                    .unwrap_or_else(|| "(schedule removed)".to_string()),
            },
            Root::WorkflowRun(id) => RecentSessionsSource::WorkflowRun {
                id,
                label: state
                    .storage
                    .get_workflow_run(id)
                    .await?
                    .and_then(|run| run.script_name.or(run.task)),
            },
        };
        let last_activity = automated
            .iter()
            .map(|session| session.last_activity)
            .chain(source_activity(&source))
            .max()
            .unwrap_or_default();
        entries.push(RecentSessionsEntry {
            source,
            last_activity,
            automated,
        });
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_activity));
    Ok(RecentSessionsResponse { entries })
}

fn source_activity(source: &RecentSessionsSource) -> Option<DateTime<Utc>> {
    match source {
        RecentSessionsSource::Conversation { session } => Some(session.last_activity),
        RecentSessionsSource::Schedule { .. } | RecentSessionsSource::WorkflowRun { .. } => None,
    }
}

fn wire_origin(origin: &SessionOrigin) -> WireSessionOrigin {
    match origin {
        SessionOrigin::Direct => WireSessionOrigin::Direct,
        SessionOrigin::ScheduleRun { schedule_id } => WireSessionOrigin::ScheduleRun {
            schedule_id: *schedule_id,
        },
        SessionOrigin::WorkflowWorker { run_id } => {
            WireSessionOrigin::WorkflowWorker { run_id: *run_id }
        }
        SessionOrigin::Delegation { parent_session_id } => WireSessionOrigin::Delegation {
            parent_session_id: *parent_session_id,
        },
    }
}

/// Follow a session's origin to the conversation, schedule or operator-started
/// workflow run at the top of its lineage.
async fn root_of(
    state: &AppState,
    session_id: &Uuid,
    origin: &SessionOrigin,
) -> Result<Root, GatewayError> {
    let mut current_session = *session_id;
    let mut current_origin = origin.clone();
    for _ in 0..MAX_ORIGIN_HOPS {
        match current_origin {
            SessionOrigin::Direct => return Ok(Root::Conversation(current_session)),
            SessionOrigin::ScheduleRun { schedule_id } => return Ok(Root::Schedule(schedule_id)),
            SessionOrigin::Delegation { parent_session_id } => {
                match state.storage.get_session(parent_session_id).await? {
                    Some(parent) => {
                        current_session = parent.id;
                        current_origin = parent.origin;
                    }
                    // The parent is gone; the child is the best root left.
                    None => return Ok(Root::Conversation(parent_session_id)),
                }
            }
            SessionOrigin::WorkflowWorker { run_id } => {
                let mut run_id = run_id;
                let mut dispatching_session = None;
                for _ in 0..MAX_ORIGIN_HOPS {
                    let Some(run) = state.storage.get_workflow_run(run_id).await? else {
                        break;
                    };
                    match run.parent_run_id {
                        Some(parent_run_id) => run_id = parent_run_id,
                        None => {
                            dispatching_session = run.dispatching_session_id;
                            break;
                        }
                    }
                }
                match dispatching_session {
                    Some(session_id) => match state.storage.get_session(session_id).await? {
                        Some(session) => {
                            current_session = session.id;
                            current_origin = session.origin;
                        }
                        None => return Ok(Root::Conversation(session_id)),
                    },
                    None => return Ok(Root::WorkflowRun(run_id)),
                }
            }
        }
    }
    tracing::warn!(%session_id, "session origin chain exceeded {MAX_ORIGIN_HOPS} hops; treating as its own root");
    Ok(Root::Conversation(*session_id))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use uuid::Uuid;

    use swage_core::Role;
    use swage_protocol::client::conversation::RecentSessionsSource;
    use swage_storage::{
        MessageStore, NewMessage, NewSchedule, NewSession, NewThread, NewTopic, NewWorkflowRun,
        ScheduleCreator, ScheduleKind, ScheduleStatus, ScheduleStore, SessionListing,
        SessionOrigin, SessionPolicy, SessionStore, SqliteStorage, ThreadStore, TopicStore,
        WorkflowStore,
    };

    use super::*;
    use crate::state::AppState;

    fn make_state() -> (AppState, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        (state, storage)
    }

    async fn open(storage: &SqliteStorage, topic_id: Uuid, origin: SessionOrigin) -> Uuid {
        storage
            .create_session(NewSession {
                topic_id,
                agent_id: "gateway".to_string(),
                origin,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session")
            .id
    }

    async fn say(storage: &SqliteStorage, session_id: Uuid, content: &str) {
        let thread = ThreadStore::create(
            storage,
            NewThread {
                session_id,
                agent_id: "gateway".to_string(),
            },
        )
        .await
        .expect("thread");
        storage
            .create_message(NewMessage {
                thread_id: thread.id,
                turn_id: None,
                role: Role::User,
                content: content.to_string(),
                attachments: Vec::new(),
                position: 0,
                agent_id: "gateway".to_string(),
            })
            .await
            .expect("message");
    }

    async fn workflow_run(
        storage: &SqliteStorage,
        topic_id: Uuid,
        dispatching_session_id: Option<Uuid>,
        parent_run_id: Option<Uuid>,
    ) -> Uuid {
        storage
            .create_workflow_run(NewWorkflowRun {
                topic_id,
                parent_run_id,
                parent_seq: parent_run_id.map(|_| 1),
                dispatching_session_id,
                script_version_id: None,
                inline_workflow_id: None,
                inline_workflow_name: Some("build-verify".to_string()),
                inline_workflow_version: None,
                carrying_skill: None,
                task: None,
                source: Some("return null;".to_string()),
                source_hash: "hash".to_string(),
                args: serde_json::Value::Null,
                budget: serde_json::Value::Null,
                engine_version: "test".to_string(),
                dispatch_bound_agent_ids: vec![],
                dispatch_bound_task_definition_ids: vec![],
                workflow_call_tools: None,
                inherited_worker_placement: None,
            })
            .await
            .expect("run")
            .id
    }

    fn entry_ids(response: &RecentSessionsResponse) -> Vec<(String, Vec<Uuid>)> {
        response
            .entries
            .iter()
            .map(|entry| {
                let source = match &entry.source {
                    RecentSessionsSource::Conversation { session } => {
                        format!("conversation:{}", session.id)
                    }
                    RecentSessionsSource::Schedule { id, .. } => format!("schedule:{id}"),
                    RecentSessionsSource::WorkflowRun { id, .. } => format!("run:{id}"),
                };
                (
                    source,
                    entry.automated.iter().map(|session| session.id).collect(),
                )
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn automated_sessions_sit_beneath_their_source_and_the_list_orders_by_group_activity() {
        let (state, storage) = make_state();
        let general = storage.general_scope().await.expect("general scope");
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Research".to_string(),
                slug: "research".into(),
            },
        )
        .await
        .expect("topic");

        // A quiet conversation, older by creation than everything below.
        let quiet = open(&storage, general.id, SessionOrigin::Direct).await;
        // A conversation that dispatched a workflow whose worker delegated a
        // child; every hop should fold into the conversation's entry.
        let dispatcher = open(&storage, topic.id, SessionOrigin::Direct).await;
        let run = workflow_run(&storage, topic.id, Some(dispatcher), None).await;
        let child_run = workflow_run(&storage, topic.id, None, Some(run)).await;
        let worker = open(
            &storage,
            topic.id,
            SessionOrigin::WorkflowWorker { run_id: child_run },
        )
        .await;
        let grandchild = open(
            &storage,
            topic.id,
            SessionOrigin::Delegation {
                parent_session_id: worker,
            },
        )
        .await;
        // A schedule with a run session.
        let schedule = storage
            .create_schedule(NewSchedule {
                id: Uuid::new_v4(),
                message: "Summarise the inbox".to_string(),
                status: ScheduleStatus::Active,
                kind: ScheduleKind::Cron,
                expression: "0 9 * * *".to_string(),
                session_policy: SessionPolicy::Fresh {
                    topic_id: topic.id,
                    agent_id: "gateway".to_string(),
                },
                creator: ScheduleCreator::User,
                delivery_routing: serde_json::json!({}),
                gate: None,
                next_run_at: None,
                last_run_at: None,
            })
            .await
            .expect("schedule");
        let scheduled = open(
            &storage,
            topic.id,
            SessionOrigin::ScheduleRun {
                schedule_id: schedule.id,
            },
        )
        .await;
        // The grandchild speaks last, so the dispatcher's group leads.
        say(&storage, scheduled, "ran").await;
        say(&storage, grandchild, "done").await;

        let response = list_recent_sessions(&state, SessionListing::default())
            .await
            .expect("listing");

        assert_eq!(
            entry_ids(&response),
            vec![
                (
                    format!("conversation:{dispatcher}"),
                    vec![grandchild, worker]
                ),
                (format!("schedule:{}", schedule.id), vec![scheduled]),
                (format!("conversation:{quiet}"), vec![]),
            ]
        );
        let RecentSessionsSource::Schedule { message, .. } = &response.entries[1].source else {
            panic!("second entry should be the schedule");
        };
        assert_eq!(message, "Summarise the inbox");
        let lead = &response.entries[0];
        assert_eq!(lead.last_activity, lead.automated[0].last_activity);
        let RecentSessionsSource::Conversation { session } = &lead.source else {
            panic!("first entry should be the dispatching conversation");
        };
        assert_eq!(session.topic_name, "Research");
        assert_eq!(session.topic_id, Some(topic.id));
        assert!(matches!(
            response.entries[2].source,
            RecentSessionsSource::Conversation { ref session } if session.topic_id.is_none() && session.topic_name == "General"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_topic_filter_narrows_membership_and_an_operator_run_is_its_own_source() {
        let (state, storage) = make_state();
        let general = storage.general_scope().await.expect("general scope");
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "Ops".to_string(),
                slug: "ops".into(),
            },
        )
        .await
        .expect("topic");
        let in_general = open(&storage, general.id, SessionOrigin::Direct).await;
        let run = workflow_run(&storage, topic.id, None, None).await;
        let worker = open(
            &storage,
            topic.id,
            SessionOrigin::WorkflowWorker { run_id: run },
        )
        .await;

        let everywhere = list_recent_sessions(&state, SessionListing::default())
            .await
            .expect("listing");
        let mut sources: Vec<String> = entry_ids(&everywhere)
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        sources.sort();
        let mut expected = vec![format!("conversation:{in_general}"), format!("run:{run}")];
        expected.sort();
        assert_eq!(sources, expected);

        let narrowed = list_recent_sessions(
            &state,
            SessionListing {
                topic_id: Some(topic.id),
                archived: false,
                include_archived: false,
            },
        )
        .await
        .expect("listing");
        assert_eq!(
            entry_ids(&narrowed),
            vec![(format!("run:{run}"), vec![worker])]
        );
        let RecentSessionsSource::WorkflowRun { label, .. } = &narrowed.entries[0].source else {
            panic!("expected the run as source");
        };
        assert_eq!(label.as_deref(), Some("build-verify"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_archived_conversation_still_heads_its_live_children() {
        let (state, storage) = make_state();
        let general = storage.general_scope().await.expect("general scope");
        let parent = open(&storage, general.id, SessionOrigin::Direct).await;
        let child = open(
            &storage,
            general.id,
            SessionOrigin::Delegation {
                parent_session_id: parent,
            },
        )
        .await;
        say(&storage, child, "still going").await;
        let archived = storage
            .archive_inactive_sessions(Utc::now() + chrono::Duration::days(1), &[child])
            .await
            .expect("archive");
        assert_eq!(archived, vec![parent]);

        let response = list_recent_sessions(&state, SessionListing::default())
            .await
            .expect("listing");

        assert_eq!(
            entry_ids(&response),
            vec![(format!("conversation:{parent}"), vec![child])]
        );
        let RecentSessionsSource::Conversation { session } = &response.entries[0].source else {
            panic!("expected the parent conversation as source");
        };
        assert!(session.archived_at.is_some());
    }
}

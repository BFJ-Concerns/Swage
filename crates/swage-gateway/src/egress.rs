//! What a sandboxed command may reach on the network, and the record of it.
//!
//! The sandbox crate's egress proxy decides nothing itself: this module
//! supplies the policy — the install's list of hosts anyone may reach, then
//! the request judge for a host the list does not name, failing closed
//! without its yes — and the recorder, which writes every connection attempt
//! as an `egress_connection` audit event attributed to the session and turn
//! whose command made it (C15, C67).

use std::sync::Arc;

use async_trait::async_trait;
use swage_agent::{JudgeRequest, JudgeVerdict, RequestedReach, SharedRequestJudge};
use swage_sandbox::{
    EgressConnection, EgressPolicy, EgressRecorder, EgressRequest, EgressSetup, EgressVerdict,
};
use swage_storage::Storage;
use uuid::Uuid;

/// The tool name a network connection is judged under: the proxy sees the
/// connection, not the tool call that opened it.
const EGRESS_TOOL_NAME: &str = "network";

/// One host pattern from the install's list: an exact host, or `*.suffix`
/// matching the suffix and every name beneath it. Matching is
/// case-insensitive, as host names are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPattern {
    Exact(String),
    Suffix(String),
}

impl HostPattern {
    pub fn parse(pattern: &str) -> Self {
        let pattern = pattern.trim().to_ascii_lowercase();
        match pattern.strip_prefix("*.") {
            Some(suffix) => HostPattern::Suffix(suffix.to_owned()),
            None => HostPattern::Exact(pattern),
        }
    }

    pub fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self {
            HostPattern::Exact(exact) => &host == exact,
            HostPattern::Suffix(suffix) => {
                &host == suffix
                    || host
                        .strip_suffix(suffix.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
        }
    }
}

/// The policy a turn's sandbox runs under: the install's reachable hosts,
/// then the judge.
pub struct SessionEgressPolicy {
    allowed: Vec<HostPattern>,
    judge: Option<SharedRequestJudge>,
    /// Where the turn's input is read for the judge and the judgement is
    /// recorded; a judgement that cannot be recorded refuses (C68).
    storage: Arc<dyn Storage>,
    /// The session and turn a judgement is attributed to. A sandbox with
    /// neither — a turn no session owns — has nobody to answer for an
    /// unlisted host, so such a host is refused.
    session_id: Option<Uuid>,
    turn_id: Option<Uuid>,
    agent_id: String,
}

impl SessionEgressPolicy {
    pub fn new(
        allowed_hosts: &[String],
        judge: Option<SharedRequestJudge>,
        storage: Arc<dyn Storage>,
        session_id: Option<Uuid>,
        turn_id: Option<Uuid>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            allowed: allowed_hosts
                .iter()
                .map(|pattern| HostPattern::parse(pattern))
                .collect(),
            judge,
            storage,
            session_id,
            turn_id,
            agent_id: agent_id.into(),
        }
    }

    fn listed(&self, host: &str) -> bool {
        self.allowed.iter().any(|pattern| pattern.matches(host))
    }
}

#[async_trait]
impl EgressPolicy for SessionEgressPolicy {
    async fn decide(&self, request: &EgressRequest) -> EgressVerdict {
        if self.listed(&request.host) {
            return EgressVerdict::Allowed {
                basis: "the install's reachable hosts".into(),
            };
        }
        let (Some(judge), Some(session_id), Some(turn_id)) =
            (&self.judge, self.session_id, self.turn_id)
        else {
            return EgressVerdict::Refused {
                reason: "the host is on no standing list of reachable hosts and no judge can \
                         answer for this connection"
                    .into(),
            };
        };
        // A network request carries no justification of its own; the judge
        // is given the task the turn is doing, as the person stated it, and
        // weighs the host against that.
        let task = match self.storage.get_turn(turn_id).await {
            Ok(Some(turn)) => turn.user_input,
            Ok(None) => None,
            Err(error) => {
                return EgressVerdict::Refused {
                    reason: format!("the turn's task could not be read for the judge: {error}"),
                };
            }
        };
        let reach = RequestedReach::NetworkHost {
            host: request.host.clone(),
            port: request.port,
        };
        let arguments = serde_json::json!({
            "host": request.host,
            "port": request.port,
            "task": task.as_deref().map(|text| excerpt(text, 2000)),
        });
        let judgement = judge
            .judge(JudgeRequest {
                session_id,
                turn_id,
                agent_id: self.agent_id.clone(),
                tool_name: EGRESS_TOOL_NAME.into(),
                arguments: arguments.clone(),
                justification: None,
                reach: reach.clone(),
            })
            .await;
        // The judgement is part of the record before the connection can be
        // made, as every judgement is; one that cannot be written refuses.
        let recorded = self
            .storage
            .create_audit_event(swage_storage::NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id: Some(session_id),
                entity_kind: "turn".into(),
                entity_id: turn_id.to_string(),
                event_kind: "request_judged".into(),
                summary: format!(
                    "{} {} for {EGRESS_TOOL_NAME}: {}",
                    judgement.verdict.as_str(),
                    reach.summary(),
                    judgement.reason
                ),
                metadata: serde_json::json!({
                    "tool": EGRESS_TOOL_NAME,
                    "arguments": arguments,
                    "reach": reach.record(),
                    "justification": serde_json::Value::Null,
                    "verdict": judgement.verdict.as_str(),
                    "reason": judgement.reason,
                    "judge": judgement.judge,
                }),
            })
            .await;
        if let Err(error) = recorded {
            return EgressVerdict::Refused {
                reason: format!("the judgement could not be recorded: {error}"),
            };
        }
        match judgement.verdict {
            JudgeVerdict::Allowed => EgressVerdict::Allowed {
                basis: format!(
                    "the judge{}: {}",
                    judgement
                        .judge
                        .as_deref()
                        .map(|judge| format!(" ({judge})"))
                        .unwrap_or_default(),
                    judgement.reason
                ),
            },
            JudgeVerdict::Refused => EgressVerdict::Refused {
                reason: format!("the judge refused it: {}", judgement.reason),
            },
            JudgeVerdict::Unavailable => EgressVerdict::Refused {
                reason: format!("no judge could allow it: {}", judgement.reason),
            },
        }
    }
}

/// The first `limit` characters of a text, marked where it was cut.
fn excerpt(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        let head: String = text.chars().take(limit).collect();
        format!("{head}…")
    }
}

/// Writes every connection attempt to the audit record, attributed to the
/// session and turn whose sandboxed command made it. A write that fails is
/// returned, and the proxy refuses the connection on it.
pub struct AuditEgressRecorder {
    storage: Arc<dyn Storage>,
    session_id: Option<Uuid>,
    turn_id: Option<Uuid>,
}

impl AuditEgressRecorder {
    pub fn new(storage: Arc<dyn Storage>, session_id: Option<Uuid>, turn_id: Option<Uuid>) -> Self {
        Self {
            storage,
            session_id,
            turn_id,
        }
    }
}

#[async_trait]
impl EgressRecorder for AuditEgressRecorder {
    async fn record(&self, connection: EgressConnection) -> Result<(), String> {
        let (verdict, detail) = match &connection.verdict {
            EgressVerdict::Allowed { basis } => ("allowed", basis.clone()),
            EgressVerdict::Refused { reason } => ("refused", reason.clone()),
        };
        let event = swage_storage::NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: self.session_id,
            entity_kind: "turn".into(),
            entity_id: self
                .turn_id
                .map(|turn| turn.to_string())
                .unwrap_or_else(|| "unattributed".into()),
            event_kind: "egress_connection".into(),
            summary: format!(
                "{verdict} {} {}:{}: {detail}",
                connection.method, connection.host, connection.port
            ),
            metadata: serde_json::json!({
                "host": connection.host,
                "port": connection.port,
                "method": connection.method,
                "verdict": verdict,
                "detail": detail,
            }),
        };
        self.storage
            .create_audit_event(event)
            .await
            .map(|_| ())
            .map_err(|error| {
                tracing::error!(%error, host = %connection.host, "egress connection could not be recorded");
                error.to_string()
            })
    }
}

/// The policy and recorder for one turn's sandbox.
pub fn turn_egress_setup(
    allowed_hosts: &[String],
    judge: Option<SharedRequestJudge>,
    storage: Arc<dyn Storage>,
    session_id: Option<Uuid>,
    turn_id: Option<Uuid>,
    agent_id: &str,
) -> EgressSetup {
    EgressSetup {
        policy: Arc::new(SessionEgressPolicy::new(
            allowed_hosts,
            judge,
            storage.clone(),
            session_id,
            turn_id,
            agent_id,
        )),
        recorder: Arc::new(AuditEgressRecorder::new(storage, session_id, turn_id)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use swage_agent::{Judgement, RequestJudge};
    use swage_storage::SqliteStorage;
    use swage_storage::store::{AuditStore, SessionStore, ThreadStore, TopicStore, TurnStore};
    use swage_storage::{NewThread, NewTurn, TurnState};

    /// A store with one session and one turn whose input is the task text.
    async fn store_with_turn() -> (Arc<SqliteStorage>, Uuid, Uuid) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let session = storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .expect("session");
        let thread = ThreadStore::create(
            storage.as_ref(),
            NewThread {
                session_id: session.id,
                agent_id: "agent".into(),
            },
        )
        .await
        .expect("thread");
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "agent".into(),
                state: TurnState::Pending,
                user_input: Some("fetch the deps and build".into()),
                parent_turn_id: None,
            })
            .await
            .expect("turn");
        (storage, session.id, turn.id)
    }

    use super::*;

    #[test]
    fn a_pattern_matches_the_host_or_the_suffix_and_what_lies_beneath_it() {
        let exact = HostPattern::parse("GitHub.com");
        assert!(exact.matches("github.com"));
        assert!(!exact.matches("api.github.com"));
        assert!(!exact.matches("notgithub.com"));

        let suffix = HostPattern::parse("*.github.com");
        assert!(suffix.matches("github.com"));
        assert!(suffix.matches("api.github.com"));
        assert!(suffix.matches("deep.api.GITHUB.com"));
        assert!(!suffix.matches("evilgithub.com"));
    }

    struct ScriptedJudge {
        verdict: JudgeVerdict,
        asked: Mutex<Vec<JudgeRequest>>,
    }

    #[async_trait]
    impl RequestJudge for ScriptedJudge {
        async fn judge(&self, request: JudgeRequest) -> Judgement {
            self.asked.lock().unwrap().push(request);
            Judgement {
                verdict: self.verdict.clone(),
                confident: true,
                reason: "scripted".into(),
                judge: Some("judge".into()),
            }
        }
    }

    fn request(host: &str) -> EgressRequest {
        EgressRequest {
            host: host.into(),
            port: 443,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_listed_host_is_allowed_without_the_judge_and_an_unlisted_one_is_judged_and_recorded()
    {
        let judge = Arc::new(ScriptedJudge {
            verdict: JudgeVerdict::Allowed,
            asked: Mutex::new(Vec::new()),
        });
        let (storage, session_id, turn_id) = store_with_turn().await;
        let policy = SessionEgressPolicy::new(
            &["github.com".into(), "*.crates.io".into()],
            Some(judge.clone()),
            storage.clone(),
            Some(session_id),
            Some(turn_id),
            "agent",
        );

        assert!(policy.decide(&request("github.com")).await.is_allowed());
        assert!(
            policy
                .decide(&request("index.crates.io"))
                .await
                .is_allowed()
        );
        assert!(judge.asked.lock().unwrap().is_empty());

        let verdict = policy.decide(&request("example.org")).await;
        assert_eq!(
            verdict,
            EgressVerdict::Allowed {
                basis: "the judge (judge): scripted".into()
            }
        );
        let asked = judge.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].tool_name, EGRESS_TOOL_NAME);
        assert_eq!(
            asked[0].reach,
            RequestedReach::NetworkHost {
                host: "example.org".into(),
                port: 443
            }
        );
        assert_eq!(
            asked[0].arguments["task"], "fetch the deps and build",
            "the judge is shown the task the turn is doing"
        );
        drop(asked);

        // The judgement is on the record like every other, beside the
        // connection event the proxy writes.
        let judged: Vec<_> = storage
            .list_audit_events_by_session(session_id)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.event_kind == "request_judged")
            .collect();
        assert_eq!(judged.len(), 1, "{judged:?}");
        assert_eq!(judged[0].metadata["reach"]["kind"], "network_host");
        assert_eq!(judged[0].metadata["verdict"], "allowed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlisted_host_fails_closed_on_a_refusal_or_with_no_judge() {
        let (storage, session_id, turn_id) = store_with_turn().await;
        let refusing = SessionEgressPolicy::new(
            &[],
            Some(Arc::new(ScriptedJudge {
                verdict: JudgeVerdict::Refused,
                asked: Mutex::new(Vec::new()),
            })),
            storage.clone(),
            Some(session_id),
            Some(turn_id),
            "agent",
        );
        assert_eq!(
            refusing.decide(&request("example.org")).await,
            EgressVerdict::Refused {
                reason: "the judge refused it: scripted".into()
            }
        );

        let unjudged = SessionEgressPolicy::new(
            &[],
            None,
            storage.clone(),
            Some(session_id),
            Some(turn_id),
            "agent",
        );
        let verdict = unjudged.decide(&request("example.org")).await;
        assert!(!verdict.is_allowed(), "{verdict:?}");

        // A connection no session owns has nobody to answer for it, judge or not.
        let unowned = SessionEgressPolicy::new(
            &[],
            Some(Arc::new(ScriptedJudge {
                verdict: JudgeVerdict::Allowed,
                asked: Mutex::new(Vec::new()),
            })),
            storage,
            None,
            None,
            "agent",
        );
        assert!(!unowned.decide(&request("example.org")).await.is_allowed());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_connection_attempt_is_an_audit_event_of_its_session_and_turn() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let session = storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .expect("session");
        let turn_id = Uuid::new_v4();
        let recorder = AuditEgressRecorder::new(storage.clone(), Some(session.id), Some(turn_id));

        recorder
            .record(EgressConnection {
                host: "github.com".into(),
                port: 443,
                method: "CONNECT".into(),
                verdict: EgressVerdict::Allowed {
                    basis: "the install's reachable hosts".into(),
                },
            })
            .await
            .expect("recorded");
        recorder
            .record(EgressConnection {
                host: "example.org".into(),
                port: 80,
                method: "GET".into(),
                verdict: EgressVerdict::Refused {
                    reason: "no judge".into(),
                },
            })
            .await
            .expect("recorded");

        let events = storage
            .list_audit_events_by_session(session.id)
            .await
            .expect("events");
        let egress: Vec<_> = events
            .iter()
            .filter(|event| event.event_kind == "egress_connection")
            .collect();
        assert_eq!(egress.len(), 2, "{events:?}");
        assert!(
            egress
                .iter()
                .all(|event| event.entity_id == turn_id.to_string())
        );
        let hosts: Vec<_> = egress
            .iter()
            .map(|event| {
                (
                    event.metadata["host"].as_str().unwrap(),
                    event.metadata["verdict"].as_str().unwrap(),
                )
            })
            .collect();
        assert!(hosts.contains(&("github.com", "allowed")));
        assert!(hosts.contains(&("example.org", "refused")));
    }
}

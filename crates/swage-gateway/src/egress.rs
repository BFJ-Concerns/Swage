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
        let judgement = judge
            .judge(JudgeRequest {
                session_id,
                turn_id,
                agent_id: self.agent_id.clone(),
                tool_name: EGRESS_TOOL_NAME.into(),
                arguments: serde_json::json!({ "host": request.host, "port": request.port }),
                justification: None,
                reach: RequestedReach::NetworkHost {
                    host: request.host.clone(),
                    port: request.port,
                },
            })
            .await;
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

/// Writes every connection attempt to the audit record, attributed to the
/// session and turn whose sandboxed command made it.
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
    async fn record(&self, connection: EgressConnection) {
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
        if let Err(error) = self.storage.create_audit_event(event).await {
            tracing::error!(%error, host = %connection.host, "egress connection could not be recorded");
        }
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
    use swage_storage::store::{AuditStore, SessionStore, TopicStore};

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

    #[tokio::test]
    async fn a_listed_host_is_allowed_without_the_judge_and_an_unlisted_one_is_judged() {
        let judge = Arc::new(ScriptedJudge {
            verdict: JudgeVerdict::Allowed,
            asked: Mutex::new(Vec::new()),
        });
        let policy = SessionEgressPolicy::new(
            &["github.com".into(), "*.crates.io".into()],
            Some(judge.clone()),
            Some(Uuid::new_v4()),
            Some(Uuid::new_v4()),
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
    }

    #[tokio::test]
    async fn an_unlisted_host_fails_closed_on_a_refusal_or_with_no_judge() {
        let refusing = SessionEgressPolicy::new(
            &[],
            Some(Arc::new(ScriptedJudge {
                verdict: JudgeVerdict::Refused,
                asked: Mutex::new(Vec::new()),
            })),
            Some(Uuid::new_v4()),
            Some(Uuid::new_v4()),
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
            Some(Uuid::new_v4()),
            Some(Uuid::new_v4()),
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
            .await;
        recorder
            .record(EgressConnection {
                host: "example.org".into(),
                port: 80,
                method: "GET".into(),
                verdict: EgressVerdict::Refused {
                    reason: "no judge".into(),
                },
            })
            .await;

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

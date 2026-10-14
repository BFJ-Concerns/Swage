//! The judge seam: who answers for a request a session's grants do not cover.
//!
//! A tool call that reaches beyond the session's grants — a filesystem
//! location outside its granted locations — needs someone to answer for it.
//! In an attended session that is the person at the approval card. In an
//! unattended session nobody is watching, so the request goes to a judging
//! model: it sees the call and the agent's stated justification and says yes
//! or no with a reason. Without a yes — a refusal, an unreadable verdict, no
//! judge configured — the request fails closed and the model is told why.
//!
//! The agent runtime owns the seam and the fail-closed rule; what judges, and
//! how, is the caller's: the gateway supplies a [`RequestJudge`] backed by its
//! configured judging model and records every judgement in the audit trail.
//! A judged yes covers that one request and nothing else — it never widens
//! the session's grants.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

/// The parameter an agent attaches to a call to say why it reaches beyond
/// its grants. Tools that may reach outside the session's granted locations
/// declare it in their schema; the judge reads it from the call's arguments.
pub const JUSTIFICATION_PARAMETER: &str = "justification";

/// The reach a request asks for beyond the session's grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedReach {
    /// A filesystem location outside the session's granted locations.
    Location {
        path: String,
        /// `read` or `write`, as the tool reported it.
        access: String,
    },
}

/// Everything the judge is given about one request.
#[derive(Debug, Clone)]
pub struct JudgeRequest {
    pub session_id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub tool_name: String,
    /// The call's arguments as the model sent them.
    pub arguments: Value,
    /// The agent's stated reason for the request, when it gave one.
    pub justification: Option<String>,
    pub reach: RequestedReach,
}

/// What the judge decided, or why it could not decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeVerdict {
    /// This one request may proceed.
    Allowed,
    /// The request may not proceed.
    Refused,
    /// No judgement was made: no judge is configured, its model could not be
    /// reached, or its reply was unreadable. Treated as a refusal.
    Unavailable,
}

impl JudgeVerdict {
    /// The verdict's recorded name.
    pub fn as_str(&self) -> &'static str {
        match self {
            JudgeVerdict::Allowed => "allowed",
            JudgeVerdict::Refused => "refused",
            JudgeVerdict::Unavailable => "unavailable",
        }
    }
}

/// One judgement: the verdict, the judge's reason in its own words, and the
/// model that gave it (absent when none did).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    pub verdict: JudgeVerdict,
    pub reason: String,
    pub judge: Option<String>,
}

impl Judgement {
    /// A judgement no model made, with the reason none could.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            verdict: JudgeVerdict::Unavailable,
            reason: reason.into(),
            judge: None,
        }
    }
}

/// Decides whether a request beyond the session's grants may proceed.
#[async_trait]
pub trait RequestJudge: Send + Sync {
    async fn judge(&self, request: JudgeRequest) -> Judgement;
}

pub type SharedRequestJudge = Arc<dyn RequestJudge>;

/// The justification a call carries, read from its arguments.
pub fn justification_from_arguments(arguments: &Value) -> Option<String> {
    arguments
        .get(JUSTIFICATION_PARAMETER)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// What the model reads when a request beyond its grants is refused.
pub(crate) fn refused_feedback(
    tool_name: &str,
    reach: &RequestedReach,
    judgement: &Judgement,
) -> String {
    let RequestedReach::Location { path, access } = reach;
    let decision = match judgement.verdict {
        JudgeVerdict::Refused => "the judge refused it",
        JudgeVerdict::Allowed | JudgeVerdict::Unavailable => "no judge could allow it",
    };
    format!(
        "Tool error: `{tool_name}` may not {access} `{path}`: the location lies outside this \
         session's granted locations and {decision} ({}). The call did not run. Work within \
         the session's granted locations, or state in `{JUSTIFICATION_PARAMETER}` why this one \
         is needed.",
        judgement.reason
    )
}

/// What the model reads above a result the judge let through.
pub(crate) fn allowed_note(reach: &RequestedReach, judgement: &Judgement) -> String {
    let RequestedReach::Location { path, access } = reach;
    format!(
        "[The judge allowed this call to {access} `{path}` ({}). This one request ran; the \
         session's granted locations are unchanged.]",
        judgement.reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_justification_is_read_from_the_arguments_and_blank_text_is_none() {
        assert_eq!(
            justification_from_arguments(&serde_json::json!({
                "path": "/etc/hosts",
                "justification": "  the task names this file  "
            })),
            Some("the task names this file".to_owned())
        );
        assert_eq!(
            justification_from_arguments(&serde_json::json!({ "justification": "   " })),
            None
        );
        assert_eq!(
            justification_from_arguments(&serde_json::json!({ "path": "/etc/hosts" })),
            None
        );
    }

    #[test]
    fn refusal_text_names_the_tool_the_location_and_the_reason() {
        let reach = RequestedReach::Location {
            path: "/srv/data".into(),
            access: "write".into(),
        };
        let refused = refused_feedback(
            "file_write",
            &reach,
            &Judgement {
                verdict: JudgeVerdict::Refused,
                reason: "the task never named this path".into(),
                judge: Some("judge".into()),
            },
        );
        assert!(
            refused.contains("`file_write` may not write `/srv/data`"),
            "{refused}"
        );
        assert!(refused.contains("the judge refused it (the task never named this path)"));
        assert!(refused.contains("did not run"));

        let unavailable = refused_feedback(
            "file_read",
            &reach,
            &Judgement::unavailable("no judge model is configured"),
        );
        assert!(unavailable.contains("no judge could allow it (no judge model is configured)"));
    }
}

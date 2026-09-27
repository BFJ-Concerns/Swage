//! Complete agent-call inputs for tests in this crate and its dependants.
//! Tests override the fields whose behaviour they exercise with struct update syntax.

use crate::AgentCallRequest;

/// A call with no selected agent, model, tools, skills, session or placement.
pub fn agent_call_request(prompt: String) -> AgentCallRequest {
    AgentCallRequest {
        prompt,
        agent: None,
        task: None,
        tools: None,
        skills: None,
        model: None,
        fallback_model: None,
        effort: None,
        timeout_ms: None,
        cwd: None,
        isolation: None,
        schema: None,
        max_attempts: 1,
        label: None,
        phase: None,
        identity: None,
        session: None,
        worker: None,
        elements: None,
        variables: None,
        assembly: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_preserves_every_other_default() {
        let request = AgentCallRequest {
            task: Some("review".into()),
            ..agent_call_request("inspect".into())
        };
        assert_eq!(request.prompt, "inspect");
        assert_eq!(request.task.as_deref(), Some("review"));
        assert!(request.agent.is_none());
        assert!(request.model.is_none());
        assert!(request.tools.is_none());
        assert!(request.skills.is_none());
        assert!(request.fallback_model.is_none());
        assert!(request.effort.is_none());
        assert!(request.timeout_ms.is_none());
        assert!(request.cwd.is_none());
        assert!(request.isolation.is_none());
        assert!(request.schema.is_none());
        assert_eq!(request.max_attempts, 1);
        assert!(request.label.is_none());
        assert!(request.phase.is_none());
        assert!(request.identity.is_none());
        assert!(request.session.is_none());
        assert!(request.worker.is_none());
        assert!(request.elements.is_none());
        assert!(request.variables.is_none());
        assert!(request.assembly.is_none());
    }
}

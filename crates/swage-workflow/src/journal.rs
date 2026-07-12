use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{JournalEntry, JournalEntryStatus, WorkflowBudget};

#[derive(Debug, Error, Eq, PartialEq)]
pub enum JournalError {
    #[error("replay diverged at sequence {seq}: expected {expected}, received {actual}")]
    NondeterministicReplay {
        seq: u64,
        expected: String,
        actual: String,
    },
    #[error("agent-call budget exhausted")]
    AgentCallsExceeded,
    #[error("token budget exhausted")]
    TokensExceeded,
}

/// Deterministic view over an existing run journal.
pub struct JournalReplay<'a> {
    entries: &'a [JournalEntry],
    cursor: usize,
    budget: &'a WorkflowBudget,
}

impl<'a> JournalReplay<'a> {
    pub const fn new(entries: &'a [JournalEntry], budget: &'a WorkflowBudget) -> Self {
        Self {
            entries,
            cursor: 0,
            budget,
        }
    }

    pub fn args_hash(request: &serde_json::Value) -> String {
        // serde_json maps are deterministically ordered without preserve_order. Hashing the
        // serialised value gives the replay tripwire a stable representation.
        let bytes = serde_json::to_vec(request).expect("JSON values always serialise");
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn substitute(
        &mut self,
        request: &serde_json::Value,
    ) -> Result<Option<&'a JournalEntry>, JournalError> {
        let Some(entry) = self.entries.get(self.cursor) else {
            return Ok(None);
        };
        let actual = Self::args_hash(request);
        if entry.args_hash != actual {
            return Err(JournalError::NondeterministicReplay {
                seq: entry.seq,
                expected: entry.args_hash.clone(),
                actual,
            });
        }
        self.cursor += 1;
        Ok((entry.status == JournalEntryStatus::Completed).then_some(entry))
    }

    pub fn spent_tokens(&self) -> u64 {
        self.entries.iter().map(JournalEntry::spent_tokens).sum()
    }

    pub fn remaining_tokens(&self) -> Option<u64> {
        self.budget
            .max_total_tokens
            .map(|limit| limit.saturating_sub(self.spent_tokens()))
    }

    pub fn check_agent_dispatch(&self) -> Result<(), JournalError> {
        let completed_calls = self
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == crate::JournalEntryKind::AgentCall
                    && entry.status == JournalEntryStatus::Completed
            })
            .count() as u32;
        if self
            .budget
            .max_agent_calls
            .is_some_and(|limit| completed_calls >= limit)
        {
            return Err(JournalError::AgentCallsExceeded);
        }
        if self
            .budget
            .max_total_tokens
            .is_some_and(|limit| self.spent_tokens() >= limit)
        {
            return Err(JournalError::TokensExceeded);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::{JournalEntryKind, JournalEntryStatus};

    fn completed(seq: u64, request: serde_json::Value, tokens: u64) -> JournalEntry {
        JournalEntry {
            id: Uuid::new_v4(),
            seq,
            completed_order: Some(seq),
            kind: JournalEntryKind::AgentCall,
            status: JournalEntryStatus::Completed,
            args_hash: JournalReplay::args_hash(&request),
            request,
            result: Some(json!({ "answer": seq })),
            error_message: None,
            input_tokens: Some(tokens),
            output_tokens: Some(0),
            effect_kind: None,
            idempotency_key: None,
        }
    }

    #[test]
    fn completed_entries_substitute_without_live_execution() {
        let request = json!({ "agent": "writer", "prompt": "draft" });
        let entries = vec![completed(0, request.clone(), 7)];
        let budget = WorkflowBudget::default();
        let mut replay = JournalReplay::new(&entries, &budget);

        let entry = replay
            .substitute(&request)
            .expect("matching replay")
            .expect("completed substitution");
        assert_eq!(entry.result, Some(json!({ "answer": 0 })));
    }

    #[test]
    fn changed_request_fails_replay_loudly() {
        let original = json!({ "prompt": "first" });
        let entries = vec![completed(0, original, 0)];
        let budget = WorkflowBudget::default();
        let mut replay = JournalReplay::new(&entries, &budget);

        let error = replay
            .substitute(&json!({ "prompt": "different" }))
            .expect_err("changed input must fail");
        assert!(matches!(
            error,
            JournalError::NondeterministicReplay { seq: 0, .. }
        ));
    }

    #[test]
    fn budgets_are_derived_from_completed_journal_results() {
        let entries = vec![completed(0, json!({ "prompt": "first" }), 11)];
        let budget = WorkflowBudget {
            max_agent_calls: Some(1),
            max_total_tokens: Some(20),
            max_concurrent_agent_calls: Some(1),
        };
        let replay = JournalReplay::new(&entries, &budget);

        assert_eq!(replay.spent_tokens(), 11);
        assert_eq!(replay.remaining_tokens(), Some(9));
        assert_eq!(
            replay.check_agent_dispatch(),
            Err(JournalError::AgentCallsExceeded)
        );
    }
}

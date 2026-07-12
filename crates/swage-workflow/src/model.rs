use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Limits enforced by the host around model calls.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowBudget {
    pub max_agent_calls: Option<u32>,
    pub max_total_tokens: Option<u64>,
    pub max_concurrent_agent_calls: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntryKind {
    AgentCall,
    Effect,
    Log,
    Phase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntryStatus {
    Started,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    IdempotencyKey,
    CheckBeforeRedo,
}

/// One durable host interaction in initiation order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: Uuid,
    pub seq: u64,
    pub completed_order: Option<u64>,
    pub kind: JournalEntryKind,
    pub status: JournalEntryStatus,
    pub args_hash: String,
    pub request: Value,
    pub result: Option<Value>,
    pub error_message: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub effect_kind: Option<EffectKind>,
    pub idempotency_key: Option<Uuid>,
}

impl JournalEntry {
    pub fn spent_tokens(&self) -> u64 {
        if self.status != JournalEntryStatus::Completed {
            return 0;
        }
        self.input_tokens.unwrap_or(0) + self.output_tokens.unwrap_or(0)
    }
}

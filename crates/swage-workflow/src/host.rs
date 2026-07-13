use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCallRequest {
    pub prompt: String,
    pub agent: String,
    pub model: Option<String>,
    pub schema: Option<Value>,
    pub max_attempts: u32,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCallResult {
    pub output: Value,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    #[serde(default)]
    pub turn_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectPayload {
    pub content: String,
    pub metadata: Option<Value>,
    pub target: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectReceipt {
    pub delivered: bool,
    pub target: String,
    pub effect_seq: u64,
}

#[async_trait]
pub trait WorkflowHost: Send + Sync {
    fn record(&self, _kind: &str, _message: String) {}

    fn budget_spent(&self) -> u64 {
        0
    }

    fn budget_remaining(&self) -> Option<u64> {
        None
    }

    async fn run_agent_call(
        &self,
        request: AgentCallRequest,
        cancellation: CancellationToken,
    ) -> Result<AgentCallResult, String>;

    async fn deliver(
        &self,
        payload: EffectPayload,
        cancellation: CancellationToken,
    ) -> Result<EffectReceipt, String>;
}

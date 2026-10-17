//! Context operations answered on the assembly caller's thread. The plugin
//! gets a JSON door, never the borrowed elements, tokeniser or config paths.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_context::{
    compaction::TruncationCompactor,
    report::{AssemblyOutput, CompactionRecord},
    tokeniser::Tokeniser,
};
use swage_core::ChatMessage;
use swage_plugin::{HostCallFailure, PluginHost};
use swage_protocol::plugin::ContextOperation;
use tokio::sync::{mpsc, oneshot};

struct Request {
    value: Value,
    reply: oneshot::Sender<Result<Value, HostCallFailure>>,
}

pub(super) struct CompositionHost {
    parent: Arc<dyn PluginHost>,
    requests: mpsc::UnboundedSender<Request>,
}

fn refusal(message: impl Into<String>) -> HostCallFailure {
    HostCallFailure {
        name: "ContextCompositionError".into(),
        message: message.into(),
    }
}

#[async_trait]
impl PluginHost for CompositionHost {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, HostCallFailure> {
        self.parent.call_tool(name, arguments).await
    }

    async fn complete(&self, request: Value) -> Result<Value, HostCallFailure> {
        self.parent.complete(request).await
    }

    async fn context(&self, value: Value) -> Result<Value, HostCallFailure> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request { value, reply })
            .map_err(|_| refusal("the assembly invocation has ended"))?;
        answer
            .await
            .map_err(|_| refusal("the assembly invocation has ended"))?
    }

    fn log(&self, message: &str) {
        self.parent.log(message);
    }
}

/// Keep borrowed composition services on the caller while host requests are
/// awaited by the isolate. Each invocation has its own queue; none of its
/// authority or pending requests can carry over to a later turn.
pub(super) async fn compose(
    slot: &super::slot::PluginSlot,
    input: Value,
    tokeniser: &dyn Tokeniser,
    named: impl FnOnce() -> AssemblyOutput,
    named_available: bool,
) -> Result<(Value, Vec<CompactionRecord>), String> {
    let (requests, mut receiver) = mpsc::unbounded_channel();
    let host = Arc::new(CompositionHost {
        parent: Arc::clone(&slot.host),
        requests,
    });
    let invocation = slot
        .runtime
        .invoke_slot("context_program", &input, &slot.context, host);
    tokio::pin!(invocation);
    let mut named = Some(named);
    let mut named_output = None;
    let mut compactions = Vec::new();
    loop {
        tokio::select! {
            answer = &mut invocation => return answer
                .map(|output| (output, compactions))
                .map_err(|error| format!("{}: {error}", slot.plugin)),
            Some(request) = receiver.recv() => {
                let result = (|| {
                    let operation: ContextOperation = serde_json::from_value(request.value)
                        .map_err(|error| refusal(error.to_string()))?;
                    let messages = |values: Vec<Value>| -> Result<Vec<ChatMessage>, HostCallFailure> {
                        values.into_iter().map(|value| serde_json::from_value(value)
                            .map_err(|error| refusal(error.to_string()))).collect()
                    };
                    match operation {
                        ContextOperation::NamedAssembly => {
                            if !named_available {
                                return Err(refusal("no named assembly is selected"));
                            }
                            if named_output.is_none() {
                                let output = named.take().expect("once")();
                                compactions.extend(output.report.compaction_applied.iter().cloned());
                                named_output = Some(serde_json::to_value(output)
                                    .map_err(|error| refusal(error.to_string()))?);
                            }
                            Ok(named_output.as_ref().expect("assembled").clone())
                        }
                        ContextOperation::CountMessages { messages: values } =>
                            Ok(json!(tokeniser.count_messages(&messages(values)?))),
                        ContextOperation::CompactHistory { messages: values, budget } => {
                            let (messages, record) = TruncationCompactor::new().compact(messages(values)?, budget, tokeniser);
                            compactions.push(record.clone());
                            serde_json::to_value(swage_protocol::plugin::ContextCompactionOutput {
                                messages: messages.iter().map(|message| serde_json::to_value(message).expect("message")).collect(),
                                record,
                            }).map_err(|error| refusal(error.to_string()))
                        }
                    }
                })();
                let _ = request.reply.send(result);
            }
        }
    }
}

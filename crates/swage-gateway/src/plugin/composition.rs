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

/// What the host did on a composition's behalf, for the report to be built
/// from: the plugin's answer is its composition, never the record of it.
pub(super) struct HostWork {
    /// The named assembly the host ran, when the plugin asked for it.
    pub named: Option<AssemblyOutput>,
    /// Each history compaction the host performed: the messages it was
    /// given, the messages it answered with, and its record.
    pub compactions: Vec<HostCompaction>,
}

pub(super) struct HostCompaction {
    pub input: Vec<ChatMessage>,
    pub output: Vec<ChatMessage>,
    pub record: CompactionRecord,
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
) -> Result<(Value, HostWork), String> {
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
    let mut work = HostWork {
        named: None,
        compactions: Vec::new(),
    };
    loop {
        tokio::select! {
            answer = &mut invocation => return answer
                .map(|output| (output, work))
                .map_err(|error| {
                    slot.note_failure("the context_program slot", &error);
                    format!("{}: {error}", slot.plugin)
                }),
            Some(request) = receiver.recv() => {
                let result = (|| {
                    let operation: ContextOperation = serde_json::from_value(request.value)
                        .map_err(|error| refusal(error.to_string()))?;
                    // The messages an operation takes are the shapes the
                    // slot's own answer may carry.
                    let messages = |values: Vec<Value>| -> Result<Vec<ChatMessage>, HostCallFailure> {
                        values.iter().enumerate().map(|(index, value)| super::slot::composed_message(value)
                            .map_err(|reason| refusal(format!("message {index} {reason}")))).collect()
                    };
                    match operation {
                        ContextOperation::NamedAssembly => {
                            if !named_available {
                                return Err(refusal("no named assembly is selected"));
                            }
                            if work.named.is_none() {
                                work.named = Some(named.take().expect("once")());
                            }
                            serde_json::to_value(work.named.as_ref().expect("assembled"))
                                .map_err(|error| refusal(error.to_string()))
                        }
                        ContextOperation::CountMessages { messages: values } =>
                            Ok(json!(tokeniser.count_messages(&messages(values)?))),
                        ContextOperation::CompactHistory { messages: values, budget } => {
                            let input = messages(values)?;
                            let (output, record) = TruncationCompactor::new().compact(input.clone(), budget, tokeniser);
                            let answer = serde_json::to_value(swage_protocol::plugin::ContextCompactionOutput {
                                messages: output.iter().map(|message| serde_json::to_value(message).expect("message")).collect(),
                                record: record.clone(),
                            }).map_err(|error| refusal(error.to_string()))?;
                            work.compactions.push(HostCompaction { input, output, record });
                            Ok(answer)
                        }
                    }
                })();
                let _ = request.reply.send(result);
            }
        }
    }
}

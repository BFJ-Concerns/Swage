//! Test-only capture of tracing messages and approval identifiers.

use std::sync::{Arc, Mutex};

/// Fields read by tests of emitted log events.
#[derive(Debug, Default)]
pub struct RecordedMessage {
    pub message: String,
    pub approval_id: Option<String>,
}

/// Captures events with a message field, preserving their approval identifier.
#[derive(Clone, Default)]
pub struct MessageRecorder(pub Arc<Mutex<Vec<RecordedMessage>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for MessageRecorder {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        #[derive(Default)]
        struct MessageVisitor(RecordedMessage);

        impl tracing::field::Visit for MessageVisitor {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                match field.name() {
                    "message" => self.0.message = value.to_owned(),
                    "approval_id" => self.0.approval_id = Some(value.to_owned()),
                    _ => {}
                }
            }

            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                match field.name() {
                    "message" => self.0.message = format!("{value:?}"),
                    "approval_id" => self.0.approval_id = Some(format!("{value:?}")),
                    _ => {}
                }
            }
        }

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        if !visitor.0.message.is_empty() {
            self.0
                .lock()
                .expect("recorded log messages")
                .push(visitor.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    #[test]
    fn records_message_and_approval_id_from_real_event() {
        let recorder = MessageRecorder::default();
        let captured = Arc::clone(&recorder.0);
        tracing::subscriber::with_default(Registry::default().with(recorder), || {
            tracing::info!(approval_id = "approval-1", "approval settled");
        });
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "approval settled");
        assert_eq!(events[0].approval_id.as_deref(), Some("approval-1"));
    }
}

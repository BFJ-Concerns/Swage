//! The clock value for the coming turn, separate from stable orientation.

use chrono::{DateTime, Utc};
use swage_core::{ChatMessage, Role};

use crate::element::ContextElement;
use crate::tokeniser::Tokeniser;

pub struct CurrentTimeElement {
    now: DateTime<Utc>,
}

impl CurrentTimeElement {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self { now }
    }
}

impl ContextElement for CurrentTimeElement {
    fn name(&self) -> &str {
        "current_time"
    }

    fn priority(&self) -> i32 {
        104
    }

    fn content(&self) -> Vec<ChatMessage> {
        vec![ChatMessage::new(
            Role::System,
            format!("Current time: {}", self.now.format("%Y-%m-%d %H:%M:%S UTC")),
        )]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        "current time".into()
    }
}

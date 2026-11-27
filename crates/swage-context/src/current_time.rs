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
        // Seconds-only formatting must not put the clock before its capture.
        let now = if self.now.timestamp_subsec_nanos() == 0 {
            self.now
        } else {
            self.now + chrono::Duration::seconds(1)
        };
        vec![ChatMessage::new(
            Role::System,
            format!("Current time: {}", now.format("%Y-%m-%d %H:%M:%S UTC")),
        )]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        "current time".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_time_is_not_earlier_than_its_full_precision_capture() {
        for instant in [
            "2026-10-16T13:42:57.123456789Z",
            "2026-10-16T23:59:59.999999999Z",
            "2026-10-16T13:42:57Z",
        ] {
            let captured = DateTime::parse_from_rfc3339(instant)
                .unwrap()
                .with_timezone(&Utc);
            let messages = CurrentTimeElement::new(captured).content();
            let text = messages[0].text_content();
            let rendered = chrono::NaiveDateTime::parse_from_str(
                text.trim_start_matches("Current time: "),
                "%Y-%m-%d %H:%M:%S UTC",
            )
            .unwrap()
            .and_utc();
            assert!(
                rendered >= captured,
                "rendered clock {rendered} precedes captured instant {captured}"
            );
            assert!(rendered - captured < chrono::Duration::seconds(1));
        }
    }
}

//! Working-norms element — what a model's own harness would have told it.
//!
//! Models arrive over the operator's proxy bare, with none of the prompt
//! scaffolding their vendor harnesses supply, so the built-in assembly
//! provides the equivalent: working norms adapted to Swage's tools, selected
//! by the coming turn's model family. The common norms render for every
//! session; a recognised family appends its own emphasis; an unrecognised
//! one degrades to the common norms, never to silence.
//!
//! Every string here is instruction text: it steers each session of a whole
//! model family, so edits are drafting-tier work, not routine maintenance.
//! The block is also an experimentation location (`context.working_norms`) —
//! alternative texts compete through the ordinary switchable/forced seam,
//! and nothing model-facing names the mechanism.

use swage_core::{ChatMessage, Role};

use crate::element::ContextElement;
use crate::tokeniser::Tokeniser;

/// The model families whose bare-proxy sessions get family-specific norms.
/// Detection is deliberately conservative: a name that matches no known
/// family gets the common norms alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    Claude,
    Gpt,
    Unknown,
}

impl ModelFamily {
    /// Classify a configured model id by its vendor naming ("claude-…",
    /// "gpt-…", the o-series). Ids are operator configuration, so matching
    /// stays on stable public prefixes rather than trying to be clever.
    pub fn from_model_id(model_id: &str) -> Self {
        let id = model_id.trim().to_ascii_lowercase();
        let o_series = id.starts_with('o')
            && id[1..].starts_with(|character: char| character.is_ascii_digit());
        if id.contains("claude") {
            Self::Claude
        } else if id.contains("gpt") || o_series {
            Self::Gpt
        } else {
            Self::Unknown
        }
    }
}

const COMMON_NORMS: &str = "\
## Working norms
Work to completion: when the task is clear, carry it through with your \
tools and report the outcome, rather than describing what you would do or \
stopping at a plan. Read each tool result before deciding the next step, \
and never invent one — when a call fails, say so and adapt. Keep your \
reply grounded in what actually happened: what you did, what you \
verified, what remains. Be direct; skip filler and flattery, and state \
genuine uncertainty plainly rather than hedging around it.";

const CLAUDE_NORMS: &str = "\
Act on the operator's request without asking permission for steps the \
request already implies; save questions for genuine forks in intent. When \
several independent lookups are needed, issue those tool calls together \
rather than one at a time. Don't pad replies with restatements of the \
request or closing summaries the work doesn't need.";

const GPT_NORMS: &str = "\
Keep going until the request is fully resolved before ending your turn — \
when a step fails, try another route within what the request authorises, \
rather than returning early with a partial answer. Call tools with \
exactly the arguments their descriptions declare, and let tool results \
settle any fact you assert afterwards. Prefer several small verifiable \
steps over one large speculative one.";

/// The context element carrying the working norms for the coming turn's
/// model. Priority 105 places it after baseline orientation (110) and
/// before operator prompts (100): norms are platform baseline that
/// operator instructions extend.
pub struct WorkingNormsElement {
    family: ModelFamily,
}

impl WorkingNormsElement {
    pub fn for_model(model_id: &str) -> Self {
        Self {
            family: ModelFamily::from_model_id(model_id),
        }
    }

    fn render(&self) -> String {
        match self.family {
            ModelFamily::Claude => format!("{COMMON_NORMS}\n\n{CLAUDE_NORMS}"),
            ModelFamily::Gpt => format!("{COMMON_NORMS}\n\n{GPT_NORMS}"),
            ModelFamily::Unknown => COMMON_NORMS.to_string(),
        }
    }
}

impl ContextElement for WorkingNormsElement {
    fn name(&self) -> &str {
        "working_norms"
    }

    /// After orientation (110), before operator prompts (100).
    fn priority(&self) -> i32 {
        105
    }

    fn content(&self) -> Vec<ChatMessage> {
        vec![ChatMessage::new(Role::System, self.render())]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        // Function, not mechanism: the report says what the block is for.
        // The family is legible from the configured model, so naming it
        // here leaks nothing a reader cannot already see.
        match self.family {
            ModelFamily::Claude => "built-in working norms (Claude family)".into(),
            ModelFamily::Gpt => "built-in working norms (GPT family)".into(),
            ModelFamily::Unknown => "built-in working norms".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_detection_is_conservative() {
        assert_eq!(
            ModelFamily::from_model_id("claude-fable-5"),
            ModelFamily::Claude
        );
        assert_eq!(ModelFamily::from_model_id("gpt-5.6-sol"), ModelFamily::Gpt);
        assert_eq!(ModelFamily::from_model_id("o4-mini"), ModelFamily::Gpt);
        assert_eq!(
            ModelFamily::from_model_id("gemini-2.5-pro"),
            ModelFamily::Unknown
        );
        assert_eq!(ModelFamily::from_model_id(""), ModelFamily::Unknown);
        // "ollama" must not read as an o-series id.
        assert_eq!(ModelFamily::from_model_id("ollama"), ModelFamily::Unknown);
    }

    #[test]
    fn a_recognised_family_appends_its_norms_to_the_common_ones() {
        let text = WorkingNormsElement::for_model("claude-opus-5").content()[0].text_content();
        assert!(text.contains("## Working norms"));
        assert!(text.contains("Work to completion"));
        assert!(text.contains("issue those tool calls together"));
        assert!(!text.contains("ending your turn"), "no GPT text on Claude");

        let text = WorkingNormsElement::for_model("gpt-5.6-luna").content()[0].text_content();
        assert!(text.contains("Work to completion"));
        assert!(text.contains("fully resolved before ending your turn"));
        assert!(
            !text.contains("tool calls together"),
            "no Claude text on GPT"
        );
    }

    #[test]
    fn an_unknown_family_degrades_to_common_norms_never_silence() {
        let element = WorkingNormsElement::for_model("glm-5.2");
        let text = element.content()[0].text_content();
        assert!(text.contains("Work to completion"));
        assert!(!text.contains("tool calls together"));
        assert!(!text.contains("ending your turn"));
    }

    #[test]
    fn priority_sits_between_orientation_and_operator_prompts() {
        let element = WorkingNormsElement::for_model("claude-opus-5");
        assert!(element.priority() < 110);
        assert!(element.priority() > 100);
    }
}

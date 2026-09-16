//! Orientation element — the baseline every session starts from.
//!
//! Built-in content that survives an empty configuration: platform identity
//! (an agent running on Swage), place (topic, working directory, session
//! time, channel), and capability (the skills available to activate).
//! Operator instructions layer after it by priority and extend the baseline
//! rather than replacing it.
//!
//! The identity text is instruction text: it steers every session on the
//! platform, so edits to it are drafting-tier work, not routine maintenance.

use chrono::{DateTime, Utc};
use swage_core::{ChatMessage, Role};

use crate::element::{ContextElement, built_in_first_sentence_preview};
use crate::tokeniser::Tokeniser;

// ── Types ─────────────────────────────────────────────────────────────────────

/// What a session knows about where it is running, independent of any
/// operator configuration.
#[derive(Debug, Clone)]
pub struct SessionEnvironment {
    /// Topic name; empty means the session was started without a topic and
    /// runs in the general scope.
    pub topic_name: String,
    /// Topic description, when the operator has written one.
    pub topic_description: String,
    /// The directory tool paths resolve against, when a workspace is mounted.
    pub working_directory: Option<String>,
    /// Session start time. Rendered to the minute — stable across the
    /// session's turns, so the assembled prefix stays cache-friendly.
    pub started_at: DateTime<Utc>,
    /// Present when the session speaks through a channel (Telegram first):
    /// the model should know whose voice it is using and what that surface
    /// can render.
    pub channel: Option<ChannelOrientation>,
}

/// The channel a session speaks through, described for the model.
#[derive(Debug, Clone)]
pub struct ChannelOrientation {
    /// The channel's name as the operator knows it, e.g. "Telegram".
    pub channel_name: String,
    /// What the surface renders and expects, stated as guidance the model
    /// can act on (message length, formatting support).
    pub constraints: String,
}

/// A skill the session could activate: its name and trigger description.
/// This is the catalogue — distinct from the active skills whose full
/// instructions the skills element carries.
#[derive(Debug, Clone)]
pub struct AvailableSkill {
    pub name: String,
    pub trigger: String,
}

// ── Element ───────────────────────────────────────────────────────────────────

const IDENTITY: &str = "\
You are an agent running on Swage, a context-governed agent platform on your \
operator's own hardware. A session may run inside a topic — a container \
holding its conversations, the topic's memory, and any attached workspaces — \
or without one, as a general assistant; the place section below says which.

Everything you do here is recorded locally on the operator's machine and is \
reconstructable afterwards: your turns, your tool calls and their results, \
and how this context was assembled. Values of secrets the platform manages \
are redacted from that record by design — so use a registered secret freely \
where a tool needs it, and keep those values out of any text you author \
(replies, files, briefs for other agents), which is recorded as written.

You have tools; each description states what the tool is for. Some tool \
calls pause for approval before running — the platform is asking your \
operator, not reporting an error: wait for the outcome rather than \
retrying, and continue with the approved result or work around a \
rejection. File paths in tool calls resolve against your working \
directory unless a tool says otherwise.

Where tools overlap, prefer the specific one. The topic's memory is yours \
to use — reach for `agent_memory` to remember and recall what you learn \
across sessions, and `memory` only to work with the topic's memory files \
directly. Use `web_search` to find web sources and `url_fetch` to read a \
URL you already have; reusable work surfaces are found with \
`surface_library`, never on the web. Use `workflow_run` for a saved, journalled multi-step \
job and `delegate` for a one-off child task. Prefer your native \
file and shell tools over `python_execute` unless the task is genuinely \
computation.";

/// The context element carrying baseline orientation.
///
/// Priority 110 places it above operator prompts (100): the baseline comes
/// first and operator instructions extend it. Unlike most elements it never
/// produces an empty message list — a fresh install with no configuration is
/// exactly the case it exists for.
pub struct OrientationElement {
    environment: SessionEnvironment,
    available_skills: Vec<AvailableSkill>,
}

impl OrientationElement {
    pub fn new(environment: SessionEnvironment, available_skills: Vec<AvailableSkill>) -> Self {
        Self {
            environment,
            available_skills,
        }
    }

    fn render(&self) -> String {
        let mut blocks = vec![IDENTITY.to_string()];

        let mut place = String::from("## Where you are\n");
        if self.environment.topic_name.is_empty() {
            // Stated, never left to inference: a session that was started
            // without a topic must not read a previous topic as current.
            place.push_str(
                "Topic: none — this session was started without a topic. It runs as a general \
                 assistant with general memory and the general knowledge base, and inherits \
                 nothing from any topic.\n",
            );
        } else {
            place.push_str(&format!("Topic: {}", self.environment.topic_name));
            if !self.environment.topic_description.is_empty() {
                place.push_str(&format!(" — {}", self.environment.topic_description));
            }
            place.push('\n');
        }
        match &self.environment.working_directory {
            Some(dir) => place.push_str(&format!("Working directory: {dir}\n")),
            None => place.push_str("Working directory: none is attached.\n"),
        }
        place.push_str(&format!(
            "Session started: {}\n",
            self.environment.started_at.format("%Y-%m-%d %H:%M UTC")
        ));
        if let Some(channel) = &self.environment.channel {
            place.push_str(&format!(
                "You are speaking with your operator over {}. {}\n",
                channel.channel_name, channel.constraints
            ));
        }
        blocks.push(place.trim_end().to_string());

        if !self.available_skills.is_empty() {
            let mut skills = String::from(
                "## Skills available in this session\n\
                 Each skill adds instructions for a kind of task. Your \
                 operator activates one by typing /its-name; when a task fits \
                 a skill's trigger and it is not active, say so and suggest \
                 it:\n",
            );
            for skill in &self.available_skills {
                skills.push_str(&format!("- {} — {}\n", skill.name, skill.trigger));
            }
            blocks.push(skills.trim_end().to_string());
        }

        blocks.join("\n\n")
    }
}

impl ContextElement for OrientationElement {
    fn name(&self) -> &str {
        "orientation"
    }

    /// Priority 110: baseline orientation precedes operator prompts (100) so
    /// operator instructions extend the baseline rather than replace it.
    fn priority(&self) -> i32 {
        110
    }

    fn content(&self) -> Vec<ChatMessage> {
        vec![ChatMessage::new(Role::System, self.render())]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        let mut parts = vec!["built-in identity".to_string()];
        if self.environment.channel.is_some() {
            parts.push("channel orientation".to_string());
        }
        if !self.available_skills.is_empty() {
            parts.push(format!("{} available skills", self.available_skills.len()));
        }
        parts.join(", ")
    }

    fn report_preview(&self) -> Option<String> {
        Some(built_in_first_sentence_preview(IDENTITY))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_environment() -> SessionEnvironment {
        SessionEnvironment {
            topic_name: String::new(),
            topic_description: String::new(),
            working_directory: None,
            started_at: DateTime::parse_from_rfc3339("2026-08-09T18:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            channel: None,
        }
    }

    /// The fresh-install case this element exists for: no configuration at
    /// all still produces a system message with identity and place.
    #[test]
    fn empty_configuration_still_orients() {
        let element = OrientationElement::new(bare_environment(), vec![]);

        let content = element.content();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].role(), Role::System);

        let text = content[0].text_content();
        assert!(text.contains("agent running on Swage"));
        assert!(text.contains("Topic: none — this session was started without a topic"));
        assert!(text.contains("inherits nothing from any topic"));
        assert!(text.contains("Working directory: none is attached."));
        assert!(text.contains("2026-08-09 18:30 UTC"));
    }

    /// The session is told what the record covers and that registered
    /// secret values are redacted from it — so it neither over- nor
    /// under-shares on a guess about where secrets go.
    #[test]
    fn identity_states_record_coverage_and_secret_redaction() {
        let element = OrientationElement::new(bare_environment(), vec![]);
        let text = element.content()[0].text_content();
        assert!(text.contains("your turns, your tool calls and their results"));
        assert!(text.contains("how this context was assembled"));
        assert!(text.contains("redacted from that record"));
        assert!(text.contains("recorded as written"));
    }

    #[test]
    fn report_preview_is_a_static_excerpt_not_session_environment() {
        let element = OrientationElement::new(bare_environment(), vec![]);
        let content = element.content()[0].text_content();
        let preview = element.report_preview().expect("built-in preview");
        let opening_sentence = built_in_first_sentence_preview(&content);

        assert!(!preview.is_empty());
        assert_eq!(preview, opening_sentence);
        assert!(!preview.contains("Session started:"));
    }

    #[test]
    fn priority_places_orientation_before_operator_prompts() {
        let element = OrientationElement::new(bare_environment(), vec![]);
        // PromptsElement sits at 100; the baseline must sort ahead of it.
        assert!(element.priority() > 100);
    }

    #[test]
    fn channel_block_renders_only_for_channel_sessions() {
        let plain = OrientationElement::new(bare_environment(), vec![]);
        assert!(!plain.content()[0].text_content().contains("speaking with"));

        let mut environment = bare_environment();
        environment.channel = Some(ChannelOrientation {
            channel_name: "Telegram".into(),
            constraints: "Replies render as plain text messages; keep them \
                          short and use no markdown tables."
                .into(),
        });
        let with_channel = OrientationElement::new(environment, vec![]);
        let text = with_channel.content()[0].text_content();
        assert!(text.contains("speaking with your operator over Telegram"));
        assert!(text.contains("plain text messages"));
    }

    #[test]
    fn skills_catalogue_lists_names_and_triggers_not_instructions() {
        let element = OrientationElement::new(
            bare_environment(),
            vec![AvailableSkill {
                name: "release-notes".into(),
                trigger: "drafting release notes for a tagged build".into(),
            }],
        );

        let text = element.content()[0].text_content();
        assert!(text.contains("release-notes — drafting release notes"));
    }

    #[test]
    fn shipped_provenance_is_not_exposed_in_the_model_catalogue() {
        let element = OrientationElement::new(
            bare_environment(),
            vec![AvailableSkill {
                name: "commission".into(),
                trigger: "shape project intent".into(),
            }],
        );

        let text = element.content()[0].text_content();
        assert!(text.contains("commission — shape project intent"));
        assert!(!text.contains("(shipped)"));
    }

    #[test]
    fn topic_and_workspace_render_when_present() {
        let mut environment = bare_environment();
        environment.topic_name = "research".into();
        environment.topic_description = "material science digging".into();
        environment.working_directory = Some("/data/projects/alloys".into());

        let element = OrientationElement::new(environment, vec![]);
        let text = element.content()[0].text_content();
        assert!(text.contains("Topic: research — material science digging"));
        assert!(text.contains("Working directory: /data/projects/alloys"));
    }

    #[test]
    fn source_names_function_not_mechanism() {
        let element = OrientationElement::new(bare_environment(), vec![]);
        assert_eq!(element.source(), "built-in identity");
    }
}

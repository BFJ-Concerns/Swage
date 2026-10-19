//! Orientation element — the baseline every session starts from.
//!
//! Built-in content that survives an empty configuration: platform identity
//! (an agent running on Swage), place (topic, working directory, session
//! time, channel), and capability (the skills available to activate).
//! Operator instructions layer after it by priority and extend the baseline
//! rather than replacing it.
//!
//! The identity text and the tool preferences are instruction text: they
//! steer every session on the platform, so edits to them are drafting-tier
//! work, not routine maintenance. A preference names a tool, so it is stated
//! only on a turn that offers that tool.

use std::collections::BTreeSet;

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
    /// The directory tool paths resolve against and where that path came from.
    pub tool_root: ToolRoot,
    /// Session start time. Rendered to the minute — stable across the
    /// session's turns, so the assembled prefix stays cache-friendly.
    pub started_at: DateTime<Utc>,
    /// Present when the session speaks through a channel (Telegram first):
    /// the model should know whose voice it is using and what that surface
    /// can render.
    pub channel: Option<ChannelOrientation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRoot {
    /// An attached workspace or a directory chosen for a placed worker.
    WorkingDirectory(String),
    /// A scratch directory used when no workspace is attached.
    ManagedScratch(String),
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
directory unless a tool says otherwise.";

/// A preference between overlapping tools. It is stated only on a turn that
/// offers the tool it names, so a session without the tool is never told to
/// use it.
struct ToolPreference {
    /// The tool's wire name, as the request's tool list carries it.
    tool: &'static str,
    sentence: &'static str,
}

const PREFERENCES_OPENING: &str = "Where tools overlap, prefer the specific one.";

/// The preferences in reading order, one tool each. The gateway holds every
/// name here to a registered tool, so a renamed tool fails that test rather
/// than silently losing its sentence.
const TOOL_PREFERENCES: &[ToolPreference] = &[
    ToolPreference {
        tool: "web_search",
        sentence: "Use `web_search` to find web sources.",
    },
    ToolPreference {
        tool: "url_fetch",
        sentence: "Use `url_fetch` to read a URL you already have.",
    },
    ToolPreference {
        tool: "surface_library",
        sentence: "Reusable work surfaces are found with `surface_library`, never on the web.",
    },
    ToolPreference {
        tool: "workflow_run",
        sentence: "Use `workflow_run` for a saved, journalled multi-step job.",
    },
    ToolPreference {
        tool: "delegate",
        sentence: "Use `delegate` for a one-off child task.",
    },
    ToolPreference {
        tool: "python_execute",
        sentence: "Prefer your native file and shell tools over `python_execute` unless the task is \
genuinely computation.",
    },
];

/// The wire names the tool preferences depend on.
pub fn tools_named_in_preferences() -> impl Iterator<Item = &'static str> {
    TOOL_PREFERENCES.iter().map(|preference| preference.tool)
}

/// The context element carrying baseline orientation.
///
/// Priority 110 places it above operator prompts (100): the baseline comes
/// first and operator instructions extend it. Unlike most elements it never
/// produces an empty message list — a fresh install with no configuration is
/// exactly the case it exists for.
pub struct OrientationElement {
    environment: SessionEnvironment,
    available_skills: Vec<AvailableSkill>,
    /// The tools this turn offers the model, by wire name — the request's
    /// own tool list, so a preference never names a tool the model lacks.
    offered_tools: BTreeSet<String>,
}

impl OrientationElement {
    pub fn new(
        environment: SessionEnvironment,
        available_skills: Vec<AvailableSkill>,
        offered_tools: BTreeSet<String>,
    ) -> Self {
        Self {
            environment,
            available_skills,
            offered_tools,
        }
    }

    fn render(&self) -> String {
        let mut blocks = vec![IDENTITY.to_string()];

        let mut preferences = vec![PREFERENCES_OPENING];
        preferences.extend(
            TOOL_PREFERENCES
                .iter()
                .filter(|preference| self.offered_tools.contains(preference.tool))
                .map(|preference| preference.sentence),
        );
        blocks.push(preferences.join(" "));

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
        match &self.environment.tool_root {
            ToolRoot::WorkingDirectory(dir) => {
                place.push_str(&format!(
                    "Working directory: {dir} — temporary files belong under it, not in /tmp.\n"
                ));
            }
            ToolRoot::ManagedScratch(dir) => {
                place.push_str(&format!(
                    "No workspace is attached. Tool paths resolve at {dir}, a scratch directory for this work; temporary files belong under it, not in /tmp.\n"
                ));
            }
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
            tool_root: ToolRoot::ManagedScratch("/scratch/general/sessions/example".into()),
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
        let element = OrientationElement::new(bare_environment(), vec![], BTreeSet::new());

        let content = element.content();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].role(), Role::System);

        let text = content[0].text_content();
        assert!(text.contains("agent running on Swage"));
        assert!(text.contains("Topic: none — this session was started without a topic"));
        assert!(text.contains("inherits nothing from any topic"));
        assert!(text.contains("No workspace is attached."));
        assert!(text.contains("Tool paths resolve at /scratch/general/sessions/example"));
        assert!(text.contains("a scratch directory for this work"));
        assert!(text.contains("2026-08-09 18:30 UTC"));
    }

    /// The session is told what the record covers and that registered
    /// secret values are redacted from it — so it neither over- nor
    /// under-shares on a guess about where secrets go.
    /// A tool preference is stated only to a session whose turn offers the
    /// tool: a file-only turn hears of none, and a turn offering one tool
    /// hears that one alone. Memory guidance is the memory element's.
    #[test]
    fn tool_preferences_follow_the_tools_the_turn_offers() {
        let file_only = OrientationElement::new(bare_environment(), vec![], BTreeSet::new());
        let text = file_only.content()[0].text_content();
        assert!(text.contains("Where tools overlap, prefer the specific one."));
        for tool in tools_named_in_preferences() {
            assert!(
                !text.contains(&format!("`{tool}`")),
                "{tool} named without being offered: {text}"
            );
        }
        assert!(!text.contains("memory_read"), "{text}");

        let with_search = OrientationElement::new(
            bare_environment(),
            vec![],
            BTreeSet::from(["web_search".to_string()]),
        );
        let text = with_search.content()[0].text_content();
        assert!(text.contains("Use `web_search` to find web sources."));
        assert!(!text.contains("`url_fetch`"), "{text}");
    }

    #[test]
    fn identity_states_record_coverage_and_secret_redaction() {
        let element = OrientationElement::new(bare_environment(), vec![], BTreeSet::new());
        let text = element.content()[0].text_content();
        assert!(text.contains("your turns, your tool calls and their results"));
        assert!(text.contains("how this context was assembled"));
        assert!(text.contains("redacted from that record"));
        assert!(text.contains("recorded as written"));
    }

    #[test]
    fn report_preview_is_a_static_excerpt_not_session_environment() {
        let element = OrientationElement::new(bare_environment(), vec![], BTreeSet::new());
        let content = element.content()[0].text_content();
        let preview = element.report_preview().expect("built-in preview");
        let opening_sentence = built_in_first_sentence_preview(&content);

        assert!(!preview.is_empty());
        assert_eq!(preview, opening_sentence);
        assert!(!preview.contains("Session started:"));
    }

    #[test]
    fn priority_places_orientation_before_operator_prompts() {
        let element = OrientationElement::new(bare_environment(), vec![], BTreeSet::new());
        // PromptsElement sits at 100; the baseline must sort ahead of it.
        assert!(element.priority() > 100);
    }

    #[test]
    fn channel_block_renders_only_for_channel_sessions() {
        let plain = OrientationElement::new(bare_environment(), vec![], BTreeSet::new());
        assert!(!plain.content()[0].text_content().contains("speaking with"));

        let mut environment = bare_environment();
        environment.channel = Some(ChannelOrientation {
            channel_name: "Telegram".into(),
            constraints: "Replies render as plain text messages; keep them \
                          short and use no markdown tables."
                .into(),
        });
        let with_channel = OrientationElement::new(environment, vec![], BTreeSet::new());
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
            BTreeSet::new(),
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
            BTreeSet::new(),
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
        environment.tool_root = ToolRoot::WorkingDirectory("/data/projects/alloys".into());

        let element = OrientationElement::new(environment, vec![], BTreeSet::new());
        let text = element.content()[0].text_content();
        assert!(text.contains("Topic: research — material science digging"));
        assert!(text.contains("Working directory: /data/projects/alloys"));
        assert!(!text.contains("No workspace is attached"));
    }

    #[test]
    fn topic_without_workspace_names_its_scratch_root() {
        let mut environment = bare_environment();
        environment.topic_name = "research".into();
        environment.tool_root =
            ToolRoot::ManagedScratch("/scratch/topics/research/sessions/42".into());

        let text = OrientationElement::new(environment, vec![], BTreeSet::new()).content()[0]
            .text_content();
        assert!(text.contains("Topic: research"));
        assert!(text.contains("No workspace is attached."));
        assert!(text.contains("Tool paths resolve at /scratch/topics/research/sessions/42"));
        assert!(!text.contains("Working directory:"));
    }

    #[test]
    fn source_names_function_not_mechanism() {
        let element = OrientationElement::new(bare_environment(), vec![], BTreeSet::new());
        assert_eq!(element.source(), "built-in identity");
    }
}

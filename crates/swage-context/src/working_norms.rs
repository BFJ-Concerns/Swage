//! Working-norms element — what a model's own harness would have told it.
//!
//! Models arrive over the operator's proxy bare, with none of the prompt
//! scaffolding their vendor harnesses supply, so the built-in assembly
//! provides the equivalent: working norms adapted to Swage's tools, selected
//! by the coming turn's exact model id. A model-specific native scaffold is
//! preferred; the established family text remains the conservative fallback.
//!
//! Every string here is instruction text: it steers each session of a whole
//! model family, so edits are drafting-tier work, not routine maintenance.
//! The block is also an experimentation location (`context.working_norms`) —
//! alternative texts compete through the ordinary switchable/forced seam,
//! and nothing model-facing names the mechanism.

use swage_core::{ChatMessage, Role};

use crate::element::{ContextElement, built_in_first_sentence_preview};
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
verified, what remains. When you summarise tool outcomes — your own \
calls or dispatched work — disclose the failures among them and what \
they leave uncovered: a report silent about its errored calls \
overstates its coverage. Be direct; skip filler and flattery, and state \
genuine uncertainty plainly rather than hedging around it. Temporary \
files, scratch work and working directories go under the place your \
orientation names for tool paths — the working directory, or the scratch \
directory when no workspace is attached — never under /tmp or anywhere \
outside the attached workspaces. When you state a measured property of \
something you delivered — a word count, a length, a limit met — measure it \
with a tool first and report the number you measured; where you have not \
measured, say so rather than asserting that the limit is met.";

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

const CLAUDE_NATIVE: &str = "\
You are an interactive agent helping with software-engineering tasks. Text \
outside tool use is shown to the operator as GitHub-flavoured Markdown. A \
denied tool call means the operator declined it: adapt instead of retrying it \
unchanged. Prefer a dedicated file or search tool when one fits, and issue \
independent tool calls together. Write code in the surrounding style. Confirm \
hard-to-reverse or outward-facing actions unless they are already authorised, \
inspect before overwriting, and report failures and skipped checks plainly.";

const CLAUDE_FABLE_NATIVE: &str = "\
Write for a teammate returning to the work, not as a process log. Give a brief \
update before the first tool call and when a load-bearing fact changes the \
direction. Put every conclusion the operator needs in the final response and \
lead it with the outcome. Prefer readable complete sentences over compressed \
shorthand. Comment code only where the code cannot express the constraint. \
Operate autonomously on reversible work already implied by the request; stop \
only for destructive action or a genuine scope decision.";

const CLAUDE_OPUS_NATIVE: &str = "\
Do the requested work at its stated scope, making routine judgement calls \
yourself and reserving questions for materially different interpretations. \
If one part is blocked, finish the rest and state the gap. Raise a real concern \
briefly, then continue under an explicit assumption where that is safe. Correct \
only errors that would change the operator's code, conclusion, or decision.";

const CLAUDE_5_5_NATIVE: &str = "\
When you have enough information to act, act: do not re-derive facts the \
conversation has already established, re-litigate a decision the operator \
has made, or narrate options you will not pursue, and when weighing a choice \
give a recommendation rather than a survey. Approval given in one context \
does not extend to the next, and sending content to an external service \
publishes it. Match the surrounding code's comment density, naming and idiom. \
Use they/them for anyone whose pronouns have not been stated; a name does not \
tell you someone's pronouns, so never infer them from one.";

const GPT_NATIVE: &str = "\
Collaborate with the operator until the goal is genuinely handled. Lead with \
the outcome, use plain language, and match detail to the reader. Keep progress \
updates brief and restate every conclusion needed in the final response. Use \
fast targeted search, parallelise independent work, preserve unrelated changes, \
and verify implementation work in proportion to risk. Make safe in-scope \
assumptions; stop when completion needs new authority or a material scope choice.";

const GPT_6_NATIVE: &str = "\
You and the operator share one workspace; collaborate until their intended \
goal is completely handled. Judge when you need permission as a competent \
colleague would: authorisation given earlier in the session persists, so do \
not ask again for it; reversible work, read-only actions, reviews and fixes \
need none; and before asking permission for an outward step, do all the work \
that is already authorised so the operator approves a concrete, reviewable \
result. Bias towards action: a request phrased as \"can you\" or \"help me\" \
is an instruction to do the work, so do not stop at a plan, an \
acknowledgement, or a partial solution, and when scope is unclear progress \
with what you have and ask while continuing. State the main point early in \
plain, concrete language with active verbs; state the intended action \
directly, without contrastive framing about what you will not do, without \
invented compound labels, filler phrases, or needless apologies. Open with a \
brief update before the first tool call and keep the final answer \
self-contained, led by the outcome. Batch independent searches and reads; \
keep edits, approvals and dependent steps sequential. Treat shell command \
text as code and quote it properly. Run the checks appropriate to the \
change, and do not write tests that merely mirror the implementation. Apply \
a skill when it would improve the outcome, say so the first time you do, and \
let the operator's instructions outrank a skill's.";

const GPT_6_1_SOL_NATIVE: &str = "\
Adapt your tone to the operator and write in connected prose, developing \
each point with the support it needs and avoiding section headings and \
closing summaries. When discussing technical work, lead with the outcome and \
then the reasoning: what changed, why, how it was tested, and the material \
risks, with evidence in the order that makes the conclusion easiest to \
assess rather than the order you worked. Summarise routine verification \
instead of listing every check; in progress updates say what you learned, \
what remains uncertain, and what the next step resolves. A pull-request \
description leads with the concrete problem and resulting behaviour, scaled \
to the change, written for a reviewer who has not seen the conversation.";

const GPT_6_LUNA_NATIVE: &str = "\
Be a simple, clear communicator: converse as you would with a colleague, and \
minimise the reader's cognitive load so they understand your response on \
first read. Prefer familiar words and concrete descriptions over abstract or \
technical language when they carry the same meaning, and never assume the \
reader will fill in missing steps. Give each paragraph one main point, in an \
order the reader can follow; when reporting changes, say what changed, why, \
how it was tested, and any material risks.";

const GPT_SOL_NATIVE: &str = "\
For work explicitly authorised for delegation, coordinate independent tasks \
as one shared effort and treat worker results as evidence to inspect before \
adopting them.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingNormsScaffold {
    Native,
    FamilyFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergenceCategory {
    ProprietaryToolMaterial,
    DynamicVendorContext,
}

#[derive(Debug, Clone, Copy)]
pub struct Divergence {
    pub category: DivergenceCategory,
    pub material: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ModelScaffold {
    model_id: &'static str,
    family_text: &'static str,
    model_text: &'static str,
    divergences: &'static [Divergence],
}

const STANDARD_DIVERGENCES: &[Divergence] = &[
    Divergence {
        category: DivergenceCategory::ProprietaryToolMaterial,
        material: "vendor tool definitions, tool-specific instructions, permission mechanics, and client-only control channels",
    },
    Divergence {
        category: DivergenceCategory::DynamicVendorContext,
        material: "working directory, date, platform, sandbox state, available capabilities, skills, memory paths, and context-window notices injected at request time",
    },
];

const GPT_SOL_DIVERGENCES: &[Divergence] = &[
    Divergence {
        category: DivergenceCategory::ProprietaryToolMaterial,
        material: "vendor tool definitions, tool-specific instructions, permission mechanics, client-only control channels, and the native collaboration-tool protocol",
    },
    STANDARD_DIVERGENCES[1],
];

const GPT_6_DIVERGENCES: &[Divergence] = &[
    Divergence {
        category: DivergenceCategory::ProprietaryToolMaterial,
        material: "vendor tool definitions (the JavaScript orchestration tool and its nested tools, goals, the sub-agent collaboration protocol, apps and plugins), the commentary/final channel mechanics, and permission mechanics",
    },
    Divergence {
        category: DivergenceCategory::DynamicVendorContext,
        material: "skills catalogue and roots, working directory, sandbox mode, and the per-turn multi-agent mode notice injected at request time",
    },
];

const MODEL_SCAFFOLDS: &[ModelScaffold] = &[
    ModelScaffold {
        model_id: "claude-opus-5-5",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_5_5_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-opus-5-5[1m]",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_5_5_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-sonnet-5-5",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_5_5_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-sonnet-5-5[1m]",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_5_5_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "gpt-6.1-sol",
        family_text: GPT_6_NATIVE,
        model_text: GPT_6_1_SOL_NATIVE,
        divergences: GPT_6_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "gpt-6-luna",
        family_text: GPT_6_NATIVE,
        model_text: GPT_6_LUNA_NATIVE,
        divergences: GPT_6_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-fable-5",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_FABLE_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-fable-5[1m]",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_FABLE_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-opus-5",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_OPUS_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-opus-5[1m]",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_OPUS_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-opus-4-8",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_OPUS_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-opus-4-8[1m]",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_OPUS_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-opus-4-6",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_OPUS_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-opus-4-6[1m]",
        family_text: CLAUDE_NATIVE,
        model_text: CLAUDE_OPUS_NATIVE,
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-sonnet-5",
        family_text: CLAUDE_NATIVE,
        model_text: "",
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-sonnet-5[1m]",
        family_text: CLAUDE_NATIVE,
        model_text: "",
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "claude-haiku-4-5-20251001",
        family_text: CLAUDE_NATIVE,
        model_text: "",
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "gpt-5.6-luna",
        family_text: GPT_NATIVE,
        model_text: "",
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "gpt-5.6-terra",
        family_text: GPT_NATIVE,
        model_text: "",
        divergences: STANDARD_DIVERGENCES,
    },
    ModelScaffold {
        model_id: "gpt-5.6-sol",
        family_text: GPT_NATIVE,
        model_text: GPT_SOL_NATIVE,
        divergences: GPT_SOL_DIVERGENCES,
    },
];

/// The context element carrying the working norms for the coming turn's
/// model. Priority 105 places it after baseline orientation (110) and
/// before operator prompts (100): norms are platform baseline that
/// operator instructions extend.
pub struct WorkingNormsElement {
    family: ModelFamily,
    scaffold: Option<&'static ModelScaffold>,
}

impl WorkingNormsElement {
    pub fn for_model(model_id: &str, selection: WorkingNormsScaffold) -> Self {
        Self {
            family: ModelFamily::from_model_id(model_id),
            scaffold: (selection == WorkingNormsScaffold::Native)
                .then(|| exact_scaffold(model_id))
                .flatten(),
        }
    }

    fn render(&self) -> String {
        if let Some(scaffold) = self.scaffold {
            return [COMMON_NORMS, scaffold.family_text, scaffold.model_text]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        match self.family {
            ModelFamily::Claude => format!("{COMMON_NORMS}\n\n{CLAUDE_NORMS}"),
            ModelFamily::Gpt => format!("{COMMON_NORMS}\n\n{GPT_NORMS}"),
            ModelFamily::Unknown => COMMON_NORMS.to_string(),
        }
    }
}

fn exact_scaffold(model_id: &str) -> Option<&'static ModelScaffold> {
    let model_id = model_id.trim();
    MODEL_SCAFFOLDS
        .iter()
        .find(|scaffold| scaffold.model_id.eq_ignore_ascii_case(model_id))
}

/// Return the audit register for material intentionally omitted from an exact
/// model scaffold. The register is product evidence, not model instructions,
/// so it is never included in [`WorkingNormsElement`] content.
pub fn native_harness_divergences(model_id: &str) -> Option<&'static [Divergence]> {
    exact_scaffold(model_id).map(|scaffold| scaffold.divergences)
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

    fn report_preview(&self) -> Option<String> {
        Some(built_in_first_sentence_preview(COMMON_NORMS))
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
        let text =
            WorkingNormsElement::for_model("claude-opus-5", WorkingNormsScaffold::FamilyFallback)
                .content()[0]
                .text_content();
        assert!(text.contains("## Working norms"));
        assert!(
            text.contains("never under /tmp"),
            "the norms place temporary output under the session's own root"
        );
        assert!(text.contains("Work to completion"));
        assert!(text.contains("issue those tool calls together"));
        assert!(!text.contains("ending your turn"), "no GPT text on Claude");

        let text =
            WorkingNormsElement::for_model("gpt-5.6-luna", WorkingNormsScaffold::FamilyFallback)
                .content()[0]
                .text_content();
        assert!(text.contains("Work to completion"));
        assert!(text.contains("fully resolved before ending your turn"));
        assert!(
            !text.contains("tool calls together"),
            "no Claude text on GPT"
        );
    }

    #[test]
    fn an_unknown_family_degrades_to_common_norms_never_silence() {
        let element = WorkingNormsElement::for_model("glm-5.2", WorkingNormsScaffold::Native);
        let text = element.content()[0].text_content();
        assert!(text.contains("Work to completion"));
        assert!(!text.contains("tool calls together"));
        assert!(!text.contains("ending your turn"));
    }

    #[test]
    fn priority_sits_between_orientation_and_operator_prompts() {
        let element =
            WorkingNormsElement::for_model("claude-opus-5", WorkingNormsScaffold::FamilyFallback);
        assert!(element.priority() < 110);
        assert!(element.priority() > 100);
    }

    #[test]
    fn report_preview_is_a_static_excerpt_of_the_common_norms() {
        let element = WorkingNormsElement::for_model("gpt-4o", WorkingNormsScaffold::Native);
        let content = element.content()[0].text_content();
        let preview = element.report_preview().expect("built-in preview");

        assert!(content.starts_with(&preview));
        assert!(preview.ends_with("stopping at a plan."));
    }

    #[test]
    fn exact_models_receive_native_scaffolds_with_a_separate_divergence_register() {
        let text = WorkingNormsElement::for_model(" CLAUDE-FABLE-5 ", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();

        assert!(text.contains("Write for a teammate returning to the work"));
        assert!(!text.contains("Native harness divergences"));

        let divergences = native_harness_divergences(" CLAUDE-FABLE-5 ").unwrap();
        assert_eq!(divergences.len(), 2);
        assert_eq!(
            divergences[0].category,
            DivergenceCategory::ProprietaryToolMaterial
        );
        assert_eq!(
            divergences[1].category,
            DivergenceCategory::DynamicVendorContext
        );
    }

    #[test]
    fn exact_model_miss_conservatively_uses_the_existing_family_fallback() {
        let text = WorkingNormsElement::for_model("claude-future-6", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();

        assert!(text.contains("issue those tool calls together"));
        assert!(!text.contains("Native harness divergences"));
        assert!(!text.contains("Write for a teammate returning to the work"));
    }

    #[test]
    fn two_models_in_one_family_differ_where_their_native_harnesses_differ() {
        let luna = WorkingNormsElement::for_model("gpt-5.6-luna", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();
        let sol = WorkingNormsElement::for_model("gpt-5.6-sol", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();

        assert_ne!(luna, sol);
        assert!(sol.contains("coordinate independent tasks"));
        assert!(!luna.contains("coordinate independent tasks"));
        assert!(
            native_harness_divergences("gpt-5.6-sol").unwrap()[0]
                .material
                .contains("native collaboration-tool protocol")
        );
    }

    #[test]
    fn models_without_a_sourced_native_difference_do_not_invent_one() {
        let luna = WorkingNormsElement::for_model("gpt-5.6-luna", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();
        let terra = WorkingNormsElement::for_model("gpt-5.6-terra", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();
        let sonnet =
            WorkingNormsElement::for_model("claude-sonnet-5", WorkingNormsScaffold::Native)
                .content()[0]
                .text_content();
        let haiku = WorkingNormsElement::for_model(
            "claude-haiku-4-5-20251001",
            WorkingNormsScaffold::Native,
        )
        .content()[0]
            .text_content();

        assert_eq!(luna, terra);
        assert_eq!(sonnet, haiku);
    }

    #[test]
    fn every_family_block_carries_the_measured_claims_rule() {
        for (model, selection) in [
            ("claude-opus-5-5", WorkingNormsScaffold::Native),
            ("claude-opus-5-5", WorkingNormsScaffold::FamilyFallback),
            ("gpt-6.1-sol", WorkingNormsScaffold::Native),
            ("gpt-6-luna", WorkingNormsScaffold::FamilyFallback),
            ("glm-5.2", WorkingNormsScaffold::Native),
        ] {
            let text = WorkingNormsElement::for_model(model, selection).content()[0].text_content();
            assert!(
                text.contains("measure it with a tool first"),
                "{model} ({selection:?}) lacks the measured-claims rule"
            );
            assert!(text.contains("say so rather than asserting that the limit is met"));
        }
    }

    #[test]
    fn current_models_receive_native_scaffolds_rather_than_the_family_fallback() {
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "gpt-6.1-sol",
            "gpt-6-luna",
        ] {
            let native = WorkingNormsElement::for_model(model, WorkingNormsScaffold::Native)
                .content()[0]
                .text_content();
            let fallback =
                WorkingNormsElement::for_model(model, WorkingNormsScaffold::FamilyFallback)
                    .content()[0]
                    .text_content();
            assert_ne!(native, fallback, "{model} has no exact scaffold");
            assert!(
                native_harness_divergences(model).is_some(),
                "{model} has no divergence register"
            );
        }
    }

    #[test]
    fn claude_5_5_models_share_one_scaffold_because_their_native_prompts_match() {
        // The 2026-10-01 harness capture shows Opus 5.5 and Sonnet 5.5 served
        // the same Claude Code prompt, differing only in dynamic context, so a
        // differentiated scaffold would be invented rather than sourced.
        let opus = WorkingNormsElement::for_model("claude-opus-5-5", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();
        let sonnet =
            WorkingNormsElement::for_model("claude-sonnet-5-5", WorkingNormsScaffold::Native)
                .content()[0]
                .text_content();
        assert_eq!(opus, sonnet);
        assert!(opus.contains("When you have enough information to act, act"));
        assert!(!opus.contains("ending your turn"), "no GPT text on Claude");
    }

    #[test]
    fn gpt_6_models_differ_where_their_developer_prompts_differ() {
        let sol = WorkingNormsElement::for_model("gpt-6.1-sol", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();
        let luna = WorkingNormsElement::for_model("gpt-6-luna", WorkingNormsScaffold::Native)
            .content()[0]
            .text_content();
        assert_ne!(sol, luna);
        for text in [&sol, &luna] {
            assert!(
                text.contains("share one workspace"),
                "shared GPT-6 base missing"
            );
            assert!(
                !text.contains("tool calls together"),
                "no Claude text on GPT"
            );
        }
        assert!(sol.contains("A pull-request description leads with the concrete problem"));
        assert!(!luna.contains("pull-request description"));
        assert!(luna.contains("minimise the reader's cognitive load"));
        assert!(!sol.contains("cognitive load"));
        assert!(
            native_harness_divergences("gpt-6-luna").unwrap()[0]
                .material
                .contains("sub-agent collaboration protocol")
        );
    }

    #[test]
    fn family_fallback_selection_bypasses_an_available_exact_scaffold() {
        let text =
            WorkingNormsElement::for_model("gpt-5.6-sol", WorkingNormsScaffold::FamilyFallback)
                .content()[0]
                .text_content();

        assert!(text.contains("fully resolved before ending your turn"));
        assert!(!text.contains("Native harness divergences"));
        assert!(!text.contains("coordinate independent tasks"));
    }
}

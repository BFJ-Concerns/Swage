use super::*;
use swage_core::Role;

// ── Test fixtures ─────────────────────────────────────────────────────────

/// Deterministic tokeniser: each message costs exactly `cost` tokens.
struct FixedCostTokeniser {
    cost: usize,
}

impl Tokeniser for FixedCostTokeniser {
    fn count_text(&self, _text: &str) -> usize {
        self.cost
    }

    fn count_messages(&self, messages: &[ChatMessage]) -> usize {
        messages.len() * self.cost
    }
}

struct MockElement {
    name: &'static str,
    priority: i32,
    messages: Vec<ChatMessage>,
    source: &'static str,
}

impl ContextElement for MockElement {
    fn name(&self) -> &str {
        self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn content(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.messages)
    }

    fn source(&self) -> String {
        self.source.to_string()
    }
}

fn prompts_element(system_text: &str) -> Box<dyn ContextElement> {
    Box::new(MockElement {
        name: "prompts",
        priority: 100,
        messages: vec![ChatMessage::new(Role::System, system_text)],
        source: "prompt rules",
    })
}

fn history_element(turns: &[(&str, &str)]) -> Box<dyn ContextElement> {
    let messages = turns
        .iter()
        .flat_map(|(user, asst)| {
            [
                ChatMessage::new(Role::User, *user),
                ChatMessage::new(Role::Assistant, *asst),
            ]
        })
        .collect();
    Box::new(MockElement {
        name: "history",
        priority: 0,
        messages,
        source: "thread history",
    })
}

fn memory_element() -> Box<dyn ContextElement> {
    Box::new(MockElement {
        name: "memory",
        priority: 103,
        messages: vec![ChatMessage::new(Role::System, "some memory content")],
        source: "operator profile and memory index",
    })
}

fn no_budget_config() -> ContextConfig {
    ContextConfig {
        assembly: "default".to_string(),
        alternatives: Vec::new(),
        timeout: 5,
        budget: None,
        compaction_threshold: None,
        diagnosis_reader_model: None,
        diagnosis_synthesis_model: None,
    }
}

fn budget_config(tokens: usize) -> ContextConfig {
    ContextConfig {
        assembly: "default".to_string(),
        alternatives: Vec::new(),
        timeout: 5,
        budget: Some(tokens),
        compaction_threshold: None,
        diagnosis_reader_model: None,
        diagnosis_synthesis_model: None,
    }
}

// ── Assembly order ────────────────────────────────────────────────────────

#[test]
fn system_message_from_prompts_appears_before_history() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        // Provide history first (lower priority) to test reordering.
        history_element(&[("Hello", "Hi!")]),
        prompts_element("You are helpful."),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    assert!(output.messages.len() >= 2);
    // System message must be first.
    assert_eq!(output.messages[0].role(), Role::System);
    // Conversation messages follow.
    assert_eq!(output.messages[1].role(), Role::User);
    assert_eq!(output.messages[2].role(), Role::Assistant);
}

#[test]
fn empty_prompts_produce_no_system_message() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        Box::new(MockElement {
            name: "prompts",
            priority: 100,
            messages: vec![], // no rules → no system message
            source: "prompt rules",
        }),
        history_element(&[("Hello", "Hi!")]),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    // Only history messages — no system message.
    assert!(
        output.messages.iter().all(|m| m.role() != Role::System),
        "expected no system message when prompts element is empty"
    );
    assert_eq!(output.messages.len(), 2);
}

// ── Memory is skipped ─────────────────────────────────────────────────────

#[test]
fn the_memory_element_is_carried_ahead_of_the_prompts() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        prompts_element("Instructions."),
        memory_element(),
        history_element(&[("Hello", "Hi!")]),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    // memory + system + 2 history messages, memory first by priority.
    assert_eq!(output.messages.len(), 4);
    assert!(matches!(
        output.messages[0].content().first(),
        Some(swage_core::ContentPart::Text { text }) if text.contains("memory content")
    ));
}

// ── Report population ─────────────────────────────────────────────────────

#[test]
fn report_assembly_name_is_default() {
    let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    assert_eq!(output.report.assembly_name, "default");
}

#[test]
fn report_contains_element_report_for_each_active_element() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        prompts_element("Instructions."),
        history_element(&[("Hi", "Hello")]),
        memory_element(),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    // Every supplied element is active and reported.
    assert_eq!(output.report.elements.len(), 3);
    let names: Vec<&str> = output
        .report
        .elements
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(names.contains(&"prompts"));
    assert!(names.contains(&"history"));
    assert!(names.contains(&"memory"));
}

#[test]
fn report_preserves_element_token_accounting_when_no_values_are_rendered() {
    struct CustomAccountingElement;

    impl ContextElement for CustomAccountingElement {
        fn name(&self) -> &str {
            "custom-accounting"
        }

        fn priority(&self) -> i32 {
            100
        }

        fn content(&self) -> Vec<ChatMessage> {
            vec![ChatMessage::new(Role::System, "one message")]
        }

        fn token_count(&self, _tokeniser: &dyn Tokeniser) -> usize {
            73
        }

        fn source(&self) -> String {
            "custom accounting".to_owned()
        }
    }

    let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(CustomAccountingElement)];
    let output = default_assembly_with_variables(
        &elements,
        &no_budget_config(),
        &FixedCostTokeniser { cost: 10 },
        &std::collections::BTreeMap::from([("task".to_owned(), "ignored".to_owned())]),
    );

    assert_eq!(output.report.elements[0].token_count, 73);
}

#[test]
fn report_total_tokens_matches_assembled_message_count() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        prompts_element("Instructions."),
        history_element(&[("Hi", "Hello")]),
    ];
    // cost=10, 3 messages → 30 tokens.
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    assert_eq!(output.report.total_tokens, output.messages.len() * 10);
}

#[test]
fn report_budget_and_budget_remaining_populated_when_budget_configured() {
    let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
    // 2 messages × 10 = 20 tokens. Budget of 100 → remaining = 80.
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &budget_config(100), &tokeniser);

    assert_eq!(output.report.budget, Some(100));
    assert_eq!(output.report.budget_remaining, Some(80));
}

#[test]
fn report_budget_none_when_not_configured() {
    let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    assert!(output.report.budget.is_none());
    assert!(output.report.budget_remaining.is_none());
}

// ── Budget enforcement and compaction ─────────────────────────────────────

#[test]
fn compaction_not_triggered_when_within_budget() {
    let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[("Hi", "Hello")])];
    // 2 messages × 10 = 20 tokens. Budget 100 → no compaction needed.
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &budget_config(100), &tokeniser);

    assert!(output.report.compaction_applied.is_empty());
}

#[test]
fn compaction_triggered_when_history_exceeds_budget() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        prompts_element("Instructions."),
        // 6 history messages × 10 = 60 tokens; prompts adds 10 → total 70.
        history_element(&[
            ("Turn 1 user", "Turn 1 assistant"),
            ("Turn 2 user", "Turn 2 assistant"),
            ("Turn 3 user", "Turn 3 assistant"),
        ]),
    ];
    // Budget of 30: prompts takes 10, leaving only 20 for history.
    // History has 6 messages × 10 = 60, so compaction must drop some.
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &budget_config(30), &tokeniser);

    assert!(
        !output.report.compaction_applied.is_empty(),
        "expected compaction to trigger"
    );
    let record = &output.report.compaction_applied[0];
    assert_eq!(record.actual_strategy, "truncation");
    assert!(record.messages_removed > 0);
}

#[test]
fn most_recent_history_exchange_preserved_after_compaction() {
    let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[
        ("Old turn", "Old reply"),
        ("Recent user", "Recent assistant"),
    ])];
    // Tight budget: only room for 2 messages (the recent exchange).
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &budget_config(20), &tokeniser);

    // The most recent exchange must remain.
    let has_recent_user = output.messages.iter().any(|m| {
            m.role() == Role::User
                && m.content().iter().any(|p| {
                    matches!(p, swage_core::ContentPart::Text { text } if text.contains("Recent user"))
                })
        });
    assert!(has_recent_user, "most recent user message was removed");
}

// ── fail-open assemble() ──────────────────────────────────────────────────

fn skills_element(source: &'static str, instructions: Option<&str>) -> Box<dyn ContextElement> {
    let messages = match instructions {
        Some(text) => vec![ChatMessage::new(Role::System, text)],
        None => vec![],
    };
    Box::new(MockElement {
        name: "skills",
        priority: 75,
        messages,
        source,
    })
}

#[test]
fn skills_element_appears_between_prompts_and_history() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        history_element(&[("Hello", "Hi!")]),
        skills_element(
            "skills: code-review (topic, ~35 tokens)",
            Some("Always review for correctness."),
        ),
        prompts_element("You are helpful."),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    // Expected order: System (prompts), System (skills), User, Assistant
    assert_eq!(output.messages.len(), 4);
    assert_eq!(output.messages[0].role(), Role::System); // prompts
    assert_eq!(output.messages[1].role(), Role::System); // skills
    assert_eq!(output.messages[2].role(), Role::User); // history
    assert_eq!(output.messages[3].role(), Role::Assistant);
}

#[test]
fn empty_skills_element_appears_in_report_with_zero_tokens() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        prompts_element("Instructions."),
        skills_element("skills (none active)", None),
        history_element(&[("Hi", "Hello")]),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    // Skills element produces no messages — total is prompts + history only.
    assert_eq!(
        output.messages.len(),
        3,
        "empty skills element adds no messages"
    );

    // But the skills element must still appear in the report.
    let skills_report = output.report.elements.iter().find(|e| e.name == "skills");
    assert!(
        skills_report.is_some(),
        "empty skills element should appear in report"
    );
    assert_eq!(skills_report.unwrap().token_count, 0);
}

#[test]
fn skills_element_report_preview_contains_skill_names() {
    let source = "skills: code-review (topic, ~35 tokens), persona (agent, ~22 tokens)";
    let elements: Vec<Box<dyn ContextElement>> = vec![
        history_element(&[("Hi", "Hello")]),
        skills_element(source, Some("Review code. Be a persona.")),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    let skills_report = output
        .report
        .elements
        .iter()
        .find(|e| e.name == "skills")
        .unwrap();
    let preview = skills_report.preview.as_deref().unwrap_or("");
    assert!(
        preview.contains("code-review"),
        "preview must include skill name"
    );
    assert!(
        preview.contains("persona"),
        "preview must include second skill name"
    );
}

#[test]
fn empty_skills_element_report_has_no_preview() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        history_element(&[("Hi", "Hello")]),
        skills_element("skills (none active)", None),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    let skills_report = output
        .report
        .elements
        .iter()
        .find(|e| e.name == "skills")
        .unwrap();
    // No active skills → preview should be None.
    assert!(
        skills_report.preview.is_none(),
        "empty skills should have no preview"
    );
}

#[test]
fn untrusted_element_content_has_no_persisted_preview() {
    let content = format!(
        "Keep this opening instruction. Bearer {} {}",
        "a".repeat(32),
        "x".repeat(300)
    );
    let elements: Vec<Box<dyn ContextElement>> = vec![prompts_element(&content)];
    let output = default_assembly(
        &elements,
        &no_budget_config(),
        &FixedCostTokeniser { cost: 10 },
    );

    assert!(output.report.elements[0].preview.is_none());
}

#[test]
fn memory_is_carried_beside_the_skills_element() {
    let elements: Vec<Box<dyn ContextElement>> = vec![
        prompts_element("Instructions."),
        skills_element(
            "skills: my-skill (topic, ~10 tokens)",
            Some("Skill instructions."),
        ),
        memory_element(),
        history_element(&[("Hi", "Hello")]),
    ];
    let tokeniser = FixedCostTokeniser { cost: 10 };
    let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

    // Memory (1) + prompts (1) + skills (1) + history (2) = 5.
    assert_eq!(output.messages.len(), 5, "memory is carried");

    let report_names: Vec<&str> = output
        .report
        .elements
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(
        report_names.contains(&"memory"),
        "memory appears in the report"
    );
    assert!(
        report_names.contains(&"skills"),
        "skills must appear in report"
    );
}

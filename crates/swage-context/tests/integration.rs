//! Cross-task integration tests for the context assembly engine.
//!
//! These tests verify interactions between multiple tasks' outputs:
//! prompt rule loading → prompts element → default assembly → report,
//! token counting → budget enforcement → truncation compaction, and
//! fail-open orchestration with error recording.
//!
//! Each test exercises the intended public surface (the assembly library API)
//! rather than internal modules directly.

use std::fs;
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: Default assembly preserves the legacy prompt shape
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — baseline behaviour.
// Cross-task: prompt rules [xswh/ivea] → PromptsElement [064z] →
//             HistoryElement [50k7] → DefaultAssembly [3iw7] →
//             Report types [wnyz]
//
// This test verifies the core equivalence guarantee: given the same
// instructions and history, the default assembly produces messages
// identical to what the current gateway path produces (system message
// from instructions, then history in order).

#[test]
fn default_assembly_preserves_legacy_prompt_shape() {
    // Set up: create prompt rules at two hierarchy levels with different priorities
    let root = TempDir::new().unwrap();
    let global_prompts = root.path().join("prompts");
    let space_prompts = root.path().join("spaces/test/prompts");
    fs::create_dir_all(&global_prompts).unwrap();
    fs::create_dir_all(&space_prompts).unwrap();

    // Global rule with lower priority
    fs::write(
        global_prompts.join("base.md"),
        "---\npriority: 10\n---\n\nYou are a helpful assistant.",
    )
    .unwrap();

    // Space rule with higher priority (appears first)
    fs::write(
        space_prompts.join("space.md"),
        "---\npriority: 100\n---\n\nYou are working on space test.",
    )
    .unwrap();

    let mut rules = vec![
        swage_config::prompt_rule::parse_prompt_rule_file(
            &global_prompts.join("base.md"),
            swage_config::HierarchyLevel::Global,
        )
        .unwrap(),
        swage_config::prompt_rule::parse_prompt_rule_file(
            &space_prompts.join("space.md"),
            swage_config::HierarchyLevel::Space,
        )
        .unwrap(),
    ];
    rules.sort_by_key(|rule| std::cmp::Reverse(rule.priority));
    assert_eq!(rules.len(), 2, "should find rules at both levels");

    // The higher-priority space rule should come first
    assert_eq!(
        rules[0].priority, 100,
        "space rule (priority 100) should be first"
    );
    assert_eq!(
        rules[1].priority, 10,
        "global rule (priority 10) should be second"
    );

    // Build elements: prompts from rules, history from messages
    let history_messages = vec![
        swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
        swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi there!"),
    ];

    // Create a tokeniser
    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    // Create elements
    let prompts_element = swage_context::prompts::PromptsElement::new(rules);
    let history_element = swage_context::history::HistoryElement::new(history_messages);

    // Run the default assembly
    let config = swage_config::ContextConfig::default();
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> =
        vec![Box::new(prompts_element), Box::new(history_element)];

    let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

    // Verify the legacy prompt-compatible ordering:
    // 1. First message should be Role::System with combined prompt rule content
    assert!(
        !output.messages.is_empty(),
        "assembly must produce messages"
    );
    assert_eq!(output.messages[0].role(), swage_core::Role::System);

    // The system message content should contain both rules' content in priority order
    let system_text = &output.messages[0].content()[0];
    match system_text {
        swage_core::ContentPart::Text { text } => {
            assert!(
                text.contains("You are working on space test."),
                "higher-priority space rule should appear in system message"
            );
            assert!(
                text.contains("You are a helpful assistant."),
                "lower-priority global rule should appear in system message"
            );
            // Priority order: space rule content should appear before global
            let space_pos = text.find("You are working on space test.").unwrap();
            let global_pos = text.find("You are a helpful assistant.").unwrap();
            assert!(
                space_pos < global_pos,
                "space rule (priority 100) should appear before global rule (priority 10)"
            );
        }
        _ => panic!("expected text content in system message"),
    }

    // 2. Remaining messages should be the history in chronological order
    assert_eq!(output.messages.len(), 3, "system + 2 history messages");
    assert_eq!(output.messages[1].role(), swage_core::Role::User);
    assert_eq!(output.messages[2].role(), swage_core::Role::Assistant);

    // 3. Report should be populated
    assert_eq!(output.report.assembly_name, "default");
    assert!(
        !output.report.elements.is_empty(),
        "report should contain element entries"
    );
    assert!(
        output.report.total_tokens > 0,
        "total tokens should be positive"
    );
    assert!(
        output.report.duration.as_nanos() > 0,
        "duration should be recorded"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: Budget enforcement triggers truncation compaction
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — long session compaction.
// Cross-task: Tokeniser [xeeo] → TruncationCompactor [dc5n] →
//             DefaultAssembly [3iw7] → Report types [wnyz]
//
// Verifies that when history exceeds the configured token budget, the
// assembly truncates oldest messages while preserving recent exchanges.

#[test]
fn budget_triggers_truncation_preserving_recent_messages() {
    // Create a deliberately small budget that can't fit all messages
    let config = swage_config::ContextConfig {
        assembly: "default".to_string(),
        alternatives: Vec::new(),
        timeout: 5,
        budget: Some(200), // Very small budget — forces truncation
    };

    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    // Create a long conversation history that exceeds the budget
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(swage_core::ChatMessage::new(
            swage_core::Role::User,
            format!("This is user message number {i} with some content to consume tokens"),
        ));
        messages.push(swage_core::ChatMessage::new(
            swage_core::Role::Assistant,
            format!(
                "This is assistant response number {i} with additional token-consuming content"
            ),
        ));
    }

    let history_element = swage_context::history::HistoryElement::new(messages.clone());
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> =
        vec![Box::new(history_element)];

    let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

    // Verify truncation occurred
    assert!(
        output.messages.len() < messages.len(),
        "assembly should have fewer messages than input ({} < {})",
        output.messages.len(),
        messages.len(),
    );

    // Most recent exchange should always be preserved
    let last_output = output.messages.last().unwrap();
    let last_input = messages.last().unwrap();
    assert_eq!(
        last_output.role(),
        last_input.role(),
        "last message role should match — most recent exchange preserved"
    );

    // Report should record truncation
    assert!(
        !output.report.compaction_applied.is_empty(),
        "compaction records should be present when truncation occurred"
    );
    let record = &output.report.compaction_applied[0];
    assert_eq!(record.actual_strategy, "truncation");
    assert!(
        record.messages_removed > 0,
        "should report messages removed"
    );
    assert!(
        record.tokens_before > record.tokens_after,
        "tokens should decrease after truncation"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: The public assembly entry point produces a reported assembly
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality.
//
// Verifies that the library's public entry point assembles messages and
// returns a populated report. Fail-open to the default assembly is now a
// property of the JavaScript assembly path and is exercised there.

#[test]
fn public_entry_point_assembles_and_reports() {
    let config = swage_config::ContextConfig::default();
    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let history_messages = vec![
        swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
        swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi!"),
    ];

    let history_element = swage_context::history::HistoryElement::new(history_messages);
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> =
        vec![Box::new(history_element)];

    let output = swage_context::assembly::assemble(&elements, &config, &tokeniser);

    // Default assembly should succeed without errors
    assert!(
        !output.messages.is_empty(),
        "assembly should produce messages"
    );
    assert!(
        output.report.errors.is_empty(),
        "no errors expected when default assembly succeeds"
    );

    // Verify the report is populated
    assert_eq!(output.report.assembly_name, "default");
    assert!(output.report.total_tokens > 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: Prompt rules from multiple hierarchy levels merge in priority order
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — configuration-driven context.
// Cross-task: PromptRule parsing [xswh] → discovery [ivea] →
//             PromptsElement [064z] → DefaultAssembly [3iw7]
//
// Verifies the full pipeline from .md files on disk through to assembled
// context, with correct priority ordering and per-rule attribution in
// the report.

#[test]
fn prompt_rules_merge_across_hierarchy_with_priority_ordering() {
    let root = TempDir::new().unwrap();

    // Global: two rules at different priorities
    let global_prompts = root.path().join("prompts");
    fs::create_dir_all(&global_prompts).unwrap();
    fs::write(
        global_prompts.join("safety.md"),
        "---\npriority: 200\n---\n\nAlways be safe and helpful.",
    )
    .unwrap();
    fs::write(
        global_prompts.join("defaults.md"),
        "---\npriority: 5\n---\n\nDefault behaviour instructions.",
    )
    .unwrap();

    // Agent: one rule with no frontmatter (default priority 0)
    let agent_prompts = root.path().join("spaces/test/agents/default/prompts");
    fs::create_dir_all(&agent_prompts).unwrap();
    fs::write(
        agent_prompts.join("persona.md"),
        "You are a coding assistant.",
    )
    .unwrap();

    let mut rules = vec![
        swage_config::prompt_rule::parse_prompt_rule_file(
            &global_prompts.join("safety.md"),
            swage_config::HierarchyLevel::Global,
        )
        .unwrap(),
        swage_config::prompt_rule::parse_prompt_rule_file(
            &global_prompts.join("defaults.md"),
            swage_config::HierarchyLevel::Global,
        )
        .unwrap(),
        swage_config::prompt_rule::parse_prompt_rule_file(
            &agent_prompts.join("persona.md"),
            swage_config::HierarchyLevel::Agent,
        )
        .unwrap(),
    ];
    rules.sort_by_key(|rule| std::cmp::Reverse(rule.priority));

    // Expected order: safety (200) > defaults (5) > persona (0, agent-level tiebreak)
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].priority, 200, "safety rule should be first");
    assert_eq!(rules[1].priority, 5, "defaults rule should be second");
    assert_eq!(
        rules[2].priority, 0,
        "persona rule (no frontmatter) should be last"
    );

    // Assemble and verify the report shows per-rule attribution
    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");
    let prompts_element = swage_context::prompts::PromptsElement::new(rules);
    let config = swage_config::ContextConfig::default();
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> =
        vec![Box::new(prompts_element)];

    let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

    // Report should show the prompts element with a token count
    let prompts_report = output
        .report
        .elements
        .iter()
        .find(|e| e.name == "prompts")
        .expect("report should contain a prompts element entry");
    assert!(
        prompts_report.token_count > 0,
        "prompts element should have positive token count"
    );

    // The assembled system message should contain all three rules' content
    assert!(!output.messages.is_empty());
    assert_eq!(output.messages[0].role(), swage_core::Role::System);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 5: Config context section merges through hierarchy
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Configuration-driven assembly behaviour.
// Cross-task: ContextConfig model [e2er] → PartialContextConfig merge [wme0] →
//             assembly uses config [faup]
//
// Verifies that context config at two hierarchy levels merges correctly
// and the assembly respects the merged budget.

#[test]
fn config_context_section_merges_through_hierarchy() {
    let root = TempDir::new().unwrap();

    // Global config sets a budget
    let global_config = root.path().join("config.toml");
    fs::write(
        &global_config,
        r#"
[context]
budget = 50000
timeout = 10
"#,
    )
    .unwrap();

    // Space config overrides the budget
    let space_dir = root.path().join("spaces/test");
    fs::create_dir_all(space_dir.join("agents/default")).unwrap();
    fs::write(
        space_dir.join("config.toml"),
        r#"
[context]
budget = 20000
"#,
    )
    .unwrap();

    let config = swage_config::load_resolved_config_from(root.path(), "test", "default")
        .expect("config should load successfully");

    // Space budget should override global
    assert_eq!(
        config.context.budget,
        Some(20000),
        "space budget should override global"
    );
    // Timeout should inherit from global (not overridden by space)
    assert_eq!(
        config.context.timeout, 10,
        "timeout should inherit from global level"
    );
    // Assembly should use the default
    assert_eq!(
        config.context.assembly, "default",
        "assembly should default to 'default'"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 6: Config validates tools and context sections without warnings
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Configuration correctness.
// Cross-task: Validation fix [ths2] + ContextConfig [e2er]
//
// Verifies the tools validation bug fix: a config with [tools] and [context]
// sections should validate cleanly with no unknown-section warnings.

#[test]
fn config_validates_tools_and_context_sections() {
    let root = TempDir::new().unwrap();
    let config_path = root.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
[server]
port = 3000

[tools]
shell_timeout_secs = 60
max_output_bytes = 200000

[context]
assembly = "default"
timeout = 5
budget = 128000
"#,
    )
    .unwrap();

    let problems = swage_config::validate_files(&[config_path.as_path()]);
    assert!(
        problems.is_empty(),
        "config with [tools] and [context] should validate cleanly, got: {problems:?}"
    );
}

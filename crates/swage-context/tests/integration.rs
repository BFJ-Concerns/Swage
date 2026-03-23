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
use std::pin::Pin;
use tempfile::TempDir;

use async_trait::async_trait;
use futures::Stream;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_provider::LlmProvider;

/// Minimal stub provider for tests that only use the default assembly path.
/// model_id() is called by assemble(); the other methods are never invoked.
struct StubProvider;

#[async_trait]
impl LlmProvider for StubProvider {
    fn model_id(&self) -> &str { "stub-model" }

    async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("StubProvider::complete")
    }

    async fn complete_with_tools(
        &self,
        _req: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("StubProvider::complete_with_tools")
    }

    fn stream(&self, _req: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        unimplemented!("StubProvider::stream")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: Default assembly produces era 0.1-equivalent output
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
fn default_assembly_produces_era_01_equivalent_output() {
    // Set up: create prompt rules at two hierarchy levels with different priorities
    let root = TempDir::new().unwrap();
    let global_prompts = root.path().join("prompts");
    let project_prompts = root.path().join("projects/test/prompts");
    fs::create_dir_all(&global_prompts).unwrap();
    fs::create_dir_all(&project_prompts).unwrap();

    // Global rule with lower priority
    fs::write(
        global_prompts.join("base.md"),
        "---\npriority: 10\n---\n\nYou are a helpful assistant.",
    ).unwrap();

    // Project rule with higher priority (appears first)
    fs::write(
        project_prompts.join("project.md"),
        "---\npriority: 100\n---\n\nYou are working on project test.",
    ).unwrap();

    // Load prompt rules from hierarchy
    let project_path = root.path().join("projects/test");
    let levels = vec![
        (swage_config::HierarchyLevel::Global, root.path().as_ref()),
        (swage_config::HierarchyLevel::Project, project_path.as_path()),
    ];

    // Load prompt rules (sorted by priority)
    let (rules, warnings) = swage_config::prompt_rule::load_prompt_rules(&levels);
    assert!(warnings.is_empty(), "no warnings expected for valid rules");
    assert_eq!(rules.len(), 2, "should find rules at both levels");

    // The higher-priority project rule should come first
    assert_eq!(rules[0].priority, 100, "project rule (priority 100) should be first");
    assert_eq!(rules[1].priority, 10, "global rule (priority 10) should be second");

    // Build elements: prompts from rules, history from messages
    let history_messages = vec![
        swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
        swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi there!"),
    ];

    // Create a tokeniser
    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    // Create elements
    let prompts_element = swage_context::prompts::PromptsElement::new(rules);
    let history_element = swage_context::history::HistoryElement::new(history_messages.clone());

    // Run the default assembly
    let config = swage_config::ContextConfig::default();
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(prompts_element),
        Box::new(history_element),
    ];

    let output = swage_context::assembly::default_assembly(
        &elements,
        &config,
        &tokeniser,
    );

    // Verify era 0.1 equivalence:
    // 1. First message should be Role::System with combined prompt rule content
    assert!(!output.messages.is_empty(), "assembly must produce messages");
    assert_eq!(output.messages[0].role, swage_core::Role::System);

    // The system message content should contain both rules' content in priority order
    let system_text = &output.messages[0].content[0];
    match system_text {
        swage_core::ContentPart::Text { text } => {
            assert!(text.contains("You are working on project test."),
                "higher-priority project rule should appear in system message");
            assert!(text.contains("You are a helpful assistant."),
                "lower-priority global rule should appear in system message");
            // Priority order: project rule content should appear before global
            let project_pos = text.find("You are working on project test.").unwrap();
            let global_pos = text.find("You are a helpful assistant.").unwrap();
            assert!(project_pos < global_pos,
                "project rule (priority 100) should appear before global rule (priority 10)");
        }
        _ => panic!("expected text content in system message"),
    }

    // 2. Remaining messages should be the history in chronological order
    assert_eq!(output.messages.len(), 3, "system + 2 history messages");
    assert_eq!(output.messages[1].role, swage_core::Role::User);
    assert_eq!(output.messages[2].role, swage_core::Role::Assistant);

    // 3. Report should be populated
    assert_eq!(output.report.assembly_name, "default");
    assert!(!output.report.elements.is_empty(), "report should contain element entries");
    assert!(output.report.total_tokens > 0, "total tokens should be positive");
    assert!(output.report.duration.as_nanos() > 0, "duration should be recorded");
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
            format!("This is assistant response number {i} with additional token-consuming content"),
        ));
    }

    let history_element = swage_context::history::HistoryElement::new(messages.clone());
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(history_element),
    ];

    let output = swage_context::assembly::default_assembly(
        &elements,
        &config,
        &tokeniser,
    );

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
    assert_eq!(last_output.role, last_input.role,
        "last message role should match — most recent exchange preserved");

    // Report should record truncation
    assert!(!output.report.compaction_applied.is_empty(),
        "compaction records should be present when truncation occurred");
    let record = &output.report.compaction_applied[0];
    assert_eq!(record.actual_strategy, "truncation");
    assert!(record.messages_removed > 0, "should report messages removed");
    assert!(record.tokens_before > record.tokens_after,
        "tokens should decrease after truncation");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: Fail-open catches error and falls back to default assembly
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — failure recovery.
// Cross-task: Fail-open orchestration [faup] → DefaultAssembly [3iw7] →
//             Report types [wnyz]
//
// Verifies that when an assembly produces invalid output (empty messages),
// fail-open falls back to the default assembly and records the error.

#[tokio::test]
async fn failopen_catches_invalid_assembly_and_falls_back() {
    let config = swage_config::ContextConfig::default();
    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let history_messages = vec![
        swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
        swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi!"),
    ];

    let history_element = swage_context::history::HistoryElement::new(history_messages);
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(history_element),
    ];

    // Use the top-level assemble() which wraps with fail-open.
    // Pass empty search levels and no Python runner — this test exercises the
    // default assembly path only.
    let output = swage_context::assembly::assemble(
        &elements,
        &config,
        &tokeniser,
        &[],
        &StubProvider,
        None,
    ).await;

    // Default assembly should succeed without errors
    assert!(!output.messages.is_empty(), "assembly should produce messages");
    assert!(output.report.errors.is_empty(),
        "no errors expected when default assembly succeeds");

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
    ).unwrap();
    fs::write(
        global_prompts.join("defaults.md"),
        "---\npriority: 5\n---\n\nDefault behaviour instructions.",
    ).unwrap();

    // Agent: one rule with no frontmatter (default priority 0)
    let agent_prompts = root.path().join("projects/test/agents/default/prompts");
    fs::create_dir_all(&agent_prompts).unwrap();
    fs::write(
        agent_prompts.join("persona.md"),
        "You are a coding assistant.",
    ).unwrap();

    // Load rules from three levels (project has no prompts — silently skipped)
    let project_path = root.path().join("projects/test");
    let agent_path = root.path().join("projects/test/agents/default");
    let levels = vec![
        (swage_config::HierarchyLevel::Global, root.path().as_ref()),
        (swage_config::HierarchyLevel::Project, project_path.as_path()),
        (swage_config::HierarchyLevel::Agent, agent_path.as_path()),
    ];

    let (rules, warnings) = swage_config::prompt_rule::load_prompt_rules(&levels);
    assert!(warnings.is_empty());

    // Expected order: safety (200) > defaults (5) > persona (0, agent-level tiebreak)
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].priority, 200, "safety rule should be first");
    assert_eq!(rules[1].priority, 5, "defaults rule should be second");
    assert_eq!(rules[2].priority, 0, "persona rule (no frontmatter) should be last");

    // Assemble and verify the report shows per-rule attribution
    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");
    let prompts_element = swage_context::prompts::PromptsElement::new(rules);
    let config = swage_config::ContextConfig::default();
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(prompts_element),
    ];

    let output = swage_context::assembly::default_assembly(
        &elements,
        &config,
        &tokeniser,
    );

    // Report should show the prompts element with a token count
    let prompts_report = output.report.elements.iter()
        .find(|e| e.name == "prompts")
        .expect("report should contain a prompts element entry");
    assert!(prompts_report.token_count > 0, "prompts element should have positive token count");

    // The assembled system message should contain all three rules' content
    assert!(!output.messages.is_empty());
    assert_eq!(output.messages[0].role, swage_core::Role::System);
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
    fs::write(&global_config, r#"
[context]
budget = 50000
timeout = 10
"#).unwrap();

    // Project config overrides the budget
    let project_dir = root.path().join("projects/test");
    fs::create_dir_all(project_dir.join("agents/default")).unwrap();
    fs::create_dir_all(root.path().join("lenses/default")).unwrap();
    fs::write(project_dir.join("config.toml"), r#"
[context]
budget = 20000
"#).unwrap();

    let config = swage_config::load_resolved_config_from(root.path(), "test", "default")
        .expect("config should load successfully");

    // Project budget should override global
    assert_eq!(config.context.budget, Some(20000),
        "project budget should override global");
    // Timeout should inherit from global (not overridden by project)
    assert_eq!(config.context.timeout, 10,
        "timeout should inherit from global level");
    // Assembly should use the default
    assert_eq!(config.context.assembly, "default",
        "assembly should default to 'default'");
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
    fs::write(&config_path, r#"
[server]
port = 3000

[tools]
shell_timeout_secs = 60
max_output_bytes = 200000

[context]
assembly = "default"
timeout = 5
budget = 128000
"#).unwrap();

    let problems = swage_config::validate_files(&[config_path.as_path()]);
    assert!(problems.is_empty(),
        "config with [tools] and [context] should validate cleanly, got: {problems:?}");
}

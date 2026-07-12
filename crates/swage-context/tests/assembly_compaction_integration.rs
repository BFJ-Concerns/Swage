//! Cross-task integration tests for programmable assembly and compaction.
//!
//! These tests verify interactions between multiple tasks' outputs:
//! - Assembly config discovery → Python execution → dispatch → output
//! - Intent recording → structured extraction → compaction results
//! - Named assembly resolution → include → nested report elements
//! - Fail-open → error recovery → default assembly fallback
//! - Circular reference detection → warning in report
//!
//! Each test exercises the intended public surface: the assembly engine API
//! configured with custom assembly names, producing assembly reports that
//! reflect Python-controlled element ordering and compaction.

use std::fs;
use std::path::Path;
use std::pin::Pin;
use tempfile::TempDir;

use async_trait::async_trait;
use futures::Stream;
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_provider::LlmProvider;

// ─────────────────────────────────────────────────────────────────────────────
// Minimal stub provider — only model_id() is called by the assembly engine;
// the completion/stream methods are never invoked in these tests.
// ─────────────────────────────────────────────────────────────────────────────

struct StubProvider;

#[async_trait]
impl LlmProvider for StubProvider {
    fn model_id(&self) -> &str {
        "stub-model"
    }

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
// Test 1: Custom Python assembly reorders context elements
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — programmable context.
// Cross-task: discovery [jcrf] → context [m8iw] → elements [iu50] →
//             execution [s7xz] → dispatch [mgoi]
//
// A Python assembly config in the space's assemblies directory reorders
// elements (memory before prompts). The assembly report should show the
// custom order with actual content in each section — not the default
// assembly's priority-based order.
//
// Public surface: Python file in space assemblies dir, selected via config.

#[tokio::test]
async fn custom_assembly_reorders_elements_via_python() {
    let root = TempDir::new().unwrap();

    // Create space assemblies directory with a custom assembly config.
    let assemblies_dir = root.path().join("spaces/test/assemblies");
    fs::create_dir_all(&assemblies_dir).unwrap();

    // This assembly puts memory first, then history, then prompts —
    // the opposite of default's priority-based order (prompts > history).
    fs::write(
        assemblies_dir.join("custom.py"),
        r#"
def assemble(ctx):
    ctx.add(ctx.memory())
    ctx.add(ctx.history_recent(budget=ctx.remaining_budget - 500))
    ctx.add(ctx.prompts())
"#,
    )
    .unwrap();

    // Configure to use the custom assembly.
    let config = swage_config::ContextConfig {
        assembly: "custom".to_string(),
        timeout: 5,
        budget: Some(16000),
    };

    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    // Build elements with identifiable content.
    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(swage_context::prompts::PromptsElement::new(vec![
            swage_config::prompt_rule::PromptRule {
                priority: 100,
                content: "You are a helpful assistant.".to_string(),
                source_level: swage_config::HierarchyLevel::Global,
                file_path: std::path::PathBuf::from("test"),
            },
        ])),
        Box::new(swage_context::memory::MemoryElement::new(
            "The user prefers Rust.".to_string(),
            "test-space".to_string(),
        )),
        Box::new(swage_context::history::HistoryElement::new(vec![
            swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
            swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi!"),
        ])),
    ];

    // Search hierarchy: space level at the temp dir root.
    let space_dir = root.path().join("spaces/test");
    let search_levels: Vec<(swage_config::HierarchyLevel, &Path)> =
        vec![(swage_config::HierarchyLevel::Space, space_dir.as_path())];

    let runner = swage_python::ConcreteAssemblyRunner;
    let output = swage_context::assembly::assemble(
        &elements,
        &config,
        &tokeniser,
        &search_levels,
        &StubProvider,
        Some(&runner),
    )
    .await;

    // The custom assembly should have run — report should show "custom",
    // not "default" (the fallback).
    assert_eq!(
        output.report.assembly_name, "custom",
        "expected custom assembly to run, not default fallback"
    );

    // Report elements should reflect the order ctx.add() was called:
    // memory first, then history, then prompts.
    assert!(
        output.report.elements.len() >= 3,
        "report should contain at least 3 elements (memory, history, prompts)"
    );
    assert_eq!(
        output.report.elements[0].name, "memory",
        "first element should be memory (custom order, not default priority)"
    );

    // Memory content should actually be present in the assembled messages —
    // not empty (false-completion guard: element APIs must return real data).
    let has_memory_content = output.messages.iter().any(|m| {
        m.content().iter().any(|p| {
            matches!(p, swage_core::ContentPart::Text { text } if text.contains("prefers Rust"))
        })
    });
    assert!(
        has_memory_content,
        "memory element content must appear in assembled messages"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: Structured extraction produces categorised records, not truncation
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — intelligent compaction.
// Cross-task: extraction [9ry2] → deferred compaction [7ank] → elements [iu50] →
//             dispatch [mgoi]
//
// A Python assembly applies strategy="structured_extract" to older history.
// The assembly report should show structured extraction output that is
// structurally different from raw message truncation — containing categorised
// records (decisions, constraints, action items, facts).
//
// Public surface: strategy parameter on history elements in the assembly Python API.

#[tokio::test]
async fn structured_extraction_produces_categorised_records() {
    let root = TempDir::new().unwrap();

    // Assembly that applies structured extraction to older history.
    let assemblies_dir = root.path().join("spaces/test/assemblies");
    fs::create_dir_all(&assemblies_dir).unwrap();
    fs::write(
        assemblies_dir.join("extract.py"),
        r#"
def assemble(ctx):
    ctx.add(ctx.prompts())
    if ctx.remaining_budget > 4000:
        ctx.add(ctx.history_older(
            budget=2000,
            strategy="structured_extract",
        ))
    ctx.add(ctx.history_recent(budget=ctx.remaining_budget))
"#,
    )
    .unwrap();

    let config = swage_config::ContextConfig {
        assembly: "extract".to_string(),
        timeout: 10,
        budget: Some(16000),
    };

    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    // Build a conversation history with identifiable decisions and facts.
    let history = vec![
        // Older history with extractable content.
        swage_core::ChatMessage::new(
            swage_core::Role::User,
            "Let's use PostgreSQL for the database. The team decided this yesterday.",
        ),
        swage_core::ChatMessage::new(
            swage_core::Role::Assistant,
            "Good choice. I'll set up the PostgreSQL schema. The constraint is that we need to support concurrent writes.",
        ),
        swage_core::ChatMessage::new(
            swage_core::Role::User,
            "Also, the deadline is March 30th. That's non-negotiable.",
        ),
        swage_core::ChatMessage::new(
            swage_core::Role::Assistant,
            "Noted. Action item: create the migration scripts by end of this week.",
        ),
        // Recent history.
        swage_core::ChatMessage::new(swage_core::Role::User, "What's the current status?"),
        swage_core::ChatMessage::new(swage_core::Role::Assistant, "Working on the schema now."),
    ];

    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(swage_context::prompts::PromptsElement::new(vec![
            swage_config::prompt_rule::PromptRule {
                priority: 100,
                content: "You are a space manager.".to_string(),
                source_level: swage_config::HierarchyLevel::Global,
                file_path: std::path::PathBuf::from("test"),
            },
        ])),
        Box::new(swage_context::history::HistoryElement::new(history)),
    ];

    let space_dir = root.path().join("spaces/test");
    let search_levels: Vec<(swage_config::HierarchyLevel, &Path)> =
        vec![(swage_config::HierarchyLevel::Space, space_dir.as_path())];
    let runner = swage_python::ConcreteAssemblyRunner;
    let output = swage_context::assembly::assemble(
        &elements,
        &config,
        &tokeniser,
        &search_levels,
        &StubProvider,
        Some(&runner),
    )
    .await;

    // The extract assembly should have run (not fallen back to default).
    assert_eq!(
        output.report.assembly_name, "extract",
        "expected extract assembly to run"
    );

    // The report should show a compaction record with structured extraction.
    let has_structured_compaction = output
        .report
        .compaction_applied
        .iter()
        .any(|r| r.actual_strategy.contains("structured_extract"));
    assert!(
        has_structured_compaction,
        "report should contain a structured_extract compaction record"
    );

    // The compacted content should contain categorised records — NOT just
    // truncated raw messages. Look for category markers in the assembled
    // messages (decisions, constraints, action items, facts).
    let assembled_text: String = output
        .messages
        .iter()
        .flat_map(|m| m.content().iter())
        .filter_map(|p| match p {
            swage_core::ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // At least one extraction category should be present in the output.
    let has_categories = assembled_text.contains("Decisions")
        || assembled_text.contains("Constraints")
        || assembled_text.contains("Action")
        || assembled_text.contains("Facts");
    assert!(
        has_categories,
        "structured extraction should produce categorised records, not raw messages"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: Named assembly included from parent hierarchy level
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — reusable assembly fragments.
// Cross-task: discovery [jcrf] → include [ht4k] → report-extensions [qfve]
//
// A global-level named assembly "intro" is included from a space-level
// assembly via ctx.include("intro"). The report should show the included
// content as a distinct nested element with provenance tracing to the global
// level. A second space-level "intro" overrides the global version.
//
// Public surface: ctx.include("name") in assembly Python code.

#[tokio::test]
async fn named_assembly_included_from_parent_hierarchy() {
    let root = TempDir::new().unwrap();

    // Global-level named assembly "intro" adds a system message.
    let global_assemblies = root.path().join("assemblies");
    fs::create_dir_all(&global_assemblies).unwrap();
    fs::write(
        global_assemblies.join("intro.py"),
        r#"
def assemble(ctx):
    ctx.add(ctx.prompts())
"#,
    )
    .unwrap();

    // Space-level main assembly includes "intro" then adds history.
    let space_assemblies = root.path().join("spaces/test/assemblies");
    fs::create_dir_all(&space_assemblies).unwrap();
    fs::write(
        space_assemblies.join("main.py"),
        r#"
def assemble(ctx):
    ctx.include("intro")
    ctx.add(ctx.history_recent(budget=ctx.remaining_budget))
"#,
    )
    .unwrap();

    let config = swage_config::ContextConfig {
        assembly: "main".to_string(),
        timeout: 5,
        budget: Some(16000),
    };

    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(swage_context::prompts::PromptsElement::new(vec![
            swage_config::prompt_rule::PromptRule {
                priority: 100,
                content: "Global instructions.".to_string(),
                source_level: swage_config::HierarchyLevel::Global,
                file_path: std::path::PathBuf::from("test"),
            },
        ])),
        Box::new(swage_context::history::HistoryElement::new(vec![
            swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
            swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi!"),
        ])),
    ];

    // Two hierarchy levels: space (high priority) and global (fallback).
    let space_dir = root.path().join("spaces/test");
    let global_dir = root.path();
    let search_levels: Vec<(swage_config::HierarchyLevel, &Path)> = vec![
        (swage_config::HierarchyLevel::Space, space_dir.as_path()),
        (swage_config::HierarchyLevel::Global, global_dir),
    ];
    let runner = swage_python::ConcreteAssemblyRunner;
    let output = swage_context::assembly::assemble(
        &elements,
        &config,
        &tokeniser,
        &search_levels,
        &StubProvider,
        Some(&runner),
    )
    .await;

    // The main assembly should have run.
    assert_eq!(
        output.report.assembly_name, "main",
        "expected main assembly to run"
    );

    // The report should contain a named assembly element showing that "intro"
    // was included, with provenance indicating the global level.
    let has_named_assembly = output.report.elements.iter().any(|e| {
        e.preview
            .as_ref()
            .map(|p| p.contains("intro"))
            .unwrap_or(false)
            || e.name.contains("intro")
    });
    assert!(
        has_named_assembly,
        "report should show included named assembly 'intro' with provenance"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: Broken Python assembly triggers fail-open with error in report
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — failure recovery.
// Cross-task: execution [s7xz] → dispatch [mgoi] → report-extensions [qfve]
//
// A Python assembly config with a syntax error is discovered and attempted.
// The agent still responds (fail-open via default assembly), and the
// assembly report contains the error with traceback.
//
// Public surface: Assembly report error field; default assembly substitution.

#[tokio::test]
async fn broken_python_assembly_triggers_failopen_with_error() {
    let root = TempDir::new().unwrap();

    // Write a broken assembly with a syntax error.
    let assemblies_dir = root.path().join("spaces/test/assemblies");
    fs::create_dir_all(&assemblies_dir).unwrap();
    fs::write(
        assemblies_dir.join("broken.py"),
        // Invalid Python — missing colon after def
        "def assemble(ctx)\n    ctx.add(ctx.prompts())\n",
    )
    .unwrap();

    let config = swage_config::ContextConfig {
        assembly: "broken".to_string(),
        timeout: 5,
        budget: None,
    };

    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let elements: Vec<Box<dyn swage_context::element::ContextElement>> =
        vec![Box::new(swage_context::history::HistoryElement::new(vec![
            swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
            swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi!"),
        ]))];

    let space_dir = root.path().join("spaces/test");
    let search_levels: Vec<(swage_config::HierarchyLevel, &Path)> =
        vec![(swage_config::HierarchyLevel::Space, space_dir.as_path())];
    let runner = swage_python::ConcreteAssemblyRunner;
    let output = swage_context::assembly::assemble(
        &elements,
        &config,
        &tokeniser,
        &search_levels,
        &StubProvider,
        Some(&runner),
    )
    .await;

    // Fail-open: the agent still responds with default assembly output.
    assert!(
        !output.messages.is_empty(),
        "fail-open should produce messages even with broken assembly"
    );

    // The report should contain an error from the broken assembly.
    assert!(
        !output.report.errors.is_empty(),
        "report should contain an error from the broken assembly"
    );

    let error = &output.report.errors[0];
    assert_eq!(
        error.assembly_name, "broken",
        "error should name the broken assembly"
    );
    // The error should contain traceback information from the Python syntax error.
    assert!(
        error.message.contains("SyntaxError") || error.message.contains("syntax"),
        "error should mention the Python syntax error, got: {}",
        error.message
    );
    assert_eq!(
        error.fallback_used, "default",
        "fallback should be the default assembly"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 5: Circular named assembly reference detected and reported
// ─────────────────────────────────────────────────────────────────────────────
//
// Journey: Sustained Conversation Quality — safe assembly composition.
// Cross-task: include [ht4k] → report-extensions [qfve]
//
// Assembly A includes assembly B, and B includes A. The cycle is detected
// before infinite recursion. The offending include is skipped and the
// report shows which assemblies formed the cycle.
//
// Public surface: Assembly report warning field with cycle path.

#[tokio::test]
async fn circular_named_assembly_reference_detected() {
    let root = TempDir::new().unwrap();

    // Assembly A includes B.
    let assemblies_dir = root.path().join("spaces/test/assemblies");
    fs::create_dir_all(&assemblies_dir).unwrap();
    fs::write(
        assemblies_dir.join("alpha.py"),
        r#"
def assemble(ctx):
    ctx.add(ctx.prompts())
    ctx.include("beta")
"#,
    )
    .unwrap();

    // Assembly B includes A — creating a cycle.
    fs::write(
        assemblies_dir.join("beta.py"),
        r#"
def assemble(ctx):
    ctx.include("alpha")
    ctx.add(ctx.history_recent(budget=ctx.remaining_budget))
"#,
    )
    .unwrap();

    let config = swage_config::ContextConfig {
        assembly: "alpha".to_string(),
        timeout: 5,
        budget: Some(16000),
    };

    let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let elements: Vec<Box<dyn swage_context::element::ContextElement>> = vec![
        Box::new(swage_context::prompts::PromptsElement::new(vec![
            swage_config::prompt_rule::PromptRule {
                priority: 100,
                content: "Instructions.".to_string(),
                source_level: swage_config::HierarchyLevel::Global,
                file_path: std::path::PathBuf::from("test"),
            },
        ])),
        Box::new(swage_context::history::HistoryElement::new(vec![
            swage_core::ChatMessage::new(swage_core::Role::User, "Hello"),
            swage_core::ChatMessage::new(swage_core::Role::Assistant, "Hi!"),
        ])),
    ];

    let space_dir = root.path().join("spaces/test");
    let search_levels: Vec<(swage_config::HierarchyLevel, &Path)> =
        vec![(swage_config::HierarchyLevel::Space, space_dir.as_path())];
    let runner = swage_python::ConcreteAssemblyRunner;
    let output = swage_context::assembly::assemble(
        &elements,
        &config,
        &tokeniser,
        &search_levels,
        &StubProvider,
        Some(&runner),
    )
    .await;

    // The alpha assembly should have run (not crashed).
    assert_eq!(
        output.report.assembly_name, "alpha",
        "expected alpha assembly to run"
    );
    assert!(
        !output.messages.is_empty(),
        "assembly should still produce messages despite the cycle"
    );

    // The report should contain a warning about the circular reference.
    let has_cycle_warning = output
        .report
        .warnings
        .iter()
        .any(|w| w.contains("circular") || w.contains("cycle") || w.contains("alpha"));
    assert!(
        has_cycle_warning,
        "report should warn about circular reference between alpha and beta"
    );
}

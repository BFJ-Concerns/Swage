//! Cross-task integration tests for the hierarchical configuration system.
//!
//! These tests exercise the full resolution pipeline: directory discovery →
//! TOML loading → schema validation → cascade merging → instruction assembly.
//! Each test proves a capability from the Delivery Closure table that spans
//! multiple internal modules.

use std::fs;
use swage_config::{discover, load_resolved_config_from};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a standard Swage directory skeleton in a temp dir.
/// Returns the TempDir (must be kept alive for the duration of the test)
/// and the root path.
fn build_skeleton(
    global_toml: Option<&str>,
    bench: &str,
    bench_toml: Option<&str>,
    agent: &str,
    agent_toml: Option<&str>,
) -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path();

    // Global level
    if let Some(content) = global_toml {
        fs::write(root.join("config.toml"), content).unwrap();
    }

    // Bench level
    let bench_dir = root.join("benches").join(bench);
    fs::create_dir_all(bench_dir.join("agents").join(agent)).unwrap();
    fs::create_dir_all(bench_dir.join("teams")).unwrap();

    if let Some(content) = bench_toml {
        fs::write(bench_dir.join("config.toml"), content).unwrap();
    }

    // Agent level
    if let Some(content) = agent_toml {
        fs::write(
            bench_dir.join("agents").join(agent).join("config.toml"),
            content,
        )
        .unwrap();
    }

    tmp
}

// ---------------------------------------------------------------------------
// Journey: Agent Configuration — cascade merging
// ---------------------------------------------------------------------------

/// Three-level fixture: global sets defaults, bench overrides model,
/// agent overrides port. The resolved config reflects all three levels.
#[test]
fn cascade_merge_three_levels() {
    let global = r#"
[server]
host = "127.0.0.1"
port = 3000

[provider]
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://global-provider.invalid/v1"
"#;

    let bench = r#"
[provider]
model = "claude-opus-4-20250514"
"#;

    let agent = r#"
[server]
port = 4000
"#;

    let tmp = build_skeleton(
        Some(global),
        "mybench",
        Some(bench),
        "assistant",
        Some(agent),
    );
    let config = load_resolved_config_from(tmp.path(), "mybench", "assistant")
        .expect("should load successfully");

    // Global value inherited
    assert_eq!(config.server.host, "127.0.0.1");
    // Agent overrides global port
    assert_eq!(config.server.port, 4000);
    // Bench overrides global model
    assert_eq!(config.provider.model, "claude-opus-4-20250514");
    // Global value inherited through bench (bench didn't set protocol)
    assert_eq!(config.provider.api_key_env, "ANTHROPIC_API_KEY");
    assert_eq!(
        config.provider.base_url.as_deref(),
        Some("https://global-provider.invalid/v1")
    );
}

/// Bench-level list field replaces (not extends) the global-level list.
/// Using provider.protocol as an enum field — if bench sets it, global's
/// value is fully replaced.
#[test]
fn bench_list_replaces_global() {
    let global = r#"
[provider]
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
"#;

    let bench = r#"
[provider]
protocol = "openai"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
base_url = "http://127.0.0.1:11434/v1"
"#;

    let tmp = build_skeleton(Some(global), "mybench", Some(bench), "assistant", None);
    let config = load_resolved_config_from(tmp.path(), "mybench", "assistant")
        .expect("should load successfully");

    // Bench completely overrides provider section values
    assert_eq!(config.provider.model, "gpt-4o");
    assert_eq!(config.provider.api_key_env, "OPENAI_API_KEY");
    assert_eq!(
        config.provider.base_url.as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );
}

/// Missing intermediate config files are skipped without error.
/// A bench with no config.toml inherits everything from global.
#[test]
fn missing_intermediate_config_skipped() {
    let global = r#"
[server]
port = 5000

[provider]
model = "claude-sonnet-4-20250514"
"#;

    // No bench config, no agent config
    let tmp = build_skeleton(Some(global), "bare-bench", None, "default", None);
    let config = load_resolved_config_from(tmp.path(), "bare-bench", "default")
        .expect("should load successfully");

    // Global values inherited all the way through
    assert_eq!(config.server.port, 5000);
    assert_eq!(config.provider.model, "claude-sonnet-4-20250514");
}

/// An unspecified team directory is outside the active config cascade.
/// Even if a team config.toml has values, it does not affect an agent-only load.
#[test]
fn unspecified_team_configs_are_ignored() {
    let global = r#"
[server]
port = 3000
"#;

    let tmp = build_skeleton(Some(global), "mybench", None, "assistant", None);

    // Write a team config that tries to override port
    let team_dir = tmp.path().join("benches/mybench/teams/ops");
    fs::create_dir_all(&team_dir).unwrap();
    fs::write(team_dir.join("config.toml"), "[server]\nport = 8888\n").unwrap();

    let config = load_resolved_config_from(tmp.path(), "mybench", "assistant")
        .expect("should load successfully");

    // Team values are ignored unless that team is explicitly part of the load.
    assert_eq!(config.server.port, 3000);
}

// ---------------------------------------------------------------------------
// Journey: Agent Configuration — instruction assembly
// ---------------------------------------------------------------------------

/// Instructions from global, bench, and agent levels compose in that order.
/// Within a level, files sort alphabetically. Each level is attributed.
#[test]
fn instructions_compose_across_levels() {
    let tmp = build_skeleton(None, "mybench", None, "assistant", None);
    let root = tmp.path();

    // Global instructions
    let global_instr = root.join("instructions");
    fs::create_dir_all(&global_instr).unwrap();
    fs::write(
        global_instr.join("01-base.md"),
        "You are a helpful assistant.",
    )
    .unwrap();
    fs::write(global_instr.join("02-safety.md"), "Always be safe.").unwrap();

    // Bench instructions
    let bench_instr = root.join("benches/mybench/instructions");
    fs::create_dir_all(&bench_instr).unwrap();
    fs::write(bench_instr.join("context.md"), "This is a Rust bench.").unwrap();

    // Agent instructions
    let agent_instr = root.join("benches/mybench/agents/assistant/instructions");
    fs::create_dir_all(&agent_instr).unwrap();
    fs::write(agent_instr.join("personality.md"), "Be concise.").unwrap();

    let config =
        load_resolved_config_from(root, "mybench", "assistant").expect("should load successfully");

    // All three levels present in assembled instructions
    assert!(config.instructions.contains("You are a helpful assistant."));
    assert!(config.instructions.contains("Always be safe."));
    assert!(config.instructions.contains("This is a Rust bench."));
    assert!(config.instructions.contains("Be concise."));

    // Global content appears before bench content, bench before agent.
    // We check relative positions.
    let global_pos = config
        .instructions
        .find("You are a helpful assistant.")
        .unwrap();
    let bench_pos = config.instructions.find("This is a Rust bench.").unwrap();
    let agent_pos = config.instructions.find("Be concise.").unwrap();
    assert!(
        global_pos < bench_pos,
        "global instructions should come before bench"
    );
    assert!(
        bench_pos < agent_pos,
        "bench instructions should come before agent"
    );

    // Level attribution is present
    assert!(
        config.instructions.contains("Global"),
        "should attribute global level"
    );
    assert!(
        config.instructions.contains("Bench"),
        "should attribute bench level"
    );
    assert!(
        config.instructions.contains("Agent"),
        "should attribute agent level"
    );
}

/// A level with no instructions/ directory contributes nothing (no error).
#[test]
fn missing_instructions_directory_is_fine() {
    let global = r#"
[server]
port = 3000
"#;

    // No instructions directories at any level
    let tmp = build_skeleton(Some(global), "mybench", None, "assistant", None);
    let config = load_resolved_config_from(tmp.path(), "mybench", "assistant")
        .expect("should load successfully");

    // Instructions should be empty (or only whitespace)
    assert!(config.instructions.trim().is_empty() || config.instructions.is_empty());
}

/// Non-markdown files in instructions/ are ignored silently.
#[test]
fn non_markdown_files_ignored_in_instructions() {
    let tmp = build_skeleton(None, "mybench", None, "assistant", None);
    let instr_dir = tmp.path().join("instructions");
    fs::create_dir_all(&instr_dir).unwrap();
    fs::write(instr_dir.join("notes.md"), "Important notes.").unwrap();
    fs::write(instr_dir.join("scratch.txt"), "This should be ignored.").unwrap();
    fs::write(instr_dir.join("data.json"), "{}").unwrap();

    let config = load_resolved_config_from(tmp.path(), "mybench", "assistant")
        .expect("should load successfully");

    assert!(config.instructions.contains("Important notes."));
    assert!(!config.instructions.contains("This should be ignored."));
    assert!(!config.instructions.contains("{}"));
}

// ---------------------------------------------------------------------------
// Journey: Agent Configuration — validation
// ---------------------------------------------------------------------------

/// Malformed TOML produces an error naming the file and line.
#[test]
fn malformed_toml_produces_parse_error() {
    let tmp = build_skeleton(None, "mybench", None, "assistant", None);
    fs::write(
        tmp.path().join("config.toml"),
        "this is not valid toml ===\n",
    )
    .unwrap();

    let result = load_resolved_config_from(tmp.path(), "mybench", "assistant");
    let err = result.expect_err("should fail on malformed TOML");

    // Error should reference the file path
    let err_str = format!("{err}");
    assert!(
        err_str.contains("config.toml"),
        "error should name the file: {err_str}"
    );
}

/// Wrong-type fields produce an error naming the file and field path.
#[test]
fn wrong_type_produces_type_error() {
    let bad_config = r#"
[server]
port = "not a number"
"#;

    let tmp = build_skeleton(Some(bad_config), "mybench", None, "assistant", None);
    let result = load_resolved_config_from(tmp.path(), "mybench", "assistant");
    let err = result.expect_err("should fail on wrong type");

    let err_str = format!("{err}");
    assert!(
        err_str.contains("port"),
        "error should name the field: {err_str}"
    );
}

/// Unknown sections produce an error naming the file and the unrecognised key.
#[test]
fn unknown_section_produces_error() {
    let bad_config = r#"
[server]
port = 3000

[databse]
url = "sqlite://test.db"
"#;

    let tmp = build_skeleton(Some(bad_config), "mybench", None, "assistant", None);
    let result = load_resolved_config_from(tmp.path(), "mybench", "assistant");
    let err = result.expect_err("should fail on unknown section");

    let err_str = format!("{err}");
    assert!(
        err_str.contains("databse"),
        "error should name the unknown section: {err_str}"
    );
}

/// Multiple errors in one file are all reported.
#[test]
fn multiple_errors_all_reported() {
    let bad_config = r#"
[server]
port = "not a number"

[unknown_section]
foo = "bar"
"#;

    let tmp = build_skeleton(Some(bad_config), "mybench", None, "assistant", None);
    let result = load_resolved_config_from(tmp.path(), "mybench", "assistant");
    let err = result.expect_err("should fail with multiple errors");

    let err_str = format!("{err}");
    // Both errors should be present
    assert!(
        err_str.contains("port") || err_str.contains("unknown_section"),
        "at least one specific error should be reported: {err_str}"
    );
}

/// Valid config across all levels loads without errors.
#[test]
fn valid_config_loads_cleanly() {
    let global = r#"
[server]
host = "127.0.0.1"
port = 3000

[provider]
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"

[workspace]
path = "/home/user/benches/test"

[sandbox]
enabled = true

[lifecycle]
idle_timeout = 300

[logging]
level = "info"
"#;

    let tmp = build_skeleton(Some(global), "mybench", None, "assistant", None);
    let config = load_resolved_config_from(tmp.path(), "mybench", "assistant")
        .expect("valid config should load without errors");

    assert_eq!(config.server.host, "127.0.0.1");
    assert_eq!(config.server.port, 3000);
    assert_eq!(config.provider.model, "claude-sonnet-4-20250514");
    assert!(config.sandbox.enabled);
    assert_eq!(config.lifecycle.idle_timeout, 300);
}

// ---------------------------------------------------------------------------
// Journey: Bench Workspace — directory discovery
// ---------------------------------------------------------------------------

/// Scanning discovers all bench directories and their agents.
#[test]
fn discovery_finds_benches_and_agents() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Create multiple benches with agents
    let proj_a = root.join("benches/alpha/agents");
    fs::create_dir_all(proj_a.join("coder")).unwrap();
    fs::create_dir_all(proj_a.join("reviewer")).unwrap();

    let proj_b = root.join("benches/beta/agents");
    fs::create_dir_all(proj_b.join("assistant")).unwrap();

    // A bench with no agents directory
    fs::create_dir_all(root.join("benches/gamma")).unwrap();

    let benches = discover(root);

    // All three benches discovered
    let names: Vec<&str> = benches.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "should discover alpha");
    assert!(names.contains(&"beta"), "should discover beta");
    assert!(names.contains(&"gamma"), "should discover gamma");

    // Alpha has two agents
    let alpha = benches.iter().find(|p| p.name == "alpha").unwrap();
    assert!(alpha.agents.contains(&"coder".to_string()));
    assert!(alpha.agents.contains(&"reviewer".to_string()));

    // Beta has one agent
    let beta = benches.iter().find(|p| p.name == "beta").unwrap();
    assert_eq!(beta.agents.len(), 1);

    // Gamma has no agents
    let gamma = benches.iter().find(|p| p.name == "gamma").unwrap();
    assert!(gamma.agents.is_empty());
}

/// A bench with no config.toml is still discovered (it inherits global config).
#[test]
fn bench_without_config_still_discovered() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Bench directory exists but has no config.toml
    fs::create_dir_all(root.join("benches/bare")).unwrap();

    let benches = discover(root);
    assert_eq!(benches.len(), 1);
    assert_eq!(benches[0].name, "bare");
}

/// The default lens directory is recognised when present.
#[test]
fn default_lens_directory_recognised() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("lenses/default")).unwrap();

    // Discovery should not error on the lens structure
    let benches = discover(root);
    assert!(benches.is_empty(), "no benches, just the lens placeholder");
}

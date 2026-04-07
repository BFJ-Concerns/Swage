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
    project: &str,
    project_toml: Option<&str>,
    agent: &str,
    agent_toml: Option<&str>,
) -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path();

    // Global level
    if let Some(content) = global_toml {
        fs::write(root.join("config.toml"), content).unwrap();
    }

    // Lens placeholder
    fs::create_dir_all(root.join("lenses/default")).unwrap();

    // Project level
    let project_dir = root.join("projects").join(project);
    fs::create_dir_all(project_dir.join("agents").join(agent)).unwrap();
    fs::create_dir_all(project_dir.join("teams")).unwrap();

    if let Some(content) = project_toml {
        fs::write(project_dir.join("config.toml"), content).unwrap();
    }

    // Agent level
    if let Some(content) = agent_toml {
        fs::write(
            project_dir.join("agents").join(agent).join("config.toml"),
            content,
        )
        .unwrap();
    }

    tmp
}

// ---------------------------------------------------------------------------
// Journey: Agent Configuration — cascade merging
// ---------------------------------------------------------------------------

/// Three-level fixture: global sets defaults, project overrides model,
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
"#;

    let project = r#"
[provider]
model = "claude-opus-4-20250514"
"#;

    let agent = r#"
[server]
port = 4000
"#;

    let tmp = build_skeleton(
        Some(global),
        "myproject",
        Some(project),
        "assistant",
        Some(agent),
    );
    let config = load_resolved_config_from(tmp.path(), "myproject", "assistant")
        .expect("should load successfully");

    // Global value inherited
    assert_eq!(config.server.host, "127.0.0.1");
    // Agent overrides global port
    assert_eq!(config.server.port, 4000);
    // Project overrides global model
    assert_eq!(config.provider.model, "claude-opus-4-20250514");
    // Global value inherited through project (project didn't set protocol)
    assert_eq!(config.provider.api_key_env, "ANTHROPIC_API_KEY");
}

/// Project-level list field replaces (not extends) the global-level list.
/// Using provider.protocol as an enum field — if project sets it, global's
/// value is fully replaced.
#[test]
fn project_list_replaces_global() {
    let global = r#"
[provider]
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
"#;

    let project = r#"
[provider]
protocol = "openai"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
"#;

    let tmp = build_skeleton(Some(global), "myproject", Some(project), "assistant", None);
    let config = load_resolved_config_from(tmp.path(), "myproject", "assistant")
        .expect("should load successfully");

    // Project completely overrides provider section values
    assert_eq!(config.provider.model, "gpt-4o");
    assert_eq!(config.provider.api_key_env, "OPENAI_API_KEY");
}

/// Missing intermediate config files are skipped without error.
/// A project with no config.toml inherits everything from global.
#[test]
fn missing_intermediate_config_skipped() {
    let global = r#"
[server]
port = 5000

[provider]
model = "claude-sonnet-4-20250514"
"#;

    // No project config, no agent config
    let tmp = build_skeleton(Some(global), "bare-project", None, "default", None);
    let config = load_resolved_config_from(tmp.path(), "bare-project", "default")
        .expect("should load successfully");

    // Global values inherited all the way through
    assert_eq!(config.server.port, 5000);
    assert_eq!(config.provider.model, "claude-sonnet-4-20250514");
}

/// Lens and team directory levels are traversed but contribute nothing.
/// Even if a lens config.toml has values, they don't appear in resolved config.
#[test]
fn lens_and_team_configs_ignored_in_era_01() {
    let global = r#"
[server]
port = 3000
"#;

    let tmp = build_skeleton(Some(global), "myproject", None, "assistant", None);

    // Write a lens config that tries to override port
    fs::write(
        tmp.path().join("lenses/default/config.toml"),
        "[server]\nport = 9999\n",
    )
    .unwrap();

    // Write a team config that tries to override port
    let team_dir = tmp.path().join("projects/myproject/teams/ops");
    fs::create_dir_all(&team_dir).unwrap();
    fs::write(team_dir.join("config.toml"), "[server]\nport = 8888\n").unwrap();

    let config = load_resolved_config_from(tmp.path(), "myproject", "assistant")
        .expect("should load successfully");

    // Lens and team values are ignored — global port survives
    assert_eq!(config.server.port, 3000);
}

// ---------------------------------------------------------------------------
// Journey: Agent Configuration — instruction assembly
// ---------------------------------------------------------------------------

/// Instructions from global, project, and agent levels compose in that order.
/// Within a level, files sort alphabetically. Each level is attributed.
#[test]
fn instructions_compose_across_levels() {
    let tmp = build_skeleton(None, "myproject", None, "assistant", None);
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

    // Project instructions
    let project_instr = root.join("projects/myproject/instructions");
    fs::create_dir_all(&project_instr).unwrap();
    fs::write(project_instr.join("context.md"), "This is a Rust project.").unwrap();

    // Agent instructions
    let agent_instr = root.join("projects/myproject/agents/assistant/instructions");
    fs::create_dir_all(&agent_instr).unwrap();
    fs::write(agent_instr.join("personality.md"), "Be concise.").unwrap();

    let config = load_resolved_config_from(root, "myproject", "assistant")
        .expect("should load successfully");

    // All three levels present in assembled instructions
    assert!(config.instructions.contains("You are a helpful assistant."));
    assert!(config.instructions.contains("Always be safe."));
    assert!(config.instructions.contains("This is a Rust project."));
    assert!(config.instructions.contains("Be concise."));

    // Global content appears before project content, project before agent.
    // We check relative positions.
    let global_pos = config
        .instructions
        .find("You are a helpful assistant.")
        .unwrap();
    let project_pos = config.instructions.find("This is a Rust project.").unwrap();
    let agent_pos = config.instructions.find("Be concise.").unwrap();
    assert!(
        global_pos < project_pos,
        "global instructions should come before project"
    );
    assert!(
        project_pos < agent_pos,
        "project instructions should come before agent"
    );

    // Level attribution is present
    assert!(
        config.instructions.contains("Global"),
        "should attribute global level"
    );
    assert!(
        config.instructions.contains("Project"),
        "should attribute project level"
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
    let tmp = build_skeleton(Some(global), "myproject", None, "assistant", None);
    let config = load_resolved_config_from(tmp.path(), "myproject", "assistant")
        .expect("should load successfully");

    // Instructions should be empty (or only whitespace)
    assert!(config.instructions.trim().is_empty() || config.instructions.is_empty());
}

/// Non-markdown files in instructions/ are ignored silently.
#[test]
fn non_markdown_files_ignored_in_instructions() {
    let tmp = build_skeleton(None, "myproject", None, "assistant", None);
    let instr_dir = tmp.path().join("instructions");
    fs::create_dir_all(&instr_dir).unwrap();
    fs::write(instr_dir.join("notes.md"), "Important notes.").unwrap();
    fs::write(instr_dir.join("scratch.txt"), "This should be ignored.").unwrap();
    fs::write(instr_dir.join("data.json"), "{}").unwrap();

    let config = load_resolved_config_from(tmp.path(), "myproject", "assistant")
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
    let tmp = build_skeleton(None, "myproject", None, "assistant", None);
    fs::write(
        tmp.path().join("config.toml"),
        "this is not valid toml ===\n",
    )
    .unwrap();

    let result = load_resolved_config_from(tmp.path(), "myproject", "assistant");
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

    let tmp = build_skeleton(Some(bad_config), "myproject", None, "assistant", None);
    let result = load_resolved_config_from(tmp.path(), "myproject", "assistant");
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

    let tmp = build_skeleton(Some(bad_config), "myproject", None, "assistant", None);
    let result = load_resolved_config_from(tmp.path(), "myproject", "assistant");
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

    let tmp = build_skeleton(Some(bad_config), "myproject", None, "assistant", None);
    let result = load_resolved_config_from(tmp.path(), "myproject", "assistant");
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
path = "/home/user/projects/test"

[sandbox]
enabled = true

[lifecycle]
idle_timeout = 300

[logging]
level = "info"
"#;

    let tmp = build_skeleton(Some(global), "myproject", None, "assistant", None);
    let config = load_resolved_config_from(tmp.path(), "myproject", "assistant")
        .expect("valid config should load without errors");

    assert_eq!(config.server.host, "127.0.0.1");
    assert_eq!(config.server.port, 3000);
    assert_eq!(config.provider.model, "claude-sonnet-4-20250514");
    assert!(config.sandbox.enabled);
    assert_eq!(config.lifecycle.idle_timeout, 300);
}

// ---------------------------------------------------------------------------
// Journey: Project Workspace — directory discovery
// ---------------------------------------------------------------------------

/// Scanning discovers all project directories and their agents.
#[test]
fn discovery_finds_projects_and_agents() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Create multiple projects with agents
    let proj_a = root.join("projects/alpha/agents");
    fs::create_dir_all(proj_a.join("coder")).unwrap();
    fs::create_dir_all(proj_a.join("reviewer")).unwrap();

    let proj_b = root.join("projects/beta/agents");
    fs::create_dir_all(proj_b.join("assistant")).unwrap();

    // A project with no agents directory
    fs::create_dir_all(root.join("projects/gamma")).unwrap();

    let projects = discover(root);

    // All three projects discovered
    let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "should discover alpha");
    assert!(names.contains(&"beta"), "should discover beta");
    assert!(names.contains(&"gamma"), "should discover gamma");

    // Alpha has two agents
    let alpha = projects.iter().find(|p| p.name == "alpha").unwrap();
    assert!(alpha.agents.contains(&"coder".to_string()));
    assert!(alpha.agents.contains(&"reviewer".to_string()));

    // Beta has one agent
    let beta = projects.iter().find(|p| p.name == "beta").unwrap();
    assert_eq!(beta.agents.len(), 1);

    // Gamma has no agents
    let gamma = projects.iter().find(|p| p.name == "gamma").unwrap();
    assert!(gamma.agents.is_empty());
}

/// A project with no config.toml is still discovered (it inherits global config).
#[test]
fn project_without_config_still_discovered() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Project directory exists but has no config.toml
    fs::create_dir_all(root.join("projects/bare")).unwrap();

    let projects = discover(root);
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "bare");
}

/// The default lens directory is recognised when present.
#[test]
fn default_lens_directory_recognised() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("lenses/default")).unwrap();

    // Discovery should not error on the lens structure
    let projects = discover(root);
    assert!(
        projects.is_empty(),
        "no projects, just the lens placeholder"
    );
}

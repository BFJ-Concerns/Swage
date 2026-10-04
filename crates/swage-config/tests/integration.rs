//! Cross-task integration tests for the hierarchical configuration system.
//!
//! These tests exercise the full resolution pipeline: directory discovery →
//! TOML loading → schema validation → cascade merging → instruction assembly.
//! Each test proves a capability from the Delivery Closure table that spans
//! multiple internal modules.

use std::fs;
use swage_config::{RESERVED_WORKFLOW_AGENT_ID, discover, load_resolved_config_from};
use swage_core::ScopeLocation;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a standard Swage directory skeleton in a temp dir.
/// Returns the TempDir (must be kept alive for the duration of the test)
/// and the root path.
fn build_skeleton(
    global_toml: Option<&str>,
    topic: &str,
    topic_toml: Option<&str>,
    agent: &str,
    agent_toml: Option<&str>,
) -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path();

    // Global level
    if let Some(content) = global_toml {
        fs::write(root.join("config.toml"), content).expect("failed to write global config");
    }

    // Topic level
    let topic_dir = root.join("topics").join(topic);
    fs::create_dir_all(topic_dir.join("agents").join(agent))
        .expect("failed to create agent config dir");

    if let Some(content) = topic_toml {
        fs::write(topic_dir.join("config.toml"), content).expect("failed to write topic config");
    }

    // Agent level
    if let Some(content) = agent_toml {
        fs::write(
            topic_dir.join("agents").join(agent).join("config.toml"),
            content,
        )
        .expect("failed to write agent config");
    }

    tmp
}

// ---------------------------------------------------------------------------
// Journey: Agent Configuration — cascade merging
// ---------------------------------------------------------------------------

/// Three-level fixture: global sets defaults, topic overrides model,
/// agent overrides port. The resolved config reflects all three levels.
#[test]
fn cascade_merge_three_levels() {
    let global = r#"
[server]
host = "127.0.0.1"
port = 3000

[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://global-provider.invalid/v1"

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-4-20250514"
"#;

    let topic = r#"
[models]
default_model_id = "claude-opus-4-20250514"
"#;

    let agent = r#"
[server]
port = 4000
"#;

    let tmp = build_skeleton(
        Some(global),
        "mytopic",
        Some(topic),
        "assistant",
        Some(agent),
    );
    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant")
            .expect("should load successfully");

    // Global value inherited
    assert_eq!(config.server.host, "127.0.0.1");
    // Agent overrides global port
    assert_eq!(config.server.port, 4000);
    // Topic overrides global model
    assert_eq!(config.default_model_id(), "claude-opus-4-20250514");
    // Global value inherited through topic (topic didn't set protocol)
    let provider = config
        .default_provider_endpoint()
        .expect("default provider should resolve");
    assert_eq!(provider.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://global-provider.invalid/v1")
    );
}

/// Topic-level list field replaces (not extends) the global-level list.
/// Using provider protocol as an enum field — if topic sets it, global's
/// value is fully replaced.
#[test]
fn topic_list_replaces_global() {
    let global = r#"
[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-4-20250514"
"#;

    let topic = r#"
[providers]
default = "openai"

[providers.entries.openai]
protocol = "openai"
api_key_env = "OPENAI_API_KEY"
base_url = "http://127.0.0.1:11434/v1"

[models]
default = "gpt4o"

[models.entries.gpt4o]
provider_id = "openai"
model_id = "gpt-4o"
"#;

    let tmp = build_skeleton(Some(global), "mytopic", Some(topic), "assistant", None);
    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant")
            .expect("should load successfully");

    // Topic switches the default provider and model together.
    assert_eq!(config.default_model_id(), "gpt-4o");
    let provider = config
        .default_provider_endpoint()
        .expect("default provider should resolve");
    assert_eq!(provider.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
    assert_eq!(
        provider.base_url.as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );
}

#[test]
fn provider_secret_reference_loads_without_legacy_env_field() {
    let global = r#"
[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"
api_key_secret_ref = "secret://gateway/11111111-1111-1111-1111-111111111111"

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-4-20250514"
"#;

    let tmp = build_skeleton(Some(global), "mytopic", None, "assistant", None);
    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant")
            .expect("should load successfully");

    let provider = config
        .default_provider_endpoint()
        .expect("default provider should resolve");
    assert_eq!(
        provider.api_key_secret_ref.as_deref(),
        Some("secret://gateway/11111111-1111-1111-1111-111111111111")
    );
    assert_eq!(provider.api_key_env, None);
}

/// Missing intermediate config files are skipped without error.
/// A topic with no config.toml inherits everything from global.
#[test]
fn missing_intermediate_config_skipped() {
    let global = r#"
[server]
port = 5000

[models]
default_model_id = "claude-sonnet-4-20250514"
"#;

    // No topic config, no agent config
    let tmp = build_skeleton(Some(global), "bare-topic", None, "default", None);
    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("bare-topic"), "default")
            .expect("should load successfully");

    // Global values inherited all the way through
    assert_eq!(config.server.port, 5000);
    assert_eq!(config.default_model_id(), "claude-sonnet-4-20250514");
}

// ---------------------------------------------------------------------------
// Journey: Agent Configuration — instruction assembly
// ---------------------------------------------------------------------------

/// Instructions from global, topic, and agent levels compose in that order.
/// Within a level, files sort alphabetically. Each level is attributed.
#[test]
fn instructions_compose_across_levels() {
    let tmp = build_skeleton(None, "mytopic", None, "assistant", None);
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

    // Topic instructions
    let topic_instr = root.join("topics/mytopic/instructions");
    fs::create_dir_all(&topic_instr).unwrap();
    fs::write(topic_instr.join("context.md"), "This is a Rust topic.").unwrap();

    // Agent instructions
    let agent_instr = root.join("topics/mytopic/agents/assistant/instructions");
    fs::create_dir_all(&agent_instr).unwrap();
    fs::write(agent_instr.join("personality.md"), "Be concise.").unwrap();

    let config = load_resolved_config_from(root, &ScopeLocation::topic("mytopic"), "assistant")
        .expect("should load successfully");

    // All three levels present in assembled instructions
    assert!(config.instructions.contains("You are a helpful assistant."));
    assert!(config.instructions.contains("Always be safe."));
    assert!(config.instructions.contains("This is a Rust topic."));
    assert!(config.instructions.contains("Be concise."));

    // Global content appears before topic content, topic before agent.
    // We check relative positions.
    let global_pos = config
        .instructions
        .find("You are a helpful assistant.")
        .unwrap();
    let topic_pos = config.instructions.find("This is a Rust topic.").unwrap();
    let agent_pos = config.instructions.find("Be concise.").unwrap();
    assert!(
        global_pos < topic_pos,
        "global instructions should come before topic"
    );
    assert!(
        topic_pos < agent_pos,
        "topic instructions should come before agent"
    );

    // Level attribution is present
    assert!(
        config.instructions.contains("Global"),
        "should attribute global level"
    );
    assert!(
        config.instructions.contains("Topic"),
        "should attribute topic level"
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
    let tmp = build_skeleton(Some(global), "mytopic", None, "assistant", None);
    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant")
            .expect("should load successfully");

    // Instructions should be empty (or only whitespace)
    assert!(config.instructions.trim().is_empty() || config.instructions.is_empty());
}

/// Non-markdown files in instructions/ are ignored silently.
#[test]
fn non_markdown_files_ignored_in_instructions() {
    let tmp = build_skeleton(None, "mytopic", None, "assistant", None);
    let instr_dir = tmp.path().join("instructions");
    fs::create_dir_all(&instr_dir).unwrap();
    fs::write(instr_dir.join("notes.md"), "Important notes.").unwrap();
    fs::write(instr_dir.join("scratch.txt"), "This should be ignored.").unwrap();
    fs::write(instr_dir.join("data.json"), "{}").unwrap();

    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant")
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
    let tmp = build_skeleton(None, "mytopic", None, "assistant", None);
    fs::write(
        tmp.path().join("config.toml"),
        "this is not valid toml ===\n",
    )
    .unwrap();

    let result =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant");
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

    let tmp = build_skeleton(Some(bad_config), "mytopic", None, "assistant", None);
    let result =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant");
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

    let tmp = build_skeleton(Some(bad_config), "mytopic", None, "assistant", None);
    let result =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant");
    let err = result.expect_err("should fail on unknown section");

    let err_str = format!("{err}");
    assert!(
        err_str.contains("databse"),
        "error should name the unknown section: {err_str}"
    );
}

/// Generation presets are a first-class config section; settings saves may
/// write them, so startup validation must accept them on the next launch.
#[test]
fn generation_presets_load_cleanly() {
    let config = r#"
[providers]
default = "openai"

[providers.entries.openai]
protocol = "openai"
base_url = "https://api.openai.com/v1"

[models]
default = "openai:gpt-4.1-mini"

[models.entries."openai:gpt-4.1-mini"]
provider_id = "openai"
model_id = "gpt-4.1-mini"
generation_preset = "short"

[generation.presets.short]
max_tokens = 1024
temperature = 0.2
"#;

    let tmp = build_skeleton(Some(config), "mytopic", None, "assistant", None);
    let resolved =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant")
            .expect("generation presets should be accepted by startup validation");

    let preset = resolved
        .generation
        .presets
        .get("short")
        .expect("preset should resolve");
    assert_eq!(preset.max_tokens, Some(1024));
    assert_eq!(preset.temperature, Some(0.2));
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

    let tmp = build_skeleton(Some(bad_config), "mytopic", None, "assistant", None);
    let result =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant");
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

[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-4-20250514"

[sandbox]
enabled = true
allow_degraded_execution = true

[logging]
level = "info"
"#;

    let tmp = build_skeleton(Some(global), "mytopic", None, "assistant", None);
    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mytopic"), "assistant")
            .expect("valid config should load without errors");

    assert_eq!(config.server.host, "127.0.0.1");
    assert_eq!(config.server.port, 3000);
    assert_eq!(config.default_model_id(), "claude-sonnet-4-20250514");
    assert!(config.sandbox.enabled);
    assert!(config.sandbox.allow_degraded_execution);
}

#[test]
fn sandbox_defaults_off_and_topic_can_enable_it() {
    let tmp = build_skeleton(None, "mytopic", None, "assistant", None);
    let scope = ScopeLocation::topic("mytopic");
    let config = load_resolved_config_from(tmp.path(), &scope, "assistant")
        .expect("configuration without a sandbox table should load");
    assert!(!config.sandbox.enabled);
    assert!(!config.sandbox.allow_degraded_execution);

    fs::write(
        tmp.path().join("topics/mytopic/config.toml"),
        "[sandbox]\nenabled = true\n",
    )
    .expect("write topic sandbox opt-in");
    let config = load_resolved_config_from(tmp.path(), &scope, "assistant")
        .expect("topic sandbox opt-in should load");
    assert!(config.sandbox.enabled);
    assert!(!config.sandbox.allow_degraded_execution);
}

#[test]
fn sandbox_degradation_opt_in_cascades_by_field() {
    let global = r#"
[sandbox]
enabled = true
allow_degraded_execution = true
"#;
    let agent = r#"
[sandbox]
enabled = false
"#;

    let tmp = build_skeleton(Some(global), "mybench", None, "assistant", Some(agent));
    let config =
        load_resolved_config_from(tmp.path(), &ScopeLocation::topic("mybench"), "assistant")
            .expect("sandbox settings should resolve across the config cascade");

    assert!(!config.sandbox.enabled);
    assert!(config.sandbox.allow_degraded_execution);
}

// ---------------------------------------------------------------------------
// Journey: Topic Workspace — directory discovery
// ---------------------------------------------------------------------------

/// Scanning discovers all topic directories and their agents.
#[test]
fn discovery_finds_spaces_and_agents() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Create multiple topics with agents
    let proj_a = root.join("topics/alpha/agents");
    fs::create_dir_all(proj_a.join("coder")).unwrap();
    fs::create_dir_all(proj_a.join("reviewer")).unwrap();
    fs::create_dir_all(proj_a.join(RESERVED_WORKFLOW_AGENT_ID)).unwrap();

    let proj_b = root.join("topics/beta/agents");
    fs::create_dir_all(proj_b.join("assistant")).unwrap();

    // A topic with no agents directory
    fs::create_dir_all(root.join("topics/gamma")).unwrap();

    let topics = discover(root);

    // All three topics discovered
    let names: Vec<&str> = topics.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "should discover alpha");
    assert!(names.contains(&"beta"), "should discover beta");
    assert!(names.contains(&"gamma"), "should discover gamma");

    // Alpha has two agents
    let alpha = topics.iter().find(|p| p.name == "alpha").unwrap();
    assert!(alpha.agents.contains(&"coder".to_string()));
    assert!(alpha.agents.contains(&"reviewer".to_string()));
    assert!(
        !alpha
            .agents
            .contains(&RESERVED_WORKFLOW_AGENT_ID.to_string())
    );

    // Beta has one agent
    let beta = topics.iter().find(|p| p.name == "beta").unwrap();
    assert_eq!(beta.agents.len(), 1);

    // Gamma has no agents
    let gamma = topics.iter().find(|p| p.name == "gamma").unwrap();
    assert!(gamma.agents.is_empty());
}

/// The bare workflow identity inherits global and topic configuration only.
#[test]
fn reserved_workflow_agent_directory_does_not_feed_configuration() {
    let tmp = build_skeleton(
        Some("[server]\nport = 3000\n"),
        "mytopic",
        None,
        RESERVED_WORKFLOW_AGENT_ID,
        Some("[server]\nport = 4321\n"),
    );
    let reserved_dir = tmp
        .path()
        .join("topics/mytopic/agents")
        .join(RESERVED_WORKFLOW_AGENT_ID);
    fs::create_dir_all(reserved_dir.join("instructions")).unwrap();
    fs::write(
        reserved_dir.join("instructions/01-authored.md"),
        "This authored agent instruction must not be loaded.",
    )
    .unwrap();

    let config = load_resolved_config_from(
        tmp.path(),
        &ScopeLocation::topic("mytopic"),
        RESERVED_WORKFLOW_AGENT_ID,
    )
    .expect("the bare workflow identity should inherit ambient configuration");

    assert_eq!(config.server.port, 3000);
    assert!(!config.instructions.contains("authored agent instruction"));
}

/// A topic with no config.toml is still discovered (it inherits global config).
#[test]
fn topic_without_config_still_discovered() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Topic directory exists but has no config.toml
    fs::create_dir_all(root.join("topics/bare")).unwrap();

    let topics = discover(root);
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0].name, "bare");
}

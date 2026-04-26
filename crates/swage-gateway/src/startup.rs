//! Gateway startup sequence extracted from main.rs.
//!
//! Encapsulates the full gateway lifecycle — configuration, storage, provider,
//! tools, sandbox, binding, serving, and shutdown — in a callable function.
//! Consumed by both `--server-mode` (background daemon) and `--foreground`
//! (inline execution).

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use swage_client::runtime::RuntimeMetadata;
use swage_config::LogLevel;

/// Run the gateway to completion.
///
/// Loads configuration, initialises all subsystems, binds the TCP listener,
/// writes runtime metadata so the CLI can discover the process, and then
/// serves until a shutdown signal is received. On shutdown, removes the
/// runtime metadata file and returns.
///
/// Optional overrides take precedence over configured values:
/// - `port` — TCP port to listen on
/// - `host` — host address to bind
/// - `config_path` — path to a TOML configuration file (name used for lookup)
///
/// Errors during startup are printed to stderr and the process exits.
pub async fn run_gateway(port: Option<u16>, host: Option<String>, config_path: Option<String>) {
    // Step 1: Load configuration. Errors go to stderr — no logging yet.
    let config_name = config_path.as_deref().unwrap_or("default");
    let mut config = match swage_config::load_resolved_config(config_name, "default") {
        Ok(c) => c,
        Err(e) => {
            for problem in &e.problems {
                eprintln!("{problem}");
            }
            std::process::exit(1);
        }
    };

    // Apply CLI overrides after loading the base config.
    if let Some(p) = port {
        config.server.port = p;
    }
    if let Some(h) = host {
        config.server.host = h;
    }

    // Step 2: Initialise logging from (possibly overridden) configuration.
    init_logging(&config);

    // Step 3: Resolve database path and ensure its directory exists.
    let db_path = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dir = PathBuf::from(home).join(".swage");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("Failed to create data directory {}: {e}", dir.display());
            std::process::exit(1);
        }
        dir.join("swage.db")
    };

    // Step 4: Open persistent storage.
    let raw_storage = open_storage(&db_path);

    // Step 5: Recover orphaned turns from a previous unclean shutdown.
    match crate::shutdown::recover_orphaned_turns(&*raw_storage, &*raw_storage).await {
        Ok(0) => {}
        Ok(n) => info!("Recovered {n} orphaned turn(s) from a previous shutdown"),
        Err(e) => {
            eprintln!("Startup recovery failed: {e}");
            std::process::exit(1);
        }
    }

    // Step 5b: Apply timeout policy to any approval requests that expired
    // while the gateway was offline. Approvals still within their window are
    // left as-is and will be re-emitted to clients on WebSocket connect.
    let message_ttl = std::time::Duration::from_secs(config.communication.message_ttl_secs);
    match crate::shutdown::recover_expired_approvals(&*raw_storage, message_ttl).await {
        Ok(counts) => {
            if counts.rejected_by_timeout > 0 {
                info!(
                    "Applied reject-policy timeout to {} expired approval(s)",
                    counts.rejected_by_timeout
                );
            }
            if counts.interrupted_by_timeout > 0 {
                info!(
                    "Applied interrupt-policy timeout to {} expired approval(s)",
                    counts.interrupted_by_timeout
                );
            }
            if counts.orphaned_cleaned > 0 {
                info!(
                    "Cleaned up {} orphaned approval(s) with deleted turns",
                    counts.orphaned_cleaned
                );
            }
        }
        Err(e) => {
            // Non-fatal: log a warning and continue. The gateway can still
            // serve — the affected approvals will remain stale in storage.
            warn!("Approval startup recovery failed: {e}");
        }
    }

    // Step 5b: Start the inter-agent message expiry background task.
    let expiry_handle = {
        let interval = std::time::Duration::from_secs(config.communication.expiry_interval_secs);
        swage_agent::expiry::spawn_expiry_task(
            Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>,
            message_ttl,
            interval,
        )
    };

    // Step 6: Create the LLM provider from configuration.
    let provider = create_provider(&config);
    let voice_provider = create_voice_provider(&config);

    // Step 7: Build the tool dispatch pipeline.
    let tool_dispatcher = build_tool_dispatcher(
        &config,
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>,
    );

    // Step 8: Assemble application state and build the router.
    let storage: Arc<dyn swage_storage::store::Storage> =
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>;
    let bench_store: Arc<dyn swage_storage::store::BenchStore> = raw_storage;
    let mut state = crate::state::AppState::new(
        storage,
        bench_store,
        provider,
        config.clone(),
        tool_dispatcher,
        None,
    );
    state.voice_provider = voice_provider;
    crate::execution_host::register_local_execution_host(&state).await;
    if let Err(error) = crate::execution_host::restore_registered_execution_hosts(&state).await {
        eprintln!("Failed to restore execution hosts: {error}");
        std::process::exit(1);
    }
    let router = crate::server::build_router(state.clone());
    let scheduler_handle = crate::schedule::spawn_scheduler_loop(state.clone());
    let heartbeat_handle = crate::heartbeat::spawn_heartbeat_loop(state.clone());
    let attachment_handle = crate::attachment::spawn_ingestion_loop(state.clone());

    // Step 10: Bind the TCP listener.
    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
        .parse()
        .unwrap_or_else(|e| {
            eprintln!(
                "Invalid address {}:{}: {e}",
                config.server.host, config.server.port
            );
            std::process::exit(1);
        });

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "Failed to bind to port {} — is something already using it? ({e})",
                addr.port()
            );
            std::process::exit(1);
        }
    };

    // Step 11: Write runtime metadata so the CLI can discover this process.
    //
    // Uses the actual bound address (which may differ from the configured one
    // when port 0 is used for OS-assigned ports). A failure here is logged as
    // a warning rather than aborting — the gateway can still serve, but `swage
    // gateway status` and `swage gateway stop` will not find it.
    let bound_addr = listener.local_addr().unwrap_or(addr);
    let metadata = RuntimeMetadata::new(std::process::id(), format!("http://{bound_addr}"));
    if let Err(e) = metadata.save() {
        warn!("Failed to write runtime metadata: {e} — CLI discovery will not work");
    }

    // Step 12: Serve until shutdown.
    info!("Swage gateway listening on {bound_addr}");
    let state_for_shutdown = state.clone();
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! {
            _ = crate::shutdown::graceful_shutdown() => {}
            _ = state_for_shutdown.shutdown_signal.cancelled() => {
                info!("HTTP shutdown endpoint triggered graceful shutdown");
            }
        }
        crate::shutdown::shutdown_active_turns(&state_for_shutdown).await;
    })
    .await
    .expect("server error");

    // Step 12b: Abort the message expiry background task.
    expiry_handle.abort();
    scheduler_handle.abort();
    heartbeat_handle.abort();
    attachment_handle.abort();

    // Step 12c: Remove runtime metadata now that the gateway has stopped.
    if let Err(e) = RuntimeMetadata::remove() {
        warn!("Failed to remove runtime metadata on shutdown: {e}");
    }
}

fn create_voice_provider(
    config: &swage_config::ResolvedConfig,
) -> Arc<dyn swage_provider::VoiceProvider> {
    let Some((provider_endpoint, model_endpoint)) = default_provider_model(config) else {
        return Arc::new(swage_provider::UnsupportedVoiceProvider::new(
            "voice is unavailable because no default provider/model pair is configured",
        ));
    };
    let provider_config =
        match swage_provider::ProviderConfig::try_from((provider_endpoint, model_endpoint)) {
            Ok(config) => config,
            Err(_) => {
                return Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                    "voice is unavailable because the provider configuration is invalid",
                ));
            }
        };

    match provider_endpoint.protocol {
        swage_config::Protocol::OpenAi | swage_config::Protocol::OpenRouter => Arc::new(
            swage_provider::OpenAiVoiceProvider::from_config(provider_config),
        ),
        swage_config::Protocol::Anthropic | swage_config::Protocol::Google => {
            Arc::new(swage_provider::UnsupportedVoiceProvider::new(format!(
                "voice is unavailable for provider protocol {:?}",
                provider_endpoint.protocol
            )))
        }
    }
}

fn provider_base_url(config: &swage_config::ProviderEndpointConfig) -> &str {
    config.base_url.as_deref().unwrap_or(match config.protocol {
        swage_config::Protocol::OpenAi => "https://api.openai.com/v1",
        swage_config::Protocol::Anthropic => "https://api.anthropic.com",
        swage_config::Protocol::Google => "https://generativelanguage.googleapis.com",
        swage_config::Protocol::OpenRouter => "https://openrouter.ai/api/v1",
    })
}

fn default_provider_model(
    config: &swage_config::ResolvedConfig,
) -> Option<(
    &swage_config::ProviderEndpointConfig,
    &swage_config::ModelEndpointConfig,
)> {
    let model = config.models.default_model()?;
    let provider = config.providers.entries.get(&model.provider_id)?;
    Some((provider, model))
}

// ---------------------------------------------------------------------------
// Logging helpers (private — not part of the public API)
// ---------------------------------------------------------------------------

/// Convert a `LogLevel` enum variant to its tracing directive string.
///
/// The directive is the level name in lowercase, matching the syntax accepted
/// by `EnvFilter::new` and RUST_LOG-style env vars.
fn log_level_to_directive(level: &LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

/// Build an `EnvFilter` from a config level and an optional explicit override.
///
/// When `override_directive` is `Some` and parses successfully, it takes
/// precedence over the config level (same semantics as SWAGE_LOG / RUST_LOG).
/// An invalid override is silently ignored, falling back to the config level.
///
/// Accepts an explicit override rather than reading the env directly so that
/// unit tests can exercise the logic without mutating process environment.
fn build_log_filter(config_level: &LogLevel, override_directive: Option<&str>) -> EnvFilter {
    override_directive
        .and_then(|d| EnvFilter::try_new(d).ok())
        .unwrap_or_else(|| EnvFilter::new(log_level_to_directive(config_level)))
}

fn select_log_override<'a>(
    swage_log: Option<&'a str>,
    legacy_tevik_log: Option<&'a str>,
) -> Option<&'a str> {
    swage_log.or(legacy_tevik_log)
}

/// Initialise the global tracing subscriber from resolved configuration.
///
/// Format is selected by the `SWAGE_LOG_FORMAT` env var:
/// - `"pretty"` → human-readable output (development)
/// - anything else → JSON structured output on stderr (production default)
///
/// `SWAGE_LOG` overrides the config log level using RUST_LOG directive syntax.
/// `TEVIK_LOG` remains as a legacy compatibility fallback.
fn init_logging(config: &swage_config::ResolvedConfig) {
    let swage_log = std::env::var("SWAGE_LOG").ok();
    let legacy_tevik_log = std::env::var("TEVIK_LOG").ok();
    let override_env = select_log_override(swage_log.as_deref(), legacy_tevik_log.as_deref());
    let filter = build_log_filter(&config.logging.level, override_env);

    let use_pretty = std::env::var("SWAGE_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("pretty"))
        .unwrap_or(false);

    if use_pretty {
        if let Err(e) = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init()
        {
            eprintln!("Warning: failed to initialise logging subscriber: {e}");
        }
    } else if let Err(e) = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
    {
        eprintln!("Warning: failed to initialise logging subscriber: {e}");
    }
}

// ---------------------------------------------------------------------------
// Construction helpers (private)
// ---------------------------------------------------------------------------

/// Open the persistent SQLite storage backend.
///
/// Returns `Arc<SqliteStorage>` rather than an erased trait object so the caller
/// can type-erase it into both `Arc<dyn Storage>` and `Arc<dyn BenchStore>`
/// from the same underlying connection pool.
fn open_storage(path: &std::path::Path) -> Arc<swage_storage::SqliteStorage> {
    Arc::new(
        swage_storage::SqliteStorage::open(path).unwrap_or_else(|e| {
            eprintln!("Failed to open database at {}: {e}", path.display());
            std::process::exit(1);
        }),
    )
}

/// Build the tool dispatch pipeline with all built-in tools from configuration.
fn build_tool_dispatcher(
    config: &swage_config::ResolvedConfig,
    storage: Arc<dyn swage_storage::store::Storage>,
) -> crate::tool::ToolDispatcher {
    use crate::tool::{
        Tool, agent_memory::AgentMemoryTool, file_list::FileListTool, file_read::FileReadTool,
        file_search::FileSearchTool, file_write::FileWriteTool, memory::MemoryTool,
        plugin::discover_plugin_tools, python_execute::PythonExecuteTool, shell::ShellTool,
        url_fetch::UrlFetchTool, web_search::WebSearchTool, workspace::resolve_workspace_root,
    };
    use std::time::Duration;

    let workspace_root = resolve_workspace_root(config);
    let max_output = config.tools.max_output_bytes;
    let shell_timeout = Duration::from_secs(config.tools.shell_timeout_secs);

    let base_tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ShellTool {
            workspace_root: workspace_root.clone(),
            timeout: shell_timeout,
            max_output_bytes: max_output,
        }),
        Arc::new(FileReadTool {
            workspace_root: workspace_root.clone(),
            max_output_bytes: max_output,
        }),
        Arc::new(FileWriteTool {
            workspace_root: workspace_root.clone(),
        }),
        Arc::new(FileSearchTool {
            workspace_root: workspace_root.clone(),
            max_output_bytes: max_output,
        }),
        Arc::new(FileListTool {
            workspace_root: workspace_root.clone(),
        }),
        Arc::new(WebSearchTool {
            provider_config: config.tools.search.clone(),
        }),
        Arc::new(UrlFetchTool::default()),
        Arc::new(swage_tool::delegate::DelegateTool),
        // MemoryTool derives the memory path from ctx.working_directory() at runtime.
        Arc::new(MemoryTool),
        Arc::new(AgentMemoryTool),
    ];

    let plugin_tools = resolve_plugin_directory(config)
        .map(|plugin_directory| discover_plugin_tools(&plugin_directory, shell_timeout))
        .unwrap_or_else(|| (vec![], vec![]));
    for warning in &plugin_tools.1 {
        warn!("{warning}");
    }

    let mut non_python_tools = base_tools;
    non_python_tools.extend(plugin_tools.0.iter().cloned());

    // Sub-dispatcher: all non-python tools, excluding python_execute to prevent
    // recursive invocation through the python_execute tool.
    let sub_dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(
        non_python_tools.iter().cloned(),
    ));

    let mut all_tools = non_python_tools;

    match swage_python::PythonWorkerRuntime::new(config.tools.python_execute.clone()) {
        Ok(runtime) if runtime.python_execute_available() => {
            let runtime = Arc::new(runtime);
            match swage_python::WorkerPool::new(Arc::clone(&runtime)) {
                Ok(pool) => {
                    all_tools.push(Arc::new(PythonExecuteTool::new(
                        runtime,
                        pool,
                        sub_dispatcher,
                        storage,
                    )));
                    info!("Python worker runtime available — python_execute tool registered");
                }
                Err(error) => {
                    warn!(
                        "Python worker pool could not be created ({error}), python_execute tool disabled"
                    );
                }
            }
        }
        Ok(_) => {
            warn!(
                "Python worker runtime resolved but sandbox is unavailable; python_execute tool disabled"
            );
        }
        Err(e) => {
            warn!("Python runtime not available ({e}), python_execute tool disabled");
        }
    }

    info!(
        "Registered {} tool(s), workspace root: {}",
        all_tools.len(),
        workspace_root.display(),
    );

    crate::tool::ToolDispatcher::allow_all(all_tools)
}

fn resolve_plugin_directory(config: &swage_config::ResolvedConfig) -> Option<PathBuf> {
    let directory = config.tools.plugin_directory.as_ref()?;
    let path = PathBuf::from(directory);
    if path.is_absolute() {
        Some(path)
    } else {
        swage_config::discovery::resolve_root()
            .ok()
            .map(|root| root.join(path.clone()))
            .or(Some(path))
    }
}

/// Create the LLM provider from resolved configuration.
fn create_provider(config: &swage_config::ResolvedConfig) -> Arc<dyn swage_provider::LlmProvider> {
    use swage_provider::ProviderConfig;
    use swage_provider::builder::ProviderChainBuilder;
    use swage_provider::retry::RetryConfig;

    let Some((provider_endpoint, model_endpoint)) = default_provider_model(config) else {
        tracing::warn!(
            "No default provider/model pair is configured. \
             Messaging will fail until configuration is fixed."
        );
        return Arc::new(swage_provider::UnsupportedLlmProvider::new(
            "no default provider/model pair is configured",
        ));
    };
    let provider_config = match ProviderConfig::try_from((provider_endpoint, model_endpoint)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Provider not fully configured: {e}. \
                 The server will start, but messaging will fail \
                 until the API key is set."
            );
            ProviderConfig::keyless(
                &model_endpoint.model_id,
                provider_base_url(provider_endpoint),
            )
        }
    };

    let retry = RetryConfig::default();

    let provider: Box<dyn swage_provider::LlmProvider> = match provider_endpoint.protocol {
        swage_config::Protocol::Anthropic => ProviderChainBuilder::new()
            .provider(swage_provider::anthropic::AnthropicProvider::from_config(
                provider_config,
            ))
            .with_retry(retry)
            .build(),
        swage_config::Protocol::OpenAi | swage_config::Protocol::OpenRouter => {
            ProviderChainBuilder::new()
                .provider(swage_provider::openai::OpenAiProvider::from_config(
                    provider_config,
                ))
                .with_retry(retry)
                .build()
        }
        swage_config::Protocol::Google => ProviderChainBuilder::new()
            .provider(swage_provider::google::GoogleProvider::from_config(
                provider_config,
            ))
            .with_retry(retry)
            .build(),
    };

    Arc::from(provider)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use swage_config::LogLevel;

    // log_level_to_directive must map every LogLevel variant correctly.
    #[test]
    fn log_level_directive_maps_all_variants() {
        assert_eq!(log_level_to_directive(&LogLevel::Trace), "trace");
        assert_eq!(log_level_to_directive(&LogLevel::Debug), "debug");
        assert_eq!(log_level_to_directive(&LogLevel::Info), "info");
        assert_eq!(log_level_to_directive(&LogLevel::Warn), "warn");
        assert_eq!(log_level_to_directive(&LogLevel::Error), "error");
    }

    // No override: filter should encode the config level.
    #[test]
    fn build_log_filter_uses_config_level_when_no_override() {
        let filter = build_log_filter(&LogLevel::Debug, None);
        assert!(
            filter.to_string().contains("debug"),
            "expected 'debug' in filter, got: {filter}",
        );
    }

    // Valid override takes precedence over config level.
    #[test]
    fn build_log_filter_uses_override_when_valid() {
        let filter = build_log_filter(&LogLevel::Info, Some("warn"));
        assert!(
            filter.to_string().contains("warn"),
            "expected 'warn' in filter, got: {filter}",
        );
    }

    // An unparseable override should fall back to the config level.
    #[test]
    fn build_log_filter_falls_back_on_invalid_override() {
        let filter = build_log_filter(&LogLevel::Error, Some("!!!invalid!!!"));
        assert!(
            filter.to_string().contains("error"),
            "expected 'error' in filter, got: {filter}",
        );
    }

    #[test]
    fn select_log_override_prefers_swage_name() {
        assert_eq!(
            select_log_override(Some("debug"), Some("warn")),
            Some("debug")
        );
    }

    #[test]
    fn select_log_override_falls_back_to_legacy_name() {
        assert_eq!(select_log_override(None, Some("warn")), Some("warn"));
    }

    #[tokio::test]
    async fn build_tool_dispatcher_registers_delegate_tool() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let dispatcher = build_tool_dispatcher(
            &swage_config::ResolvedConfig::default(),
            storage as Arc<dyn swage_storage::store::Storage>,
        );

        assert!(
            dispatcher
                .registry()
                .get(swage_tool::delegate::DELEGATE_TOOL_NAME)
                .is_some(),
            "delegate tool should be registered with the built-in dispatcher",
        );
    }

    #[tokio::test]
    async fn build_tool_dispatcher_registers_plugin_tools_from_configured_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let plugin_dir = temp_dir.path().join("plugins").join("echo-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("capability.json"),
            serde_json::json!({
                "description": "Echo plugin",
                "tool": {
                    "name": "echo_plugin",
                    "description": "Echo plugin",
                    "parameters": {"type": "object", "properties": {"text": {"type": "string"}}},
                    "executable": "run.py"
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("run.py"),
            "#!/usr/bin/env python3\nprint('{}')\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(plugin_dir.join("run.py"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(plugin_dir.join("run.py"), perms).unwrap();
        }

        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let mut config = swage_config::ResolvedConfig::default();
        config.tools.plugin_directory = Some(temp_dir.path().join("plugins").display().to_string());

        let dispatcher =
            build_tool_dispatcher(&config, storage as Arc<dyn swage_storage::store::Storage>);

        assert!(dispatcher.registry().get("echo_plugin").is_some());
    }

    #[tokio::test]
    async fn create_provider_fails_closed_when_default_model_provider_is_missing() {
        let mut config = swage_config::ResolvedConfig::default();
        config
            .models
            .entries
            .get_mut(&config.models.default)
            .expect("default model entry")
            .provider_id = "missing-provider".to_string();

        let provider = create_provider(&config);
        let result = provider
            .complete(swage_core::CompletionRequest {
                messages: vec![],
                model: None,
                max_tokens: None,
                temperature: None,
                stop_sequences: None,
                metadata: None,
                tool_definitions: vec![],
            })
            .await;

        assert_eq!(provider.model_id(), "unavailable");
        assert!(matches!(
            result,
            Err(swage_core::ProviderError::InvalidConfig { .. })
        ));
    }
}

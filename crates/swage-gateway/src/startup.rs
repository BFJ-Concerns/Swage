//! Gateway startup sequence extracted from main.rs.
//!
//! Encapsulates the full gateway lifecycle — configuration, storage, provider,
//! tools, sandbox, binding, serving, and shutdown — in a callable function.
//! Consumed by both `--server-mode` (background daemon) and `--foreground`
//! (inline execution).

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use swage_config::LogLevel;
use swage_client::runtime::RuntimeMetadata;

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
pub async fn run_gateway(
    port: Option<u16>,
    host: Option<String>,
    config_path: Option<String>,
) {
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
    match crate::shutdown::recover_orphaned_turns(&*raw_storage).await {
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
    match crate::shutdown::recover_expired_approvals(&*raw_storage).await {
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
        let ttl = std::time::Duration::from_secs(config.communication.message_ttl_secs);
        let interval = std::time::Duration::from_secs(config.communication.expiry_interval_secs);
        swage_agent::expiry::spawn_expiry_task(
            Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>,
            ttl,
            interval,
        )
    };

    // Step 6: Create the LLM provider from configuration.
    let provider = create_provider(&config);

    // Step 7: Build the tool dispatch pipeline.
    let tool_dispatcher = build_tool_dispatcher(
        &config,
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>,
    );

    // Step 8: Initialise the sandbox when enabled.
    let sandbox = build_sandbox(&config, Arc::clone(&provider)).await;

    // Step 9: Assemble application state and build the router.
    let storage: Arc<dyn swage_storage::store::Storage> =
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>;
    let project_store: Arc<dyn swage_storage::store::ProjectStore> = raw_storage;
    let state = crate::state::AppState::new(
        storage,
        project_store,
        provider,
        config.clone(),
        tool_dispatcher,
        sandbox,
    );
    let router = crate::server::build_router(state.clone());

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
    let metadata = RuntimeMetadata::new(
        std::process::id(),
        format!("http://{bound_addr}"),
    );
    if let Err(e) = metadata.save() {
        warn!("Failed to write runtime metadata: {e} — CLI discovery will not work");
    }

    // Step 12: Serve until shutdown.
    info!("Swage gateway listening on {bound_addr}");
    let state_for_shutdown = state.clone();
    let idle_timeout = config.lifecycle.idle_timeout;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! {
            _ = crate::shutdown::graceful_shutdown() => {}
            _ = crate::shutdown::idle_monitor(state_for_shutdown.clone(), idle_timeout) => {
                info!("Idle timeout triggered shutdown");
            }
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

    // Step 12c: Remove runtime metadata now that the gateway has stopped.
    if let Err(e) = RuntimeMetadata::remove() {
        warn!("Failed to remove runtime metadata on shutdown: {e}");
    }

    // Step 13: Shut down the sandbox cleanly.
    if let Some(mut sandbox_arc) = state.sandbox {
        if let Some(sandbox) = Arc::get_mut(&mut sandbox_arc) {
            if let Err(e) = sandbox.shutdown().await {
                warn!("Sandbox shutdown returned an error: {e}");
            } else {
                info!("Sandbox agent shut down cleanly");
            }
        } else {
            warn!(
                "Sandbox Arc has unexpected extra references at shutdown \
                 — skipping clean shutdown"
            );
        }
    }
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
        LogLevel::Info  => "info",
        LogLevel::Warn  => "warn",
        LogLevel::Error => "error",
    }
}

/// Build an `EnvFilter` from a config level and an optional explicit override.
///
/// When `override_directive` is `Some` and parses successfully, it takes
/// precedence over the config level (same semantics as TEVIK_LOG / RUST_LOG).
/// An invalid override is silently ignored, falling back to the config level.
///
/// Accepts an explicit override rather than reading the env directly so that
/// unit tests can exercise the logic without mutating process environment.
fn build_log_filter(config_level: &LogLevel, override_directive: Option<&str>) -> EnvFilter {
    override_directive
        .and_then(|d| EnvFilter::try_new(d).ok())
        .unwrap_or_else(|| EnvFilter::new(log_level_to_directive(config_level)))
}

/// Initialise the global tracing subscriber from resolved configuration.
///
/// Format is selected by the `SWAGE_LOG_FORMAT` env var:
/// - `"pretty"` → human-readable output (development)
/// - anything else → JSON structured output on stderr (production default)
///
/// `TEVIK_LOG` overrides the config log level using RUST_LOG directive syntax.
fn init_logging(config: &swage_config::ResolvedConfig) {
    let override_env = std::env::var("TEVIK_LOG").ok();
    let filter = build_log_filter(&config.logging.level, override_env.as_deref());

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
    } else {
        if let Err(e) = tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init()
        {
            eprintln!("Warning: failed to initialise logging subscriber: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Construction helpers (private)
// ---------------------------------------------------------------------------

/// Open the persistent SQLite storage backend.
///
/// Returns `Arc<SqliteStorage>` rather than an erased trait object so the caller
/// can type-erase it into both `Arc<dyn Storage>` and `Arc<dyn ProjectStore>`
/// from the same underlying connection pool.
fn open_storage(path: &std::path::Path) -> Arc<swage_storage::SqliteStorage> {
    Arc::new(swage_storage::SqliteStorage::open(path).unwrap_or_else(|e| {
        eprintln!("Failed to open database at {}: {e}", path.display());
        std::process::exit(1);
    }))
}

/// Build the tool dispatch pipeline with all built-in tools from configuration.
fn build_tool_dispatcher(
    config: &swage_config::ResolvedConfig,
    storage: Arc<dyn swage_storage::store::Storage>,
) -> crate::tool::ToolDispatcher {
    use std::time::Duration;
    use crate::tool::{
        Tool,
        file_list::FileListTool,
        file_read::FileReadTool,
        file_search::FileSearchTool,
        file_write::FileWriteTool,
        memory::MemoryTool,
        shell::ShellTool,
        url_fetch::UrlFetchTool,
        web_search::WebSearchTool,
        workspace::resolve_workspace_root,
        python_execute::PythonExecuteTool,
    };

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
        // MemoryTool derives the memory path from ctx.working_directory() at runtime.
        Arc::new(MemoryTool),
    ];

    // Sub-dispatcher: base tools only, python_execute excluded to prevent
    // recursive invocation through the python_execute tool.
    let sub_dispatcher = Arc::new(crate::tool::ToolDispatcher::allow_all(
        base_tools.iter().cloned(),
    ));

    let mut all_tools = base_tools;

    match swage_python::runtime::PythonRuntime::new(None) {
        Ok(runtime) => {
            all_tools.push(Arc::new(PythonExecuteTool::new(
                Arc::new(runtime),
                Duration::from_secs(config.tools.shell_timeout_secs),
                sub_dispatcher,
                storage,
            )));
            info!("Python runtime available — python_execute tool registered");
        }
        Err(e) => {
            warn!("Python runtime not available ({e}), python_execute tool disabled");
        }
    }

    info!(
        "Registered {} built-in tool(s), workspace root: {}",
        all_tools.len(),
        workspace_root.display(),
    );

    crate::tool::ToolDispatcher::allow_all(all_tools)
}

/// Create the LLM provider from resolved configuration.
fn create_provider(
    config: &swage_config::ResolvedConfig,
) -> Arc<dyn swage_provider::LlmProvider> {
    use swage_provider::ProviderConfig;
    use swage_provider::builder::ProviderChainBuilder;
    use swage_provider::retry::RetryConfig;

    let provider_config = match ProviderConfig::try_from(&config.provider) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Provider not fully configured: {e}. \
                 The server will start, but messaging will fail \
                 until the API key is set."
            );
            let base_url = match config.provider.protocol {
                swage_config::Protocol::OpenAi => "https://api.openai.com/v1",
                swage_config::Protocol::Anthropic => "https://api.anthropic.com",
                swage_config::Protocol::Google => "https://generativelanguage.googleapis.com",
                swage_config::Protocol::OpenRouter => "https://openrouter.ai/api/v1",
            };
            ProviderConfig::keyless(&config.provider.model, base_url)
        }
    };

    let retry = RetryConfig::default();

    let provider: Box<dyn swage_provider::LlmProvider> = match config.provider.protocol {
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

/// Initialise the sandbox agent when configuration enables it.
async fn build_sandbox(
    config: &swage_config::ResolvedConfig,
    provider: Arc<dyn swage_provider::LlmProvider>,
) -> Option<Arc<dyn swage_sandbox::Sandbox>> {
    use swage_core::SandboxError;
    use swage_sandbox::{Sandbox, llm_handler::HostLlmHandler};
    use crate::tool::workspace::resolve_workspace_root;

    if !config.sandbox.enabled {
        return None;
    }

    let workspace_root = resolve_workspace_root(config);

    let sandbox_config = match swage_sandbox::SandboxConfig::builder()
        .working_directory(&workspace_root)
        .read_write_mount(&workspace_root)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to build sandbox config: {e} — sandbox disabled");
            return None;
        }
    };

    let llm_handler = HostLlmHandler::new(move |req: swage_core::CompletionRequest| {
        let p = Arc::clone(&provider);
        async move {
            p.complete(req)
                .await
                .map_err(|e| SandboxError::IpcError { message: e.to_string() })
        }
    });

    let mut sandbox = swage_sandbox::BubblewrapSandbox::new(sandbox_config, llm_handler);
    match sandbox.spawn_agent().await {
        Ok(()) => {
            info!("Sandbox agent spawned — tool calls will run in an isolated process");
            Some(Arc::new(sandbox) as Arc<dyn swage_sandbox::Sandbox>)
        }
        Err(SandboxError::Unavailable { .. }) => {
            warn!(
                "Sandbox enabled but bubblewrap not available — \
                 falling back to direct execution"
            );
            None
        }
        Err(e) => {
            warn!("Sandbox initialisation failed: {e} — falling back to direct execution");
            None
        }
    }
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
        assert_eq!(log_level_to_directive(&LogLevel::Info),  "info");
        assert_eq!(log_level_to_directive(&LogLevel::Warn),  "warn");
        assert_eq!(log_level_to_directive(&LogLevel::Error), "error");
    }

    // No override: filter should encode the config level.
    #[test]
    fn build_log_filter_uses_config_level_when_no_override() {
        let filter = build_log_filter(&LogLevel::Debug, None);
        assert!(
            filter.to_string().contains("debug"),
            "expected 'debug' in filter, got: {}",
            filter,
        );
    }

    // Valid override takes precedence over config level.
    #[test]
    fn build_log_filter_uses_override_when_valid() {
        let filter = build_log_filter(&LogLevel::Info, Some("warn"));
        assert!(
            filter.to_string().contains("warn"),
            "expected 'warn' in filter, got: {}",
            filter,
        );
    }

    // An unparseable override should fall back to the config level.
    #[test]
    fn build_log_filter_falls_back_on_invalid_override() {
        let filter = build_log_filter(&LogLevel::Error, Some("!!!invalid!!!"));
        assert!(
            filter.to_string().contains("error"),
            "expected 'error' in filter, got: {}",
            filter,
        );
    }
}

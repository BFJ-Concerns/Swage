use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use swage_config::LogLevel;

use swage_config::load_resolved_config;
use swage_gateway::{
    server, shutdown,
    state::AppState,
    tool::{
        ToolDispatcher,
        file_list::FileListTool,
        file_read::FileReadTool,
        file_search::FileSearchTool,
        file_write::FileWriteTool,
        memory::MemoryTool,
        shell::ShellTool,
        url_fetch::UrlFetchTool,
        web_search::WebSearchTool,
        workspace::resolve_workspace_root,
    },
};
use swage_sandbox::{BubblewrapSandbox, Sandbox, SandboxConfig};

#[tokio::main]
async fn main() {
    // Step 1: Load configuration first so the logging subscriber can be
    // initialised from the configured log level. Errors at this stage go
    // to stderr directly — no structured logging is available yet.
    let config = match load_resolved_config("default", "default") {
        Ok(c) => c,
        Err(e) => {
            for problem in &e.problems {
                eprintln!("{problem}");
            }
            std::process::exit(1);
        }
    };

    // Step 2: Initialise the logging subscriber from configuration.
    // JSON structured format on stderr for production; set SWAGE_LOG_FORMAT=pretty
    // for human-readable development output. TEVIK_LOG overrides config level.
    init_logging(&config);

    // Step 3: Determine database path (~/.swage/swage.db) and ensure the
    // parent directory exists before we try to open the file.
    let db_path = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dir = PathBuf::from(home).join(".swage");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("Failed to create data directory {}: {e}", dir.display());
            std::process::exit(1);
        }
        dir.join("swage.db")
    };

    // Step 4: Open persistent storage. SqliteStorage implements both Storage
    // and ProjectStore, so we open one instance and type-erase it twice.
    let raw_storage = open_storage(&db_path);

    // Step 5: Recover any turns left Processing by a previous unclean shutdown.
    // This must complete before the server begins accepting connections.
    match shutdown::recover_orphaned_turns(&*raw_storage).await {
        Ok(0) => {}
        Ok(n) => info!("Recovered {n} orphaned turn(s) from a previous shutdown"),
        Err(e) => {
            eprintln!("Startup recovery failed: {e}");
            std::process::exit(1);
        }
    }

    // Step 6: Create the LLM provider from configuration.
    let provider = create_provider(&config);

    // Step 7: Build the tool dispatch pipeline with all built-in tools.
    let tool_dispatcher = build_tool_dispatcher(
        &config,
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>,
    );

    // Step 8: Initialise the sandbox agent when enabled. The sandbox runs tool
    // calls inside a bubblewrap namespace with credential isolation via IPC.
    // When disabled or when bwrap is unavailable, tool dispatch falls back to
    // the normal in-process path with no sandbox overhead.
    let sandbox = build_sandbox(&config, Arc::clone(&provider)).await;

    // Step 9: Assemble application state and build the router.
    // Type-erase the storage into its two distinct trait objects.
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>;
    let project_store: Arc<dyn swage_storage::store::ProjectStore> = raw_storage;
    let state = AppState::new(storage, project_store, provider, config.clone(), tool_dispatcher, sandbox);
    let router = server::build_router(state.clone());

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

    // Step 11: Start serving. Graceful shutdown cancels active turns and
    // drains in-flight connections before the process exits.
    //
    // Either a signal (SIGTERM / ctrl-c) or the idle monitor reaching its
    // threshold triggers the shutdown sequence. Both paths call
    // shutdown_active_turns so in-flight work is cancelled cleanly.
    info!("Swage gateway listening on {addr}");
    let state_for_shutdown = state.clone();
    let idle_timeout = config.lifecycle.idle_timeout;
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = shutdown::graceful_shutdown() => {}
                _ = shutdown::idle_monitor(state_for_shutdown.clone(), idle_timeout) => {
                    info!("Idle timeout triggered shutdown");
                }
            }
            shutdown::shutdown_active_turns(&state_for_shutdown).await;
        })
        .await
        .expect("server error");

    // Step 12: Shut down sandbox cleanly. After serve completes, all route
    // handler clones have been dropped, leaving `state` as the sole Arc holder.
    // Arc::get_mut therefore succeeds and we can call shutdown() mutably.
    if let Some(mut sandbox_arc) = state.sandbox {
        if let Some(sandbox) = Arc::get_mut(&mut sandbox_arc) {
            if let Err(e) = sandbox.shutdown().await {
                warn!("Sandbox shutdown returned an error: {e}");
            } else {
                info!("Sandbox agent shut down cleanly");
            }
        } else {
            // Unexpected: another reference still alive. The OS will reclaim
            // the child process when the host exits.
            warn!("Sandbox Arc has unexpected extra references at shutdown — skipping clean shutdown");
        }
    }
}

// ---------------------------------------------------------------------------
// Construction helpers
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
///
/// Base tools (shell, file ops, web) are assembled first and wrapped in a
/// shared sub-dispatcher that `PythonExecuteTool` uses for sub-tool dispatch.
/// This keeps python_execute out of the sub-dispatcher to prevent recursion.
///
/// The final dispatcher uses allow-all permissions and no rate limits — the
/// default posture for the embedded gateway. Restrictive configs are a future
/// configuration concern and can be wired through `ToolDispatcher::new`.
fn build_tool_dispatcher(
    config: &swage_config::ResolvedConfig,
    storage: Arc<dyn swage_storage::store::Storage>,
) -> ToolDispatcher {
    use std::time::Duration;
    use swage_gateway::tool::python_execute::PythonExecuteTool;

    let workspace_root = resolve_workspace_root(config);
    let max_output = config.tools.max_output_bytes;
    let shell_timeout = Duration::from_secs(config.tools.shell_timeout_secs);

    // Collect base tools. These are shared between the sub-dispatcher (for
    // python_execute's sub-tool calls) and the final top-level dispatcher.
    let base_tools: Vec<Arc<dyn swage_gateway::tool::Tool>> = vec![
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
        // MemoryTool derives the memory path from ctx.working_directory() at runtime —
        // no constructor args needed.
        Arc::new(MemoryTool),
    ];

    // Sub-dispatcher: base tools only. python_execute is excluded so that
    // GatewayToolDispatcher cannot recursively invoke python_execute.
    let sub_dispatcher = Arc::new(ToolDispatcher::allow_all(base_tools.iter().cloned()));

    // Collect all tools for the top-level dispatcher, starting with the base set.
    let mut all_tools = base_tools;

    // Attempt to add python_execute if a Python runtime is available.
    // Graceful degradation: if Python isn't found, the tool is simply absent
    // from the model's tool list — everything else works normally.
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

    ToolDispatcher::allow_all(all_tools)
}

// ---------------------------------------------------------------------------
// Logging initialisation helpers
// ---------------------------------------------------------------------------

/// Convert a `LogLevel` enum variant to its tracing directive string.
///
/// The directive is the level name in lowercase, matching the syntax accepted
/// by `EnvFilter::new` and RUST_LOG-style env vars (e.g. `"debug"`, `"warn"`).
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
    // Try the explicit override first; fall back to the config level.
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
/// The `TEVIK_LOG` env var overrides the config log level using RUST_LOG
/// directive syntax (e.g. `TEVIK_LOG=debug` or `TEVIK_LOG=swage_gateway=trace`).
///
/// Failures are reported via `eprintln!` and do not crash the gateway — a
/// subscriber may already be registered (common in tests).
fn init_logging(config: &swage_config::ResolvedConfig) {
    let override_env = std::env::var("TEVIK_LOG").ok();
    let filter = build_log_filter(&config.logging.level, override_env.as_deref());

    let use_pretty = std::env::var("SWAGE_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("pretty"))
        .unwrap_or(false);

    if use_pretty {
        // Human-readable output — useful during development.
        if let Err(e) = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init()
        {
            eprintln!("Warning: failed to initialise logging subscriber: {e}");
        }
    } else {
        // JSON structured output — machine-parseable with timestamp, level,
        // target, and message fields for production log ingestion.
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_directive_maps_all_variants() {
        assert_eq!(log_level_to_directive(&LogLevel::Trace), "trace");
        assert_eq!(log_level_to_directive(&LogLevel::Debug), "debug");
        assert_eq!(log_level_to_directive(&LogLevel::Info),  "info");
        assert_eq!(log_level_to_directive(&LogLevel::Warn),  "warn");
        assert_eq!(log_level_to_directive(&LogLevel::Error), "error");
    }

    #[test]
    fn build_log_filter_uses_config_level_when_no_override() {
        // No override: filter should encode the config level.
        let filter = build_log_filter(&LogLevel::Debug, None);
        assert!(
            filter.to_string().contains("debug"),
            "expected 'debug' in filter, got: {}",
            filter,
        );
    }

    #[test]
    fn build_log_filter_uses_override_when_valid() {
        // Valid override takes precedence over config level.
        let filter = build_log_filter(&LogLevel::Info, Some("warn"));
        assert!(
            filter.to_string().contains("warn"),
            "expected 'warn' in filter, got: {}",
            filter,
        );
    }

    #[test]
    fn build_log_filter_falls_back_on_invalid_override() {
        // An unparseable override should fall back to the config level.
        let filter = build_log_filter(&LogLevel::Error, Some("!!!invalid!!!"));
        assert!(
            filter.to_string().contains("error"),
            "expected 'error' in filter, got: {}",
            filter,
        );
    }
}

/// Create the LLM provider from resolved configuration.
///
/// Dispatches to the correct provider implementation based on the configured
/// protocol (Anthropic, OpenAI-compatible, Google, OpenRouter). The provider
/// is wrapped in a retry decorator via `ProviderChainBuilder` for transient
/// failure resilience.
///
/// If the API key environment variable is not set, the server starts anyway
/// with a warning — the error surfaces through the WebSocket when a message
/// is sent, rather than blocking startup.
fn create_provider(config: &swage_config::ResolvedConfig) -> Arc<dyn swage_provider::LlmProvider> {
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
            // Derive a fallback base URL so the provider can at least be
            // constructed. Requests will fail with an auth error at the
            // provider, which propagates as an error WebSocket event.
            let base_url = match config.provider.protocol {
                swage_config::Protocol::OpenAi => "https://api.openai.com/v1",
                swage_config::Protocol::Anthropic => "https://api.anthropic.com",
                swage_config::Protocol::Google => "https://generativelanguage.googleapis.com",
                swage_config::Protocol::OpenRouter => "https://openrouter.ai/api/v1",
            };
            ProviderConfig::keyless(&config.provider.model, base_url)
        }
    };

    // Dispatch to the correct provider implementation based on protocol,
    // wrapping each in the reliability stack (retry with sensible defaults).
    // Circuit breaker is omitted for single-provider setups — it adds
    // complexity without a failover target to route to.
    let retry = RetryConfig::default();

    let provider: Box<dyn swage_provider::LlmProvider> = match config.provider.protocol {
        swage_config::Protocol::Anthropic => {
            ProviderChainBuilder::new()
                .provider(swage_provider::anthropic::AnthropicProvider::from_config(
                    provider_config,
                ))
                .with_retry(retry)
                .build()
        }
        swage_config::Protocol::OpenAi | swage_config::Protocol::OpenRouter => {
            ProviderChainBuilder::new()
                .provider(swage_provider::openai::OpenAiProvider::from_config(
                    provider_config,
                ))
                .with_retry(retry)
                .build()
        }
        swage_config::Protocol::Google => {
            ProviderChainBuilder::new()
                .provider(swage_provider::google::GoogleProvider::from_config(
                    provider_config,
                ))
                .with_retry(retry)
                .build()
        }
    };

    Arc::from(provider)
}

/// Initialise the sandbox agent when configuration enables it.
///
/// Creates a `BubblewrapSandbox` with the workspace root as its working
/// directory and spawns the child agent process. Returns `None` if:
/// - `config.sandbox.enabled` is false (disabled by configuration), or
/// - bubblewrap (`bwrap`) is not installed on the host, or
/// - the agent process fails to start for any other reason.
///
/// In all `None` cases, tool calls fall back to the normal in-process
/// dispatch path — no sandbox overhead, no credential isolation.
async fn build_sandbox(
    config: &swage_config::ResolvedConfig,
    provider: Arc<dyn swage_provider::LlmProvider>,
) -> Option<Arc<dyn Sandbox>> {
    use swage_core::SandboxError;
    use swage_sandbox::llm_handler::HostLlmHandler;

    if !config.sandbox.enabled {
        return None;
    }

    let workspace_root = resolve_workspace_root(config);

    // Build sandbox configuration. The workspace root is mounted read-write
    // so tool calls can write files in the project directory.
    let sandbox_config = match SandboxConfig::builder()
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

    // Wire the host's LLM provider into the sandbox's LLM proxy handler.
    // The sandboxed agent never sees API keys — completions are forwarded
    // through this closure from the host side.
    let llm_handler = HostLlmHandler::new(move |req: swage_core::CompletionRequest| {
        let p = Arc::clone(&provider);
        async move {
            p.complete(req)
                .await
                .map_err(|e| SandboxError::IpcError { message: e.to_string() })
        }
    });

    let mut sandbox = BubblewrapSandbox::new(sandbox_config, llm_handler);
    match sandbox.spawn_agent().await {
        Ok(()) => {
            info!("Sandbox agent spawned — tool calls will run in an isolated process");
            Some(Arc::new(sandbox) as Arc<dyn Sandbox>)
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

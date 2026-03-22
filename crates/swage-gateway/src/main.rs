use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

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

#[tokio::main]
async fn main() {
    // Step 1: Initialise tracing (env filter, default INFO).
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Step 2: Load configuration. Report all validation errors before exiting.
    let config = match load_resolved_config("default", "default") {
        Ok(c) => c,
        Err(e) => {
            for problem in &e.problems {
                eprintln!("{problem}");
            }
            std::process::exit(1);
        }
    };

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

    // Step 8: Assemble application state and build the router.
    // Type-erase the storage into its two distinct trait objects.
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>;
    let project_store: Arc<dyn swage_storage::store::ProjectStore> = raw_storage;
    let state = AppState::new(storage, project_store, provider, config.clone(), tool_dispatcher);
    let router = server::build_router(state.clone());

    // Step 9: Bind the TCP listener.
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

    // Step 10: Start serving. Graceful shutdown cancels active turns and
    // drains in-flight connections before the process exits.
    info!("Swage gateway listening on {addr}");
    let state_for_shutdown = state.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown::graceful_shutdown().await;
            shutdown::shutdown_active_turns(&state_for_shutdown).await;
        })
        .await
        .expect("server error");
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

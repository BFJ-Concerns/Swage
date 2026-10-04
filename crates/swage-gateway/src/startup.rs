//! Gateway startup sequence extracted from main.rs.
//!
//! Encapsulates the full gateway lifecycle — configuration, storage, provider,
//! tools, sandbox, binding, serving, and shutdown — in a callable function.
//! Consumed by both `--server-mode` (background daemon) and `--foreground`
//! (inline execution).

use std::{collections::BTreeSet, net::SocketAddr, path::PathBuf, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use swage_client::runtime::RuntimeMetadata;
use swage_config::LogLevel;
use swage_storage::{ChannelQueueStore, WorkflowStore};
use tokio_util::sync::CancellationToken;

const CODEX_COMPLETION_UNSUPPORTED_REASON: &str = "Codex turns run through the App Server harness";

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
    // The gateway's own configuration is the global layer with a default
    // agent overlay; a configured name selects the topic whose layer applies.
    let scope = match config_path.as_deref() {
        Some(name) => swage_core::ScopeLocation::topic(name),
        None => swage_core::ScopeLocation::General,
    };
    let mut config = match swage_config::load_resolved_config(&scope, "default") {
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

    // Step 3: Resolve the canonical Swage data root and make sure managed
    // secrets have an encryption key before migrations or provider setup can
    // read/write encrypted rows.
    let root = {
        match swage_config::discovery::resolve_root() {
            Ok(path) => path,
            Err(problem) => {
                eprintln!("{problem}");
                std::process::exit(1);
            }
        }
    };
    // Name the root before anything opens under it: a gateway launched
    // without SWAGE_HOME silently resolves to the operator's own root, and
    // the log line is what makes that visible.
    tracing::info!(
        root = %root.display(),
        from_environment = std::env::var_os("SWAGE_HOME").is_some(),
        "Swage data root resolved"
    );
    if let Err(problem) = ensure_managed_secret_key(&root) {
        eprintln!("{problem}");
        std::process::exit(1);
    }
    if let Err(error) = crate::shipped_skill_projection::regenerate(&root).await {
        warn!(%error, "failed to regenerate shipped skill resources");
    }
    let db_path = root.join("swage.db");

    // Step 4: Open persistent storage.
    let raw_storage = open_storage(&db_path);

    // Step 5: Requeue channel messages claimed by a crashed gateway before
    // interrupting the orphaned turns they were attached to. A running process
    // cannot survive startup, so any in-flight turn here is a crash orphan.
    match raw_storage.requeue_presented_for_inflight_turns().await {
        Ok(0) => {}
        Ok(n) => info!("Requeued {n} channel message(s) from orphaned turn(s)"),
        Err(e) => {
            eprintln!("Channel queue startup recovery failed: {e}");
            std::process::exit(1);
        }
    }

    // Step 5b: Recover orphaned turns from a previous unclean shutdown.
    match crate::shutdown::recover_orphaned_turns(&*raw_storage, &*raw_storage).await {
        Ok(0) => {}
        Ok(n) => info!("Recovered {n} orphaned turn(s) from a previous shutdown"),
        Err(e) => {
            eprintln!("Startup recovery failed: {e}");
            std::process::exit(1);
        }
    }

    match crate::shutdown::recover_orphaned_background_runs(&*raw_storage).await {
        Ok(counts) => {
            if counts.schedule_runs_failed > 0 {
                info!(
                    "Recovered {} orphaned schedule run(s) from a previous shutdown",
                    counts.schedule_runs_failed
                );
            }
            if counts.background_jobs_retried > 0 {
                info!(
                    "Recovered {} orphaned background job(s) from a previous shutdown",
                    counts.background_jobs_retried
                );
            }
        }
        Err(e) => {
            eprintln!("Background startup recovery failed: {e}");
            std::process::exit(1);
        }
    }

    match raw_storage.interrupt_running_workflow_runs().await {
        Ok(0) => {}
        // Marking is half the recovery; the workflow wake loop spawned at
        // server start relaunches these (and due parked runs) by replay.
        Ok(n) => info!("Marked {n} workflow run(s) interrupted by a previous shutdown; resuming"),
        Err(e) => {
            eprintln!("Workflow startup recovery failed: {e}");
            std::process::exit(1);
        }
    }

    // Step 5c: Apply timeout policy to any approval requests that expired
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

    // Step 6: Create provider clients from governed credential resolution.
    let startup_secret_manager = crate::secret::SecretManager::new(
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>
    );
    let provider = create_provider(&config, &startup_secret_manager).await;
    let voice_provider = create_voice_provider(&config, &startup_secret_manager).await;
    let telegram = create_telegram_client(&config, &startup_secret_manager).await;

    // Step 7: Build the tool dispatch pipeline from the root selected above.
    let browser_host = Some(crate::browser::BrowserHost::new(
        crate::browser::BrowserConfig::from_tool_config(&config.tools.browser, &root),
    ));
    let tool_dispatcher = build_tool_dispatcher_with_browser(
        &config,
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>,
        browser_host.clone(),
        &root,
    );

    // Step 8: Assemble application state and build the router.
    let storage: Arc<dyn swage_storage::store::Storage> =
        Arc::clone(&raw_storage) as Arc<dyn swage_storage::store::Storage>;
    let topic_store: Arc<dyn swage_storage::store::TopicStore> = raw_storage;
    let mut state = crate::state::AppState::new_with_config_root(
        storage,
        topic_store,
        provider,
        config.clone(),
        tool_dispatcher,
        None,
        root,
    );
    state.replace_voice_provider(voice_provider);
    state.telegram = telegram;
    // The Codex CLI's auth store lives under a Swage-owned state directory;
    // registering it as file-backed secrets brings the harness's own tokens
    // under governance — audited resolution and redaction — without moving
    // them. Registration precedes sign-in, so the store need not exist yet.
    if let Err(error) = state
        .secret_manager
        .register_codex_auth_store(&state.codex_supervisor.state_dir())
        .await
    {
        eprintln!("Failed to register the Codex auth store as governed secrets: {error}");
        std::process::exit(1);
    }
    if let Err(error) = recover_worktrees_after_crash(&state).await {
        eprintln!("Worktree startup recovery failed: {error}");
        std::process::exit(1);
    }
    state.start_agent_message_expiry();
    if let Err(error) = crate::device::restore_registered_devices(&state).await {
        eprintln!("Failed to restore devices: {error}");
        std::process::exit(1);
    }
    let router = crate::server::build_router(state.clone());
    let background_shutdown = CancellationToken::new();
    let schedule_handle = crate::schedule::spawn_schedule_loop_until_cancelled(
        state.clone(),
        background_shutdown.clone(),
    );
    // Durable workflow waits: its first pass resumes the runs the previous
    // shutdown interrupted, then it wakes parked runs as they come due.
    let workflow_wake_handle =
        crate::workflow::spawn_workflow_wake_loop(state.clone(), background_shutdown.clone());
    let attachment_handle = crate::attachment::spawn_ingestion_loop_until_cancelled(
        state.clone(),
        background_shutdown.clone(),
    );
    // The inactivity archive: hides sessions nobody has spoken in for the
    // configured delay, first pass at startup.
    let archive_handle = crate::session_archive::spawn_archive_loop_until_cancelled(
        state.clone(),
        background_shutdown.clone(),
    );
    // Messages queued behind turns the restart orphaned run now that those
    // turns are settled.
    match crate::turn_queue::drain_queues_at_startup(&state).await {
        Ok(0) => {}
        Ok(count) => tracing::info!(count, "draining turn queues left by the previous process"),
        Err(error) => tracing::warn!(%error, "turn queues could not be drained at startup"),
    }
    // Memory curation: the platform's curator reads each attended
    // conversation into the operator's memory once it has fallen quiet.
    let curation_handle = crate::memory_curation::spawn_curation_loop_until_cancelled(
        state.clone(),
        background_shutdown.clone(),
    );
    let queue_handle = crate::background_loop::spawn_periodic_loop(
        std::time::Duration::from_millis(250),
        background_shutdown.clone(),
        "channel queue drain failed",
        {
            let state = state.clone();
            move || {
                let state = state.clone();
                async move {
                    crate::channel_gateway::drain_channel_queue(state).await;
                    Ok(())
                }
            }
        },
    );
    let telegram_handle = crate::telegram::spawn_poll_loop_until_cancelled(
        state.clone(),
        background_shutdown.clone(),
    );
    let browser_idle_handle = browser_host.clone().map(|browser_host| {
        crate::background_loop::spawn_periodic_loop(
            std::time::Duration::from_secs(30),
            background_shutdown.clone(),
            "browser idle sweep failed",
            move || {
                let browser_host = Arc::clone(&browser_host);
                async move {
                    browser_host.sweep_idle().await;
                    Ok(())
                }
            },
        )
    });

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

    {
        state.stop_agent_message_expiry();
        background_shutdown.cancel();
        for (name, handle) in [
            ("schedule", schedule_handle),
            ("workflow wake", workflow_wake_handle),
            ("attachment ingestion", attachment_handle),
            ("session archive", archive_handle),
            ("memory curation", curation_handle),
            ("channel queue", queue_handle),
        ] {
            if let Err(error) = handle.await {
                warn!(%error, "background {name} loop did not shut down cleanly");
            }
        }
        if let Some(handle) = telegram_handle
            && let Err(error) = handle.await
        {
            warn!(%error, "background telegram polling loop did not shut down cleanly");
        }
        if let Some(handle) = browser_idle_handle
            && let Err(error) = handle.await
        {
            warn!(%error, "background browser idle loop did not shut down cleanly");
        }
    }

    if let Some(browser_host) = browser_host {
        browser_host.shutdown().await;
    }
    state.worktree_host.shutdown().await;

    // Step 12c: Remove runtime metadata now that the gateway has stopped.
    if let Err(e) = RuntimeMetadata::remove() {
        warn!("Failed to remove runtime metadata on shutdown: {e}");
    }
}

/// Recover crash-stranded worktrees after durable workflow state is available.
///
/// Workflow runs that can replay retain their owner directories. Finalised
/// changed worker paths also remain available for later integration.
async fn recover_worktrees_after_crash(state: &crate::state::AppState) -> Result<usize, String> {
    let claims = state.worktree_recovery_claims().await?;
    state.worktree_host.recover(&claims).await
}

/// Builds the voice provider from the default provider's endpoint and
/// credentials plus the `[voice]` model routes. Every unavailable case carries
/// a reason naming what to configure.
pub(crate) async fn create_voice_provider(
    config: &swage_config::ResolvedConfig,
    secret_manager: &crate::secret::SecretManager,
) -> Arc<dyn swage_provider::VoiceProvider> {
    let Some((provider_endpoint, model_endpoint)) = default_provider_model(config) else {
        return Arc::new(swage_provider::UnsupportedVoiceProvider::new(
            "voice needs an OpenAI-protocol default provider; no default provider/model pair is configured",
        ));
    };
    let provider_config = match resolve_provider_config(
        provider_endpoint,
        model_endpoint,
        secret_manager,
    )
    .await
    {
        Ok(config) => config,
        Err(_) => {
            return Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                "voice is unavailable because the default provider's credentials are unavailable",
            ));
        }
    };

    match provider_endpoint.protocol {
        swage_config::Protocol::OpenAi
        | swage_config::Protocol::OpenAiResponses
        | swage_config::Protocol::OpenRouter => Arc::new(swage_provider::OpenAiVoiceProvider::new(
            provider_config,
            swage_provider::VoiceModelRoutes {
                transcription_model: config.voice.transcription_model.clone(),
                speech_model: config.voice.speech_model.clone(),
            },
        )),
        swage_config::Protocol::Anthropic
        | swage_config::Protocol::Google
        | swage_config::Protocol::Codex => {
            Arc::new(swage_provider::UnsupportedVoiceProvider::new(format!(
                "voice needs an OpenAI-protocol default provider; provider protocol {:?} serves no audio API",
                provider_endpoint.protocol
            )))
        }
    }
}

fn provider_base_url(config: &swage_config::ProviderEndpointConfig) -> &str {
    config.base_url.as_deref().unwrap_or(match config.protocol {
        swage_config::Protocol::OpenAi | swage_config::Protocol::OpenAiResponses => {
            "https://api.openai.com/v1"
        }
        swage_config::Protocol::Anthropic => "https://api.anthropic.com",
        swage_config::Protocol::Google => "https://generativelanguage.googleapis.com",
        swage_config::Protocol::OpenRouter => "https://openrouter.ai/api/v1",
        swage_config::Protocol::Codex => "",
    })
}

async fn resolve_provider_config(
    provider: &swage_config::ProviderEndpointConfig,
    model: &swage_config::ModelEndpointConfig,
    secret_manager: &crate::secret::SecretManager,
) -> Result<swage_provider::ProviderConfig, swage_core::ProviderError> {
    let base_url = provider_base_url(provider).to_string();

    if let Some(reference) = provider.api_key_secret_ref.as_deref() {
        let resolved = secret_manager.resolve(reference).await.map_err(|error| {
            swage_core::ProviderError::Auth {
                provider: format!("failed to resolve provider credential {reference}: {error}"),
            }
        })?;
        if resolved.record.material_kind != "provider_key" {
            return Err(swage_core::ProviderError::Auth {
                provider: format!(
                    "provider credential {reference} has material_kind '{}', expected 'provider_key'",
                    resolved.record.material_kind
                ),
            });
        }
        return Ok(swage_provider::ProviderConfig::new(
            resolved.value,
            &model.model_id,
            base_url,
        ));
    }

    if let Some(api_key_env) = provider.api_key_env.as_deref() {
        tracing::warn!(
            "Provider '{}' is using legacy api_key_env credential resolution. \
             Store the credential in Settings > Providers to create a governed provider_key secret \
             and set api_key_secret_ref.",
            model.provider_id
        );
        let api_key = std::env::var(api_key_env).map_err(|_| swage_core::ProviderError::Auth {
            provider: format!("environment variable '{api_key_env}' is not set"),
        })?;
        return Ok(swage_provider::ProviderConfig::new(
            api_key,
            &model.model_id,
            base_url,
        ));
    }

    Ok(swage_provider::ProviderConfig::keyless(
        &model.model_id,
        base_url,
    ))
}

async fn create_telegram_client(
    config: &swage_config::ResolvedConfig,
    secret_manager: &crate::secret::SecretManager,
) -> Option<Arc<crate::telegram::TelegramClient>> {
    let telegram = config.channels.telegram.as_ref()?;

    let token = match resolve_telegram_token(telegram, secret_manager).await {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(
                "Telegram channel credential resolution failed: {error}. \
                 The server will start, but Telegram polling will stay disabled \
                 until configuration is fixed."
            );
            return None;
        }
    };

    let timeout = std::time::Duration::from_secs(telegram.poll_timeout_secs.saturating_add(10));
    match crate::telegram::TelegramClient::new(crate::telegram::TelegramClientConfig {
        base_url: telegram.api_base_url.clone(),
        token,
        request_timeout: timeout,
    }) {
        Ok(client) => Some(Arc::new(client)),
        Err(error) => {
            tracing::warn!(
                "Telegram channel startup failed: {error}. \
                 The server will start, but Telegram polling will stay disabled \
                 until configuration is fixed."
            );
            None
        }
    }
}

async fn resolve_telegram_token(
    telegram: &swage_config::TelegramChannelConfig,
    secret_manager: &crate::secret::SecretManager,
) -> Result<String, String> {
    if let Some(reference) = telegram.bot_token_secret_ref.as_deref() {
        let resolved = secret_manager.resolve(reference).await.map_err(|error| {
            format!("failed to resolve telegram credential {reference}: {error}")
        })?;
        if resolved.record.material_kind != "channel_token" {
            return Err(format!(
                "telegram credential {reference} has material_kind '{}', expected 'channel_token'",
                resolved.record.material_kind
            ));
        }
        return Ok(resolved.value);
    }

    if let Some(env_var) = telegram.bot_token_env.as_deref() {
        warn!(
            "Telegram channel is using legacy bot_token_env credential resolution. \
             Create a governed channel_token secret through POST /api/secrets and set \
             bot_token_secret_ref."
        );
        return std::env::var(env_var)
            .map_err(|_| format!("environment variable '{env_var}' is not set"));
    }

    Err("telegram channel configured without bot_token_secret_ref or bot_token_env".into())
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
/// can type-erase it into both `Arc<dyn Storage>` and `Arc<dyn TopicStore>`
/// from the same underlying connection pool.
fn open_storage(path: &std::path::Path) -> Arc<swage_storage::SqliteStorage> {
    Arc::new(
        swage_storage::SqliteStorage::open(path).unwrap_or_else(|e| {
            eprintln!("{}", storage_open_error(path, &e));
            std::process::exit(1);
        }),
    )
}

fn storage_open_error(path: &std::path::Path, error: &swage_core::StorageError) -> String {
    let detail = error.to_string();
    if let swage_core::StorageError::UnknownSchema { message } = error {
        return format!(
            "The database at {} was not produced by this build ({message}). Rebuild this data root, or run the build that owns it; data migration is not supported during the private alpha.",
            path.display()
        );
    }
    if detail.contains("no such table: workflow_runs") {
        format!(
            "The data root at {} uses an obsolete private-alpha database. Rebuild this data root before starting Swage; data migration is not supported during the private alpha.",
            path.parent().unwrap_or(path).display()
        )
    } else {
        format!("Failed to open database at {}: {error}", path.display())
    }
}

/// The tools that act on a workspace — shell and files — with the
/// configured limits. The gateway's own dispatcher starts from them, and a
/// device process runs exactly this set over its dialled connection.
pub fn workspace_tools(config: &swage_config::ResolvedConfig) -> Vec<Arc<dyn crate::tool::Tool>> {
    use crate::tool::{
        file_edit::FileEditTool, file_list::FileListTool, file_read::FileReadTool,
        file_search::FileSearchTool, file_write::FileWriteTool, shell::ShellTool,
    };
    use std::time::Duration;

    let max_output = config.tools.max_output_bytes;
    vec![
        Arc::new(ShellTool {
            timeout: Duration::from_secs(config.tools.shell_timeout_secs),
            max_output_bytes: max_output,
        }),
        Arc::new(FileReadTool {
            max_output_bytes: max_output,
        }),
        Arc::new(FileEditTool),
        Arc::new(FileWriteTool),
        Arc::new(FileSearchTool {
            max_output_bytes: max_output,
        }),
        Arc::new(FileListTool),
    ]
}

/// Build the tool dispatch pipeline with all built-in tools from configuration.
pub(crate) fn build_tool_dispatcher(
    config: &swage_config::ResolvedConfig,
    storage: Arc<dyn swage_storage::store::Storage>,
    config_root: &std::path::Path,
) -> crate::tool::ToolDispatcher {
    let browser_host = Some(crate::browser::BrowserHost::new(
        crate::browser::BrowserConfig::from_tool_config(&config.tools.browser, config_root),
    ));
    build_tool_dispatcher_with_browser(config, storage, browser_host, config_root)
}

pub(crate) fn build_tool_dispatcher_with_browser(
    config: &swage_config::ResolvedConfig,
    storage: Arc<dyn swage_storage::store::Storage>,
    browser_host: Option<Arc<crate::browser::BrowserHost>>,
    config_root: &std::path::Path,
) -> crate::tool::ToolDispatcher {
    use crate::tool::{
        Tool, browser::browser_tools, docs_read::DocsReadTool, hold::WorkflowHoldTool,
        knowledge::KnowledgeCiteTool, knowledge::KnowledgeCurateOrientationTool,
        knowledge::KnowledgeCurateTool, knowledge::KnowledgeOrientTool,
        knowledge::KnowledgeRecordTool, knowledge::KnowledgeSearchTool, memory::MemoryReadTool,
        memory::MemoryWriteTool, plugin::discover_plugin_tools, python_execute::PythonExecuteTool,
        run_event::RunEventPostTool, schedule::ScheduleTool, schedule::WakeMeLaterTool,
        session_variable::SessionVariableTool, surface::SurfaceLibraryTool,
        surface::SurfaceReadTool, surface::SurfaceWriteTool, task_definition::TaskDefinitionTool,
        tool_result_read::ToolResultReadTool, url_fetch::UrlFetchTool, web_search::WebSearchTool,
    };

    let max_output = config.tools.max_output_bytes;
    let shell_timeout = std::time::Duration::from_secs(config.tools.shell_timeout_secs);

    let mut base_tools: Vec<Arc<dyn Tool>> = workspace_tools(config);
    base_tools.extend::<Vec<Arc<dyn Tool>>>(vec![
        Arc::new(WebSearchTool {
            provider_config: config.tools.search.clone(),
        }),
        Arc::new(UrlFetchTool::default()),
        Arc::new(ToolResultReadTool {
            max_output_bytes: max_output,
        }),
        // The platform's own documentation, read from the gateway's
        // compiled-in set; host-only, it touches no workspace.
        Arc::new(DocsReadTool {
            max_output_bytes: max_output,
        }),
        Arc::new(swage_tool::delegate::DelegateTool),
        Arc::new(SurfaceReadTool),
        Arc::new(SurfaceWriteTool),
        Arc::new(SurfaceLibraryTool),
        Arc::new(crate::tool::workflow_list::WorkflowListTool),
        Arc::new(crate::tool::workflow_run::WorkflowRunTool),
        Arc::new(crate::tool::workflow_run_account::WorkflowRunAccountTool),
        Arc::new(crate::tool::workflow_run_status::WorkflowRunStatusTool),
        Arc::new(crate::tool::workflow_save::WorkflowSaveTool),
        // The operator's memory follows the calling session's scope on the
        // gateway host: every session may read; the curator alone holds the
        // write tool (`Tool::platform_holder`).
        Arc::new(MemoryReadTool),
        Arc::new(MemoryWriteTool),
        // Schedule tools reach the gateway's schedule service through the host
        // tool context; both are host-only (requires_device_execution).
        Arc::new(ScheduleTool),
        Arc::new(TaskDefinitionTool),
        Arc::new(WakeMeLaterTool),
        Arc::new(RunEventPostTool),
        Arc::new(WorkflowHoldTool),
        Arc::new(SessionVariableTool),
        // Knowledge-base tools: the topic's gathered external evidence.
        // Host-only; read, capture authoring, and curation authoring are
        // separately grantable.
        Arc::new(KnowledgeSearchTool),
        Arc::new(KnowledgeRecordTool),
        Arc::new(KnowledgeCurateTool),
        Arc::new(KnowledgeCiteTool),
        Arc::new(KnowledgeOrientTool),
        Arc::new(KnowledgeCurateOrientationTool),
    ]);

    let base_tools = if let Some(browser_host) = browser_host {
        let mut tools = base_tools;
        tools.extend(browser_tools(browser_host));
        tools
    } else {
        base_tools
    };

    let plugin_tools = resolve_plugin_directory(config, config_root)
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

    if !config.tools.python_execute.enabled {
        warn!(
            "tools.python_execute.enabled is false; python_execute was not registered; enable it unless this was intentional"
        );
    } else {
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
                            "Python worker pool could not be initialised unexpectedly ({error}); python_execute was not registered; report this internal startup error"
                        );
                    }
                }
            }
            Ok(_) => {
                warn!(
                    "bubblewrap is unavailable; python_execute was not registered; install it or set tools.python_execute.require_sandbox = false to run Python unsandboxed"
                );
            }
            Err(e) => {
                warn!(
                    "Python runtime could not be resolved ({e}); python_execute was not registered; install Python 3 or correct tools.python_execute.runtime_path"
                );
            }
        }
    }

    info!(
        "Registered {} tool(s); workspace roots will be resolved from each tool context",
        all_tools.len()
    );

    crate::tool::ToolDispatcher::allow_all(all_tools)
}

fn resolve_plugin_directory(
    config: &swage_config::ResolvedConfig,
    config_root: &std::path::Path,
) -> Option<PathBuf> {
    let directory = config.tools.plugin_directory.as_ref()?;
    let path = PathBuf::from(directory);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(config_root.join(path))
    }
}

fn ensure_managed_secret_key(root: &std::path::Path) -> Result<PathBuf, String> {
    if std::env::var_os(swage_storage::MANAGED_SECRET_KEY_ENV).is_some() {
        return Ok(root.join("secrets").join("managed-secret.key"));
    }

    let key_path = root.join("secrets").join("managed-secret.key");
    let encoded = if key_path.exists() {
        std::fs::read_to_string(&key_path).map_err(|error| {
            format!(
                "failed to read managed secret key at {}: {error}",
                key_path.display()
            )
        })?
    } else {
        let mut key = [0_u8; 32];
        rand::fill(&mut key);
        let encoded = BASE64.encode(key);
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create managed secret key directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        write_managed_secret_key(&key_path, &encoded)?;
        encoded
    };

    let encoded = encoded.trim();
    let bytes = BASE64.decode(encoded).map_err(|error| {
        format!(
            "managed secret key at {} is invalid base64: {error}",
            key_path.display()
        )
    })?;
    if bytes.len() != 32 {
        return Err(format!(
            "managed secret key at {} must decode to exactly 32 bytes",
            key_path.display()
        ));
    }

    // SAFETY: this runs during gateway startup before serving requests. The
    // storage encryption code intentionally reads the key from the process
    // environment, so the local key file is bridged into that existing path.
    unsafe { std::env::set_var(swage_storage::MANAGED_SECRET_KEY_ENV, encoded) };
    Ok(key_path)
}

#[cfg(unix)]
fn write_managed_secret_key(path: &std::path::Path, encoded: &str) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to create managed secret key at {}: {error}",
                path.display()
            )
        })?;
    file.write_all(encoded.as_bytes()).map_err(|error| {
        format!(
            "failed to write managed secret key at {}: {error}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn write_managed_secret_key(path: &std::path::Path, encoded: &str) -> Result<(), String> {
    std::fs::write(path, encoded).map_err(|error| {
        format!(
            "failed to write managed secret key at {}: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
fn swage_database_path() -> Result<PathBuf, swage_config::ConfigProblem> {
    swage_config::discovery::resolve_root().map(|root| root.join("swage.db"))
}

/// Create the LLM provider from resolved configuration.
pub(crate) async fn create_provider(
    config: &swage_config::ResolvedConfig,
    secret_manager: &crate::secret::SecretManager,
) -> Arc<dyn swage_provider::LlmProvider> {
    create_provider_with_retry(
        config,
        secret_manager,
        swage_provider::retry::RetryConfig::default(),
    )
    .await
}

async fn create_provider_with_retry(
    config: &swage_config::ResolvedConfig,
    secret_manager: &crate::secret::SecretManager,
    retry: swage_provider::retry::RetryConfig,
) -> Arc<dyn swage_provider::LlmProvider> {
    use swage_provider::ProviderConfig;

    if default_provider_model(config).is_none() {
        tracing::warn!(
            "No default provider/model pair is configured. \
             Messaging will fail until configuration is fixed."
        );
        return Arc::new(swage_provider::UnsupportedLlmProvider::new(
            "no default provider/model pair is configured",
        ));
    }

    let mut providers = Vec::new();
    for model_id in ordered_runtime_model_ids(config) {
        let Some(model_endpoint) = config.models.entries.get(&model_id) else {
            continue;
        };
        let Some(provider_endpoint) = config.providers.entries.get(&model_endpoint.provider_id)
        else {
            tracing::warn!(
                model_id,
                provider_id = %model_endpoint.provider_id,
                "Skipping model because its provider entry is missing"
            );
            continue;
        };

        // Codex is thread/turn-shaped and runs below the provider-parity
        // selection surface. It must never enter the completion failover chain.
        if provider_endpoint.protocol == swage_config::Protocol::Codex {
            continue;
        }

        let provider_config = match resolve_provider_config(
            provider_endpoint,
            model_endpoint,
            secret_manager,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    model_id,
                    provider_id = %model_endpoint.provider_id,
                    "Provider not fully configured: {e}. \
                     The server will start, but this model will fail \
                     until its API key is set."
                );
                ProviderConfig::keyless(
                    &model_endpoint.model_id,
                    provider_base_url(provider_endpoint),
                )
            }
        };
        providers.push(concrete_provider(
            provider_endpoint.protocol.clone(),
            provider_config,
        ));
    }

    if providers.is_empty() {
        return Arc::new(swage_provider::UnsupportedLlmProvider::new(
            CODEX_COMPLETION_UNSUPPORTED_REASON,
        ));
    }

    build_provider_chain(providers, retry)
}

fn ordered_runtime_model_ids(config: &swage_config::ResolvedConfig) -> Vec<String> {
    let mut ordered = Vec::new();
    if !config.models.default.is_empty() {
        ordered.push(config.models.default.clone());
    }

    let default_seen = ordered.first().cloned();
    let remaining: BTreeSet<_> = config
        .models
        .entries
        .keys()
        .filter(|id| Some(id.as_str()) != default_seen.as_deref())
        .cloned()
        .collect();
    ordered.extend(remaining);
    ordered
}

fn concrete_provider(
    protocol: swage_config::Protocol,
    provider_config: swage_provider::ProviderConfig,
) -> Box<dyn swage_provider::LlmProvider> {
    match protocol {
        swage_config::Protocol::Anthropic => Box::new(
            swage_provider::anthropic::AnthropicProvider::from_config(provider_config),
        ),
        swage_config::Protocol::OpenAi | swage_config::Protocol::OpenRouter => Box::new(
            swage_provider::openai::OpenAiProvider::from_config(provider_config),
        ),
        swage_config::Protocol::OpenAiResponses => Box::new(
            swage_provider::responses::ResponsesProvider::from_config(provider_config),
        ),
        swage_config::Protocol::Google => Box::new(
            swage_provider::google::GoogleProvider::from_config(provider_config),
        ),
        swage_config::Protocol::Codex => Box::new(swage_provider::UnsupportedLlmProvider::new(
            CODEX_COMPLETION_UNSUPPORTED_REASON,
        )),
    }
}

fn build_provider_chain(
    providers: Vec<Box<dyn swage_provider::LlmProvider>>,
    retry: swage_provider::retry::RetryConfig,
) -> Arc<dyn swage_provider::LlmProvider> {
    use swage_provider::builder::ProviderChainBuilder;
    use swage_provider::failover::FailoverConfig;

    let mut providers = providers.into_iter();
    let first = providers
        .next()
        .expect("provider chain is non-empty after default provider validation");

    let mut chain = ProviderChainBuilder::new()
        .provider_boxed(first)
        .with_retry(retry.clone());
    let mut count = 1usize;
    for provider in providers {
        chain = chain.provider_boxed(provider).with_retry(retry.clone());
        count += 1;
    }

    let provider = if count > 1 {
        chain.with_failover(FailoverConfig::default()).build()
    } else {
        chain.build()
    };
    Arc::from(provider)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::test_support::SwageHomeGuard;

    use super::*;
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use futures::StreamExt;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use swage_config::{
        LogLevel, ModelCatalogConfig, ModelEndpointConfig, Protocol, ProviderCatalogConfig,
        ProviderEndpointConfig, TelegramChannelConfig,
    };
    use swage_storage::{
        SqliteStorage,
        model::{NewWorkflowJournalIntent, NewWorkflowRun},
        store::{AuditStore, Storage, TopicStore, WorkflowStore},
    };
    use tokio::net::TcpListener;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    use swage_agent::fixtures::MessageRecorder;

    #[tokio::test(flavor = "multi_thread")]
    async fn startup_recovery_retains_replayable_and_finalised_changed_worktrees() {
        let fixture = tempfile::tempdir().expect("fixture");
        let repo = fixture.path().join("repo");
        std::fs::create_dir(&repo).expect("repository directory");
        for args in [
            &["init"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "Test"][..],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .expect("run git")
                    .success()
            );
        }
        std::fs::write(repo.join("seed"), "seed").expect("seed repository");
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["add", "seed"])
                .status()
                .expect("stage seed")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["commit", "-m", "seed"])
                .status()
                .expect("commit seed")
                .success()
        );

        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let new_run = |source_hash: &str| NewWorkflowRun {
            source_hash: source_hash.into(),
            ..swage_storage::fixtures::new_workflow_run(topic.id)
        };
        let run = storage
            .create_workflow_run(new_run("startup-recovery-interrupted"))
            .await
            .expect("workflow run");
        storage
            .update_workflow_run_state(run.id, "interrupted", None, None, None)
            .await
            .expect("interrupt run");
        let parked = storage
            .create_workflow_run(new_run("startup-recovery-parked"))
            .await
            .expect("parked workflow run");
        storage
            .set_workflow_run_parked(parked.id, chrono::Utc::now() + chrono::Duration::hours(1))
            .await
            .expect("park run");
        let terminal = storage
            .create_workflow_run(new_run("startup-recovery-terminal"))
            .await
            .expect("terminal workflow run");
        storage
            .update_workflow_run_state(terminal.id, "failed", None, None, Some("test failure"))
            .await
            .expect("fail run");
        let failed_with_result = storage
            .create_workflow_run(new_run("startup-recovery-failed-result"))
            .await
            .expect("failed workflow run with result");
        storage
            .update_workflow_run_state(
                failed_with_result.id,
                "failed",
                None,
                None,
                Some("failure after worker completion"),
            )
            .await
            .expect("fail run after worker completion");
        let completed_with_result = storage
            .create_workflow_run(new_run("startup-recovery-completed-result"))
            .await
            .expect("completed workflow run with result");
        storage
            .update_workflow_run_state(
                completed_with_result.id,
                "completed",
                Some(Value::Null),
                None,
                None,
            )
            .await
            .expect("complete run with worker result");
        let state = crate::state::AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
        .with_worktree_root(fixture.path().join("worker-worktrees"));
        let worktree = state
            .worktree_host
            .provision(run.id, 0, &repo)
            .await
            .expect("provision worktree");
        let parked_worktree = state
            .worktree_host
            .provision(parked.id, 0, &repo)
            .await
            .expect("provision parked worktree");
        let terminal_worktree = state
            .worktree_host
            .provision(terminal.id, 0, &repo)
            .await
            .expect("provision terminal worktree");
        let failed_result_worktree = state
            .worktree_host
            .provision(failed_with_result.id, 0, &repo)
            .await
            .expect("provision failed-run result worktree");
        let completed_result_worktree = state
            .worktree_host
            .provision(completed_with_result.id, 0, &repo)
            .await
            .expect("provision completed-run result worktree");
        std::fs::write(failed_result_worktree.path.join("failed-change"), "changed")
            .expect("change failed-run worktree");
        std::fs::write(
            completed_result_worktree.path.join("completed-change"),
            "changed",
        )
        .expect("change completed-run worktree");
        let failed_completion = state
            .worktree_host
            .finish(failed_with_result.id, 0)
            .await
            .expect("finalise failed-run worktree");
        let completed_completion = state
            .worktree_host
            .finish(completed_with_result.id, 0)
            .await
            .expect("finalise completed-run worktree");
        for (run_id, path, completion) in [
            (
                failed_with_result.id,
                &failed_result_worktree.path,
                failed_completion,
            ),
            (
                completed_with_result.id,
                &completed_result_worktree.path,
                completed_completion,
            ),
        ] {
            let entry = storage
                .create_workflow_journal_intent(NewWorkflowJournalIntent {
                    run_id,
                    seq: 0,
                    kind: "agent_call".into(),
                    args_hash: "test".into(),
                    request: json!({"prompt": "completed worker"}),
                    effect_kind: None,
                    idempotency_key: None,
                    target_kind: None,
                })
                .await
                .expect("journal completed worker");
            storage
                .complete_workflow_journal_entry(
                    entry.id,
                    json!({
                        "working_directory": path,
                        "worktree": {
                            "base_commit": completion.base_commit,
                            "tip_commit": completion.tip_commit,
                            "changed": completion.changed,
                            "removed": completion.removed,
                        }
                    }),
                )
                .await
                .expect("persist completed worker result");
        }

        assert_eq!(
            recover_worktrees_after_crash(&state)
                .await
                .expect("startup recovery"),
            1,
            "startup must reclaim only the unclaimed terminal worktree"
        );
        assert!(
            worktree.path.exists(),
            "replayable worktree must survive startup"
        );
        assert!(
            parked_worktree.path.exists(),
            "parked replayable worktree must survive startup"
        );
        assert!(
            !terminal_worktree.path.exists(),
            "an unfinished terminal worktree must be reclaimed at startup"
        );
        assert!(
            failed_result_worktree.path.exists(),
            "a changed worker must survive when its workflow later fails"
        );
        assert!(
            completed_result_worktree.path.exists(),
            "a completed run's changed worker must survive for integration"
        );
        assert!(
            !terminal_worktree
                .path
                .parent()
                .expect("terminal owner directory")
                .exists(),
            "terminal owner directory must be reclaimed at startup"
        );
        let registrations = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("list worktrees");
        assert!(registrations.status.success());
        assert!(
            String::from_utf8_lossy(&registrations.stdout)
                .contains(&format!("worktree {}", worktree.path.display())),
            "Git registration must survive startup recovery"
        );
        assert!(
            String::from_utf8_lossy(&registrations.stdout)
                .contains(&format!("worktree {}", parked_worktree.path.display())),
            "parked Git registration must survive startup recovery"
        );
        assert!(
            !String::from_utf8_lossy(&registrations.stdout)
                .contains(&format!("worktree {}", terminal_worktree.path.display())),
            "terminal Git registration must be reclaimed at startup"
        );
        assert!(
            String::from_utf8_lossy(&registrations.stdout).contains(&format!(
                "worktree {}",
                failed_result_worktree.path.display()
            )),
            "failed-run completion registration must survive startup recovery"
        );
        assert!(
            String::from_utf8_lossy(&registrations.stdout).contains(&format!(
                "worktree {}",
                completed_result_worktree.path.display()
            )),
            "completed-run registration must survive startup recovery"
        );

        state.worktree_host.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn store_with_a_foreign_migration_history_reports_rebuild_instruction() {
        let directory = tempfile::tempdir().expect("obsolete data root");
        let path = directory.path().join("swage.db");
        let storage = swage_storage::SqliteStorage::open(&path).expect("seed current database");
        storage
            .with_conn(|connection| {
                connection.execute_batch(
                    "DROP TABLE workflow_runs;
                     DELETE FROM _swage_migrations
                     WHERE name = '046_workflow_run_parentage';",
                )
            })
            .await
            .expect("strip workflow schema from database");
        drop(storage);

        let error = swage_storage::SqliteStorage::open(&path)
            .err()
            .expect("a store with a gap in its recorded migrations is refused at open");
        assert!(matches!(
            error,
            swage_core::StorageError::UnknownSchema { .. }
        ));

        let message = storage_open_error(&path, &error);

        assert!(message.contains(&path.display().to_string()));
        assert!(message.contains("not produced by this build"));
        assert!(message.contains("046_workflow_run_parentage"));
        assert!(message.contains("Rebuild this data root"));
        assert!(message.contains("data migration is not supported"));
    }

    #[test]
    fn swage_database_path_uses_swage_root() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let root = temp_dir.path().join("app-data");
        let _swage_home = SwageHomeGuard::set_to(&root);

        let path = swage_database_path().unwrap();

        assert_eq!(path, root.join("swage.db"));
        assert!(
            root.is_dir(),
            "database path resolution should auto-create root"
        );
    }

    #[test]
    fn ensure_managed_secret_key_creates_local_key_when_env_is_missing() {
        let _swage_home = SwageHomeGuard::new();
        let previous_key = std::env::var_os(swage_storage::MANAGED_SECRET_KEY_ENV);
        unsafe { std::env::remove_var(swage_storage::MANAGED_SECRET_KEY_ENV) };

        let result = ensure_managed_secret_key(_swage_home.path());

        let key_path = result.expect("managed secret key should be provisioned");
        let encoded = std::fs::read_to_string(&key_path).expect("key file should exist");
        let decoded = BASE64
            .decode(encoded.trim())
            .expect("key should be base64 encoded");
        assert_eq!(decoded.len(), 32);
        assert_eq!(
            std::env::var(swage_storage::MANAGED_SECRET_KEY_ENV).unwrap(),
            encoded.trim()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        match previous_key {
            Some(value) => unsafe {
                std::env::set_var(swage_storage::MANAGED_SECRET_KEY_ENV, value)
            },
            None => unsafe { std::env::remove_var(swage_storage::MANAGED_SECRET_KEY_ENV) },
        }
    }

    #[test]
    fn ensure_managed_secret_key_uses_existing_env_key() {
        let _swage_home = SwageHomeGuard::new();
        let previous_key = std::env::var_os(swage_storage::MANAGED_SECRET_KEY_ENV);
        let explicit_key = BASE64.encode([23_u8; 32]);
        unsafe { std::env::set_var(swage_storage::MANAGED_SECRET_KEY_ENV, &explicit_key) };

        let result = ensure_managed_secret_key(_swage_home.path());

        let key_path = result.expect("explicit managed secret key should be accepted");
        assert!(!key_path.exists());
        assert_eq!(
            std::env::var(swage_storage::MANAGED_SECRET_KEY_ENV).unwrap(),
            explicit_key
        );

        match previous_key {
            Some(value) => unsafe {
                std::env::set_var(swage_storage::MANAGED_SECRET_KEY_ENV, value)
            },
            None => unsafe { std::env::remove_var(swage_storage::MANAGED_SECRET_KEY_ENV) },
        }
    }

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

    #[test]
    fn ordered_runtime_model_ids_keeps_default_first_then_sorts_the_rest() {
        let mut config = swage_config::ResolvedConfig {
            models: ModelCatalogConfig {
                default: "primary".to_string(),
                entries: [
                    ("zeta".to_string(), model("provider", "zeta-model")),
                    ("primary".to_string(), model("provider", "primary-model")),
                    ("alpha".to_string(), model("provider", "alpha-model")),
                ]
                .into_iter()
                .collect(),
            },
            ..Default::default()
        };

        assert_eq!(
            ordered_runtime_model_ids(&config),
            vec!["primary", "alpha", "zeta"]
        );

        config.models.default = "missing".to_string();
        assert_eq!(
            ordered_runtime_model_ids(&config),
            vec!["missing", "alpha", "primary", "zeta"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_provider_uses_configured_models_as_failover_candidates() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let secondary_calls = Arc::new(AtomicUsize::new(0));
        let primary_base_url = spawn_openai_compatible_server(
            Arc::clone(&primary_calls),
            StatusCode::INTERNAL_SERVER_ERROR,
            "primary unavailable",
        )
        .await;
        let secondary_base_url = spawn_openai_compatible_server(
            Arc::clone(&secondary_calls),
            StatusCode::OK,
            "secondary response",
        )
        .await;
        let config = swage_config::ResolvedConfig {
            providers: ProviderCatalogConfig {
                default: "primary".to_string(),
                entries: [
                    (
                        "primary".to_string(),
                        ProviderEndpointConfig {
                            worker_staleness_threshold_ms: None,
                            protocol: Protocol::OpenAi,
                            api_key_secret_ref: None,
                            api_key_env: None,
                            base_url: Some(primary_base_url),
                            command: None,
                            state_dir: None,
                        },
                    ),
                    (
                        "secondary".to_string(),
                        ProviderEndpointConfig {
                            worker_staleness_threshold_ms: None,
                            protocol: Protocol::OpenAi,
                            api_key_secret_ref: None,
                            api_key_env: None,
                            base_url: Some(secondary_base_url),
                            command: None,
                            state_dir: None,
                        },
                    ),
                ]
                .into_iter()
                .collect(),
            },
            models: ModelCatalogConfig {
                default: "primary-model".to_string(),
                entries: [
                    (
                        "primary-model".to_string(),
                        model("primary", "primary-wire"),
                    ),
                    (
                        "secondary-model".to_string(),
                        model("secondary", "secondary-wire"),
                    ),
                ]
                .into_iter()
                .collect(),
            },
            ..Default::default()
        };
        let retry = swage_provider::retry::RetryConfig {
            max_retries: 0,
            initial_backoff: std::time::Duration::ZERO,
            max_backoff: std::time::Duration::ZERO,
        };

        let provider = create_provider_with_retry(&config, &secret_manager, retry).await;
        let response = provider
            .complete(swage_core::CompletionRequest {
                effort: None,
                messages: vec![],
                model: None,
                max_tokens: None,
                temperature: None,
                stop_sequences: None,
                metadata: None,
                tool_definitions: vec![],
            })
            .await
            .expect("secondary provider should satisfy the request");

        assert_eq!(provider.model_id(), "primary-wire");
        assert_eq!(response.content, "secondary response");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(secondary_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn codex_only_configuration_does_not_emit_no_provider_warning() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        let config = swage_config::ResolvedConfig {
            providers: ProviderCatalogConfig {
                default: "codex".to_string(),
                entries: [(
                    "codex".to_string(),
                    ProviderEndpointConfig {
                        worker_staleness_threshold_ms: None,
                        protocol: Protocol::Codex,
                        api_key_secret_ref: None,
                        api_key_env: None,
                        base_url: None,
                        command: None,
                        state_dir: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
            models: ModelCatalogConfig {
                default: "codex".to_string(),
                entries: [("codex".to_string(), model("codex", "gpt-5.6-codex"))]
                    .into_iter()
                    .collect(),
            },
            ..Default::default()
        };
        let events = MessageRecorder::default();
        let captured = Arc::clone(&events.0);
        let subscriber = Registry::default().with(events);

        let (provider_model, completion_error) =
            tracing::subscriber::with_default(subscriber, || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let provider = create_provider(&config, &secret_manager).await;
                        let provider_model = provider.model_id().to_string();
                        let completion_error = provider
                            .complete(swage_core::CompletionRequest {
                                effort: None,
                                messages: vec![],
                                model: None,
                                max_tokens: None,
                                temperature: None,
                                stop_sequences: None,
                                metadata: None,
                                tool_definitions: vec![],
                            })
                            .await
                            .expect_err("Codex-only configuration must fail closed on completion");
                        (provider_model, completion_error)
                    })
            });

        let events = captured.lock().unwrap();
        assert!(
            !events.iter().any(|event| event
                .message
                .contains("No usable provider/model pairs are configured")),
            "Codex-only configuration emitted a false no-provider warning: {events:?}"
        );
        assert_eq!(provider_model, "unavailable");
        assert!(matches!(
            completion_error,
            swage_core::ProviderError::InvalidConfig { message }
                if message == CODEX_COMPLETION_UNSUPPORTED_REASON
        ));
    }

    #[test]
    fn unusable_configuration_retains_provider_configuration_warning() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        let config = swage_config::ResolvedConfig::default();
        let events = MessageRecorder::default();
        let captured = Arc::clone(&events.0);
        let subscriber = Registry::default().with(events);

        tracing::subscriber::with_default(subscriber, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(create_provider(&config, &secret_manager));
        });

        let events = captured.lock().unwrap();
        assert!(
            events.iter().any(|event| event
                .message
                .contains("No default provider/model pair is configured")),
            "genuinely unusable configuration omitted the warning: {events:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_environment_credentials_warn_with_governed_remedies() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        unsafe {
            std::env::set_var("SWAGE_LEGACY_PROVIDER_KEY_TEST", "provider-key");
            std::env::set_var("SWAGE_LEGACY_TELEGRAM_TOKEN_TEST", "telegram-token");
        }
        let events = MessageRecorder::default();
        let captured = Arc::clone(&events.0);
        let _subscriber = tracing::subscriber::set_default(Registry::default().with(events));
        let provider = ProviderEndpointConfig {
            worker_staleness_threshold_ms: None,
            protocol: Protocol::OpenAi,
            api_key_secret_ref: None,
            api_key_env: Some("SWAGE_LEGACY_PROVIDER_KEY_TEST".into()),
            base_url: Some("https://provider.invalid".into()),
            command: None,
            state_dir: None,
        };
        let model = model("provider", "test-model");
        let telegram = TelegramChannelConfig {
            bot_token_env: Some("SWAGE_LEGACY_TELEGRAM_TOKEN_TEST".into()),
            ..Default::default()
        };

        let provider_config = resolve_provider_config(&provider, &model, &secret_manager)
            .await
            .expect("legacy provider environment credential should still resolve");
        let telegram_token = resolve_telegram_token(&telegram, &secret_manager)
            .await
            .expect("legacy Telegram environment credential should still resolve");

        assert_eq!(provider_config.api_key.as_deref(), Some("provider-key"));
        assert_eq!(telegram_token, "telegram-token");
        let events = captured.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.message.contains("Settings > Providers")),
            "provider legacy credential warning must direct operators to Settings: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| event.message.contains("POST /api/secrets")),
            "Telegram legacy credential warning must direct operators to its governed route: {events:?}"
        );
    }

    #[test]
    fn create_provider_candidates_preserve_default_provider_as_primary() {
        let config = swage_config::ResolvedConfig {
            providers: ProviderCatalogConfig {
                default: "anthropic".to_string(),
                entries: [
                    (
                        "openai".to_string(),
                        ProviderEndpointConfig {
                            worker_staleness_threshold_ms: None,
                            protocol: Protocol::OpenAi,
                            api_key_secret_ref: None,
                            api_key_env: None,
                            base_url: Some("http://localhost/openai".to_string()),
                            command: None,
                            state_dir: None,
                        },
                    ),
                    (
                        "anthropic".to_string(),
                        ProviderEndpointConfig {
                            worker_staleness_threshold_ms: None,
                            protocol: Protocol::Anthropic,
                            api_key_secret_ref: None,
                            api_key_env: None,
                            base_url: Some("http://localhost/anthropic".to_string()),
                            command: None,
                            state_dir: None,
                        },
                    ),
                ]
                .into_iter()
                .collect(),
            },
            models: ModelCatalogConfig {
                default: "claude".to_string(),
                entries: [
                    ("gpt".to_string(), model("openai", "gpt-test")),
                    ("claude".to_string(), model("anthropic", "claude-test")),
                ]
                .into_iter()
                .collect(),
            },
            ..Default::default()
        };

        let ids = ordered_runtime_model_ids(&config);

        assert_eq!(ids, vec!["claude", "gpt"]);
    }

    fn model(provider_id: &str, model_id: &str) -> ModelEndpointConfig {
        ModelEndpointConfig {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            aliases: vec![],
            context_window: None,
            generation_preset: None,
            default_effort: None,
            capabilities: swage_config::ModelCapabilitiesConfig::default(),
        }
    }

    fn endpoint(protocol: Protocol, base_url: String) -> ProviderEndpointConfig {
        ProviderEndpointConfig {
            worker_staleness_threshold_ms: None,
            protocol,
            api_key_secret_ref: None,
            api_key_env: None,
            base_url: Some(base_url),
            command: None,
            state_dir: None,
        }
    }

    async fn unreachable_base_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind probe");
        let addr = listener.local_addr().expect("probe address");
        drop(listener);
        format!("http://{addr}/v1")
    }

    fn selected_model_request(model: &str) -> swage_core::CompletionRequest {
        swage_core::CompletionRequest {
            effort: None,
            messages: vec![swage_core::ChatMessage::new(swage_core::Role::User, "hi")],
            model: Some(model.to_string()),
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        }
    }

    fn no_retry() -> swage_provider::retry::RetryConfig {
        swage_provider::retry::RetryConfig {
            max_retries: 0,
            initial_backoff: std::time::Duration::ZERO,
            max_backoff: std::time::Duration::ZERO,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn selected_model_turn_fails_at_its_unreachable_provider_without_other_providers() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        let anthropic_calls = Arc::new(AtomicUsize::new(0));
        let fixture_calls = Arc::new(AtomicUsize::new(0));
        let anthropic_base_url = spawn_openai_compatible_server(
            Arc::clone(&anthropic_calls),
            StatusCode::OK,
            "anthropic-route reply",
        )
        .await;
        let fixture_base_url = spawn_openai_compatible_server(
            Arc::clone(&fixture_calls),
            StatusCode::OK,
            "fixture reply",
        )
        .await;
        let unreachable = unreachable_base_url().await;
        let config = swage_config::ResolvedConfig {
            providers: ProviderCatalogConfig {
                default: "proxy-openai".to_string(),
                entries: [
                    (
                        "proxy-openai".to_string(),
                        endpoint(Protocol::OpenAi, unreachable.clone()),
                    ),
                    (
                        "proxy-anthropic".to_string(),
                        endpoint(Protocol::OpenAi, anthropic_base_url),
                    ),
                    (
                        "fixture".to_string(),
                        endpoint(Protocol::OpenAi, fixture_base_url),
                    ),
                ]
                .into_iter()
                .collect(),
            },
            models: ModelCatalogConfig {
                default: "luna".to_string(),
                entries: [
                    ("luna".to_string(), model("proxy-openai", "gpt-6-luna")),
                    (
                        "sonnet".to_string(),
                        model("proxy-anthropic", "claude-sonnet-5-5"),
                    ),
                    (
                        "fake-echo".to_string(),
                        model("fixture", "fake-fixture-model"),
                    ),
                ]
                .into_iter()
                .collect(),
            },
            ..Default::default()
        };

        let provider = create_provider_with_retry(&config, &secret_manager, no_retry()).await;

        let completion = provider
            .complete(selected_model_request("gpt-6-luna"))
            .await;
        let error = completion.expect_err("an unreachable selected provider must fail the turn");
        let port = unreachable
            .rsplit(':')
            .next()
            .expect("port")
            .trim_end_matches("/v1");
        assert!(
            error.to_string().contains(port),
            "error should name the unreachable endpoint: {error}"
        );

        let chunks: Vec<_> = provider
            .stream(selected_model_request("gpt-6-luna"))
            .collect()
            .await;
        assert!(
            chunks
                .iter()
                .all(|chunk| !matches!(chunk, swage_core::StreamChunk::Delta { .. })),
            "no reply may arrive from another provider: {chunks:?}"
        );
        assert!(
            chunks
                .iter()
                .any(|chunk| matches!(chunk, swage_core::StreamChunk::Error { .. })),
            "the streamed turn must end in an error: {chunks:?}"
        );

        assert_eq!(anthropic_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture_calls.load(Ordering::SeqCst), 0);

        let fixture_reply = provider
            .complete(selected_model_request("fake-fixture-model"))
            .await
            .expect("a model bound to the fixture reaches the fixture");
        assert_eq!(fixture_reply.content, "fixture reply");
        assert_eq!(fixture_calls.load(Ordering::SeqCst), 1);
        assert_eq!(anthropic_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn openai_responses_provider_turn_records_responses_protocol_after_a_failure() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        let chat_calls = Arc::new(AtomicUsize::new(0));
        let chat_base_url = spawn_openai_compatible_server(
            Arc::clone(&chat_calls),
            StatusCode::OK,
            "chat completions reply",
        )
        .await;
        let config = swage_config::ResolvedConfig {
            providers: ProviderCatalogConfig {
                default: "proxy-responses".to_string(),
                entries: [
                    (
                        "proxy-responses".to_string(),
                        endpoint(Protocol::OpenAiResponses, unreachable_base_url().await),
                    ),
                    (
                        "proxy-openai".to_string(),
                        endpoint(Protocol::OpenAi, chat_base_url),
                    ),
                ]
                .into_iter()
                .collect(),
            },
            models: ModelCatalogConfig {
                default: "sol".to_string(),
                entries: [
                    ("sol".to_string(), model("proxy-responses", "gpt-6.1-sol")),
                    ("luna".to_string(), model("proxy-openai", "gpt-6-luna")),
                ]
                .into_iter()
                .collect(),
            },
            ..Default::default()
        };

        let provider = create_provider_with_retry(&config, &secret_manager, no_retry()).await;
        let request = selected_model_request("gpt-6.1-sol");
        let _failed = provider.complete(request.clone()).await;

        let snapshot = provider.provider_request_snapshot(&request, true);
        assert_eq!(snapshot["protocol"], "responses", "snapshot: {snapshot}");
        assert_eq!(chat_calls.load(Ordering::SeqCst), 0);
    }

    async fn spawn_openai_compatible_server(
        calls: Arc<AtomicUsize>,
        status: StatusCode,
        content: &'static str,
    ) -> String {
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(_body): Json<Value>| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if status.is_success() {
                        (
                            status,
                            Json(json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 0,
                                "model": "mock-model",
                                "choices": [{
                                    "index": 0,
                                    "message": {
                                        "role": "assistant",
                                        "content": content,
                                    },
                                    "finish_reason": "stop",
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 2,
                                    "total_tokens": 3,
                                },
                            })),
                        )
                            .into_response()
                    } else {
                        (status, content).into_response()
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock provider");
        let addr = listener.local_addr().expect("mock provider addr");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock provider");
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn build_tool_dispatcher_registers_delegate_tool() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let dispatcher = build_tool_dispatcher(
            &swage_config::ResolvedConfig::default(),
            storage as Arc<dyn swage_storage::store::Storage>,
            std::path::Path::new(".swage"),
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
    async fn build_tool_dispatcher_registers_workflow_list_tool() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let dispatcher = build_tool_dispatcher(
            &swage_config::ResolvedConfig::default(),
            storage as Arc<dyn swage_storage::store::Storage>,
            std::path::Path::new(".swage"),
        );

        assert!(
            dispatcher
                .registry()
                .get(crate::tool::workflow_list::WORKFLOW_LIST_TOOL)
                .is_some()
        );
    }

    #[tokio::test]
    async fn build_tool_dispatcher_omits_disabled_python_with_missing_runtime() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let mut config = swage_config::ResolvedConfig::default();
        config.tools.python_execute.enabled = false;
        config.tools.python_execute.runtime_path = Some("/definitely/missing/python".to_string());

        let dispatcher = build_tool_dispatcher(
            &config,
            storage as Arc<dyn swage_storage::store::Storage>,
            std::path::Path::new(".swage"),
        );

        assert!(dispatcher.registry().get("python_execute").is_none());
    }

    #[tokio::test]
    async fn build_tool_dispatcher_registers_citation_tool() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let dispatcher = build_tool_dispatcher(
            &swage_config::ResolvedConfig::default(),
            storage as Arc<dyn swage_storage::store::Storage>,
            std::path::Path::new(".swage"),
        );

        assert!(
            dispatcher.registry().get("knowledge_cite").is_some(),
            "citation tool should be registered with the built-in dispatcher",
        );
    }

    #[tokio::test]
    async fn build_tool_dispatcher_registers_orientation_tool() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let dispatcher = build_tool_dispatcher(
            &swage_config::ResolvedConfig::default(),
            storage as Arc<dyn swage_storage::store::Storage>,
            std::path::Path::new(".swage"),
        );

        assert!(
            dispatcher.registry().get("knowledge_orient").is_some(),
            "orientation tool should be registered with the built-in dispatcher",
        );
        assert!(
            dispatcher
                .registry()
                .get("knowledge_curate_orientation")
                .is_some(),
            "orientation curation tool should be registered with the built-in dispatcher",
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
        config.tools.plugin_directory = Some("plugins".to_string());

        let dispatcher = build_tool_dispatcher(
            &config,
            storage as Arc<dyn swage_storage::store::Storage>,
            temp_dir.path(),
        );

        assert!(dispatcher.registry().get("echo_plugin").is_some());
    }

    #[tokio::test]
    async fn create_provider_fails_closed_when_default_model_provider_is_missing() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        let mut config = swage_config::ResolvedConfig::default();
        // Wire up a default model that points at a provider id absent from
        // the catalogue — exactly the misconfiguration this test exercises.
        config.models.default = "default".to_string();
        config.models.entries.insert(
            "default".to_string(),
            swage_config::ModelEndpointConfig {
                provider_id: "missing-provider".to_string(),
                model_id: "any-model".to_string(),
                aliases: Vec::new(),
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: swage_config::ModelCapabilitiesConfig::default(),
            },
        );

        let provider = create_provider(&config, &secret_manager).await;
        let result = provider
            .complete(swage_core::CompletionRequest {
                effort: None,
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

    #[tokio::test(flavor = "multi_thread")]
    async fn provider_config_resolves_api_key_from_governed_secret_reference() {
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let secret_manager = crate::secret::SecretManager::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>
        );
        unsafe {
            std::env::set_var("SWAGE_PROVIDER_SECRET_REF_TEST", "resolved-provider-key");
        }

        let secret = secret_manager
            .create_secret(crate::secret::CreateSecretRequest {
                name: "Anthropic provider key".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: "env".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                metadata: None,
                value: None,
                external_reference: Some("SWAGE_PROVIDER_SECRET_REF_TEST".into()),
            })
            .await
            .expect("secret should be created");

        let provider = swage_config::ProviderEndpointConfig {
            worker_staleness_threshold_ms: None,
            protocol: swage_config::Protocol::Anthropic,
            api_key_secret_ref: Some(secret.reference),
            api_key_env: Some("SHOULD_NOT_BE_READ".into()),
            base_url: Some("https://provider.invalid".into()),
            command: None,
            state_dir: None,
        };
        let model = swage_config::ModelEndpointConfig {
            provider_id: "anthropic".into(),
            model_id: "claude-test".into(),
            aliases: Vec::new(),
            context_window: None,
            generation_preset: None,
            default_effort: None,
            capabilities: swage_config::ModelCapabilitiesConfig::default(),
        };

        let config = resolve_provider_config(&provider, &model, &secret_manager)
            .await
            .expect("provider config should resolve from governed secret");

        assert_eq!(config.api_key.as_deref(), Some("resolved-provider-key"));
        assert_eq!(config.model, "claude-test");
        assert_eq!(config.base_url, "https://provider.invalid");

        let audit_events = storage.list_audit_events().await.expect("audit events");
        assert!(
            audit_events
                .iter()
                .any(|event| event.event_kind == "accessed"),
            "secret access should be audited"
        );
    }
}

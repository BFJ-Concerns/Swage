//! CLI subcommand definitions and dispatch for the combined `swage` binary.
//!
//! Uses clap v4 derive macros to define a grouped subcommand architecture.
//! The `gateway` group contains lifecycle management; the `completions` command
//! generates shell autocompletion scripts. The structure accommodates future
//! groups (e.g. `swage project ...`, `swage config ...`).

pub mod completions;
pub mod install;
pub mod restart;
pub mod run;
pub mod start;
pub mod status;
pub mod stop;
pub mod tui;

use clap::Parser;

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Top-level CLI entry point for the combined `swage` binary.
///
/// When no subcommand is given (`command` is `None`), callers should print help
/// and exit. The hidden `--server-mode` flag is set by the `gateway start`
/// handler when it re-spawns the binary as a background daemon — it is not
/// intended for direct user invocation.
#[derive(Debug, Parser)]
#[command(name = "swage", version, about = "Swage AI assistant platform")]
pub struct Cli {
    /// The top-level subcommand to run. None causes callers to show help.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Internal flag: run as gateway server rather than CLI dispatch.
    ///
    /// Set by `swage gateway start` when it re-executes the current binary as
    /// a background daemon. Hidden from help to keep the user-facing interface
    /// clean.
    #[arg(long, hide = true)]
    pub server_mode: bool,

    /// Internal flag: run as the sandbox child-process tool host.
    ///
    /// Set only by sandbox process managers when they re-exec the current
    /// binary for IPC-backed tool execution.
    #[arg(long, hide = true)]
    pub sandbox_mode: bool,
}

// ---------------------------------------------------------------------------
// Top-level subcommand group
// ---------------------------------------------------------------------------

/// Top-level subcommands exposed by `swage`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Run a single prompt against the gateway and print the streamed reply.
    Run(RunArgs),
    /// Manage the Swage gateway process.
    Gateway(GatewayArgs),
    /// Launch the terminal user interface.
    Tui(TuiArgs),
    /// Generate shell autocompletion scripts.
    Completions(CompletionsArgs),
}

// ---------------------------------------------------------------------------
// Run argument struct
// ---------------------------------------------------------------------------

/// Arguments for `swage run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Gateway URL to connect to.
    ///
    /// Defaults to the running gateway's address found via runtime metadata,
    /// or `http://127.0.0.1:3000` if no metadata is available.
    #[arg(long)]
    pub url: Option<String>,

    /// Path to an audio file that should be transcribed before sending the turn.
    #[arg(long)]
    pub audio_input: Option<String>,

    /// Path where synthesized response audio should be written.
    #[arg(long)]
    pub audio_output: Option<String>,

    /// Optional mime type for `--audio-input`.
    #[arg(long, default_value = "audio/wav")]
    pub audio_mime_type: String,

    /// Optional voice name for synthesized output.
    #[arg(long)]
    pub voice: Option<String>,

    /// Output format for synthesized speech.
    #[arg(long, default_value = "wav")]
    pub audio_format: String,

    /// Prompt text to send. When omitted, the command reads stdin.
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Gateway subcommand group
// ---------------------------------------------------------------------------

/// Arguments for `swage gateway <subcommand>`.
#[derive(Debug, clap::Args)]
pub struct GatewayArgs {
    #[command(subcommand)]
    pub command: GatewayCommand,
}

/// Gateway lifecycle subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum GatewayCommand {
    /// Store default lifecycle configuration for this machine.
    Install(InstallArgs),
    /// Remove stored lifecycle configuration for this machine.
    Uninstall(UninstallArgs),
    /// Launch the gateway as a background process.
    Start(StartArgs),
    /// Restart the gateway, optionally forcing the stop phase.
    Restart(RestartArgs),
    /// Stop the running gateway.
    Stop(StopArgs),
    /// Check whether the gateway is running.
    Status,
}

// ---------------------------------------------------------------------------
// Start / Stop argument structs
// ---------------------------------------------------------------------------

/// Arguments for `swage gateway start`.
#[derive(Debug, clap::Args)]
pub struct StartArgs {
    /// TCP port for the gateway to listen on (overrides config).
    #[arg(long)]
    pub port: Option<u16>,

    /// Host address to bind (overrides config).
    #[arg(long)]
    pub host: Option<String>,

    /// Path to a configuration file (overrides the default search path).
    #[arg(long)]
    pub config: Option<String>,

    /// Run the gateway in the foreground rather than detaching to the background.
    #[arg(long)]
    pub foreground: bool,
}

/// Arguments for `swage gateway install`.
#[derive(Debug, clap::Args)]
pub struct InstallArgs {
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long)]
    pub config: Option<String>,
}

/// Arguments for `swage gateway stop`.
#[derive(Debug, clap::Args)]
pub struct StopArgs {
    /// Send SIGTERM directly instead of using the graceful HTTP shutdown endpoint.
    ///
    /// Use when the gateway is unresponsive to HTTP requests.
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `swage gateway restart`.
#[derive(Debug, clap::Args)]
pub struct RestartArgs {
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long)]
    pub config: Option<String>,
    #[arg(long)]
    pub foreground: bool,
}

/// Arguments for `swage gateway uninstall`.
#[derive(Debug, clap::Args, Default)]
pub struct UninstallArgs {}

// ---------------------------------------------------------------------------
// TUI argument struct
// ---------------------------------------------------------------------------

/// Arguments for `swage tui`.
#[derive(Debug, clap::Args)]
pub struct TuiArgs {
    /// Gateway URL to connect to (overrides runtime metadata discovery).
    ///
    /// Defaults to the running gateway's address found via runtime metadata,
    /// or `http://127.0.0.1:3000` if no gateway metadata is available.
    #[arg(long)]
    pub url: Option<String>,
}

// ---------------------------------------------------------------------------
// Completions argument struct
// ---------------------------------------------------------------------------

/// Arguments for `swage completions <shell>`.
#[derive(Debug, clap::Args)]
pub struct CompletionsArgs {
    /// The target shell to generate completions for.
    pub shell: clap_complete::Shell,
}

// ---------------------------------------------------------------------------
// Tests (written first — TDD)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // Parsing `swage gateway start` with no flags should produce all-None/false defaults.
    #[test]
    fn parse_gateway_start_defaults() {
        let cli = Cli::try_parse_from(["swage", "gateway", "start"]).unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        let GatewayCommand::Start(args) = gw.command else {
            panic!("expected Start subcommand");
        };
        assert!(args.port.is_none());
        assert!(args.host.is_none());
        assert!(args.config.is_none());
        assert!(!args.foreground);
    }

    #[test]
    fn parse_run_with_inline_message() {
        let cli = Cli::try_parse_from(["swage", "run", "hello"]).unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected Run command");
        };
        assert_eq!(args.message.as_deref(), Some("hello"));
        assert!(args.url.is_none());
        assert!(args.audio_input.is_none());
    }

    #[test]
    fn parse_run_with_explicit_url() {
        let cli = Cli::try_parse_from(["swage", "run", "--url", "http://localhost:9999"]).unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected Run command");
        };
        assert_eq!(args.url.as_deref(), Some("http://localhost:9999"));
        assert!(args.message.is_none());
    }

    #[test]
    fn parse_run_with_audio_flags() {
        let cli = Cli::try_parse_from([
            "swage",
            "run",
            "--audio-input",
            "input.wav",
            "--audio-output",
            "reply.wav",
            "--voice",
            "nova",
        ])
        .unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected Run command");
        };
        assert_eq!(args.audio_input.as_deref(), Some("input.wav"));
        assert_eq!(args.audio_output.as_deref(), Some("reply.wav"));
        assert_eq!(args.voice.as_deref(), Some("nova"));
        assert_eq!(args.audio_mime_type, "audio/wav");
        assert_eq!(args.audio_format, "wav");
    }

    // All StartArgs flags round-trip through the parser.
    #[test]
    fn parse_gateway_start_with_flags() {
        let cli = Cli::try_parse_from([
            "swage",
            "gateway",
            "start",
            "--port",
            "8080",
            "--host",
            "0.0.0.0",
            "--config",
            "/tmp/config.toml",
            "--foreground",
        ])
        .unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        let GatewayCommand::Start(args) = gw.command else {
            panic!("expected Start subcommand");
        };
        assert_eq!(args.port, Some(8080u16));
        assert_eq!(args.host.as_deref(), Some("0.0.0.0"));
        assert_eq!(args.config.as_deref(), Some("/tmp/config.toml"));
        assert!(args.foreground);
    }

    // `swage gateway stop` defaults to force=false.
    #[test]
    fn parse_gateway_stop_defaults() {
        let cli = Cli::try_parse_from(["swage", "gateway", "stop"]).unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        let GatewayCommand::Stop(args) = gw.command else {
            panic!("expected Stop subcommand");
        };
        assert!(!args.force);
    }

    #[test]
    fn parse_gateway_install_defaults() {
        let cli = Cli::try_parse_from(["swage", "gateway", "install"]).unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        let GatewayCommand::Install(args) = gw.command else {
            panic!("expected Install subcommand");
        };
        assert!(args.port.is_none());
        assert!(args.host.is_none());
        assert!(args.config.is_none());
    }

    #[test]
    fn parse_gateway_restart_flags() {
        let cli = Cli::try_parse_from([
            "swage",
            "gateway",
            "restart",
            "--force",
            "--port",
            "4000",
        ])
        .unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        let GatewayCommand::Restart(args) = gw.command else {
            panic!("expected Restart subcommand");
        };
        assert!(args.force);
        assert_eq!(args.port, Some(4000));
    }

    // `--force` flag on stop is parsed correctly.
    #[test]
    fn parse_gateway_stop_force() {
        let cli = Cli::try_parse_from(["swage", "gateway", "stop", "--force"]).unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        let GatewayCommand::Stop(args) = gw.command else {
            panic!("expected Stop subcommand");
        };
        assert!(args.force);
    }

    // `swage gateway status` is a unit variant — no fields.
    #[test]
    fn parse_gateway_status() {
        let cli = Cli::try_parse_from(["swage", "gateway", "status"]).unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        assert!(matches!(gw.command, GatewayCommand::Status));
    }

    // Shell completions with bash positional argument.
    #[test]
    fn parse_completions_bash() {
        let cli = Cli::try_parse_from(["swage", "completions", "bash"]).unwrap();
        let Some(Command::Completions(args)) = cli.command else {
            panic!("expected Completions command");
        };
        assert_eq!(args.shell, clap_complete::Shell::Bash);
    }

    // Shell completions with zsh positional argument.
    #[test]
    fn parse_completions_zsh() {
        let cli = Cli::try_parse_from(["swage", "completions", "zsh"]).unwrap();
        let Some(Command::Completions(args)) = cli.command else {
            panic!("expected Completions command");
        };
        assert_eq!(args.shell, clap_complete::Shell::Zsh);
    }

    // Shell completions with fish positional argument.
    #[test]
    fn parse_completions_fish() {
        let cli = Cli::try_parse_from(["swage", "completions", "fish"]).unwrap();
        let Some(Command::Completions(args)) = cli.command else {
            panic!("expected Completions command");
        };
        assert_eq!(args.shell, clap_complete::Shell::Fish);
    }

    // No subcommand → command field is None (caller shows help).
    #[test]
    fn parse_no_subcommand_returns_none() {
        let cli = Cli::try_parse_from(["swage"]).unwrap();
        assert!(cli.command.is_none());
        assert!(!cli.server_mode);
        assert!(!cli.sandbox_mode);
    }

    // `--server-mode` flag is accepted and parsed as true.
    #[test]
    fn parse_server_mode_flag() {
        let cli = Cli::try_parse_from(["swage", "--server-mode"]).unwrap();
        assert!(cli.server_mode);
    }

    #[test]
    fn parse_sandbox_mode_flag() {
        let cli = Cli::try_parse_from(["swage", "--sandbox-mode"]).unwrap();
        assert!(cli.sandbox_mode);
    }

    // `--server-mode` must be hidden so it never appears in help output.
    #[test]
    fn server_mode_arg_is_hidden() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "server_mode")
            .expect("--server-mode argument not found in Cli");
        assert!(
            arg.is_hide_set(),
            "--server-mode should be hidden from help output"
        );
    }

    // The top-level command must declare a version so `--version` works.
    #[test]
    fn version_is_declared() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        assert!(
            cmd.get_version().is_some(),
            "Cli must declare a version for --version to work"
        );
    }
}

//! Combined binary entry point for the `swage` platform.
//!
//! Dispatches either to the embedded gateway server (when `--server-mode` is
//! set) or to the appropriate CLI handler for gateway lifecycle management and
//! shell completions.

use async_trait::async_trait;
use clap::{CommandFactory, Parser};
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_gateway::cli::{Cli, Command, GatewayCommand};
use swage_gateway::tool::{
    file_edit::FileEditTool, file_list::FileListTool, file_read::FileReadTool,
    file_search::FileSearchTool, file_write::FileWriteTool, shell::ShellTool,
};
use swage_tool::context::ToolContext;
use swage_tool::tool::Tool;

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Repeat the given parameters back unchanged."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        Ok(params)
    }
}

struct EnvReadTool;

#[async_trait]
impl Tool for EnvReadTool {
    fn name(&self) -> &str {
        "env_read"
    }

    fn description(&self) -> &str {
        "Read one environment variable by name, returning null when it is unset."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: name".to_owned(),
            })?;

        Ok(std::env::var(name)
            .ok()
            .map(Value::String)
            .unwrap_or(Value::Null))
    }
}

fn build_sandbox_agent_tools() -> Vec<Box<dyn Tool>> {
    let tool_config = swage_config::ToolConfig::default();
    let max_output_bytes = tool_config.max_output_bytes;

    vec![
        Box::new(EchoTool),
        Box::new(EnvReadTool),
        Box::new(ShellTool {
            timeout: std::time::Duration::from_secs(tool_config.shell_timeout_secs),
            max_output_bytes,
        }),
        Box::new(FileReadTool { max_output_bytes }),
        Box::new(FileEditTool),
        Box::new(FileWriteTool),
        Box::new(FileSearchTool { max_output_bytes }),
        Box::new(FileListTool),
    ]
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if cli.sandbox_mode {
        if let Err(error) = swage_sandbox::run_agent_loop(build_sandbox_agent_tools()).await {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
        return;
    }

    // --server-mode is set when we re-exec ourselves as a background daemon.
    // Skip all CLI dispatch and run the gateway directly.
    if cli.server_mode {
        let (port, host, config) = match &cli.command {
            Some(Command::Gateway(args)) => match &args.command {
                GatewayCommand::Start(start_args) => (
                    start_args.port,
                    start_args.host.clone(),
                    start_args.config.clone(),
                ),
                _ => (None, None, None),
            },
            _ => (None, None, None),
        };
        swage_gateway::startup::run_gateway(port, host, config).await;
        return;
    }

    let result = match cli.command {
        Some(Command::Run(run_args)) => swage_gateway::cli::run::run(&run_args).await,
        Some(Command::Gateway(args)) => match args.command {
            GatewayCommand::SetDefaults(install_args) => {
                swage_gateway::cli::install::install(&install_args).await
            }
            GatewayCommand::ClearDefaults(uninstall_args) => {
                swage_gateway::cli::install::uninstall(&uninstall_args).await
            }
            GatewayCommand::Start(start_args) => swage_gateway::cli::start::run(&start_args).await,
            GatewayCommand::Restart(restart_args) => {
                swage_gateway::cli::restart::run(&restart_args).await
            }
            GatewayCommand::Stop(stop_args) => swage_gateway::cli::stop::run(&stop_args).await,
            GatewayCommand::Status => swage_gateway::cli::status::run().await,
        },
        Some(Command::Chat(tui_args)) => swage_gateway::cli::tui::run(&tui_args).await,
        Some(Command::Device(args)) => match args.command {
            swage_gateway::cli::device::DeviceCommand::Connect(connect_args) => {
                swage_gateway::cli::device::connect(&connect_args).await
            }
        },
        Some(Command::GenerateCompletion(comp_args)) => {
            swage_gateway::cli::completions::run(&comp_args);
            Ok(())
        }
        None => {
            // No subcommand — print help and exit 0.
            Cli::command().print_help().ok();
            println!();
            return;
        }
    };

    std::process::exit(to_exit_code(result));
}

/// Map a handler result to a process exit code.
///
/// `Ok(())` → 0 (success). Any error → prints the message to stderr and
/// returns 1 (generic failure), so the shell receives a non-zero status.
fn to_exit_code(result: Result<(), swage_client::error::ClientError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use swage_gateway::cli::{Cli, Command, GatewayCommand};

    #[test]
    fn success_result_maps_to_exit_zero() {
        assert_eq!(super::to_exit_code(Ok(())), 0);
    }

    #[test]
    fn error_result_maps_to_nonzero_exit() {
        let err: Result<(), swage_client::error::ClientError> =
            Err(swage_client::error::ClientError::GatewayNotRunning {
                reason: "test".to_string(),
            });
        assert_ne!(super::to_exit_code(err), 0);
    }

    #[test]
    fn help_flag_produces_parse_error_with_help_kind() {
        let err = Cli::try_parse_from(["swage", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn server_mode_flag_is_accepted() {
        let cli = Cli::try_parse_from(["swage", "--server-mode"]).unwrap();
        assert!(cli.server_mode);
        assert!(cli.command.is_none());
    }

    #[test]
    fn sandbox_mode_flag_is_accepted() {
        let cli = Cli::try_parse_from(["swage", "--sandbox-mode"]).unwrap();
        assert!(cli.sandbox_mode);
        assert!(cli.command.is_none());
    }

    #[test]
    fn sandbox_agent_registers_workspace_tools() {
        let tools = super::build_sandbox_agent_tools();
        let names: std::collections::HashSet<_> = tools.iter().map(|tool| tool.name()).collect();

        for name in [
            "shell",
            "file_edit",
            "file_read",
            "file_write",
            "file_search",
            "file_list",
        ] {
            assert!(names.contains(name), "sandbox agent must register {name}");
        }
        assert!(
            !names.contains("memory"),
            "shared memory lives in the session's scope on the gateway host, not in the sandbox"
        );
    }

    #[test]
    fn server_mode_accepts_embedded_gateway_start_args() {
        let cli = Cli::try_parse_from([
            "swage",
            "--server-mode",
            "gateway",
            "start",
            "--port",
            "8080",
            "--host",
            "0.0.0.0",
            "--config",
            "/tmp/swage.toml",
        ])
        .unwrap();
        assert!(cli.server_mode);

        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        let GatewayCommand::Start(args) = gw.command else {
            panic!("expected Start subcommand");
        };
        assert_eq!(args.port, Some(8080));
        assert_eq!(args.host.as_deref(), Some("0.0.0.0"));
        assert_eq!(args.config.as_deref(), Some("/tmp/swage.toml"));
    }

    #[test]
    fn gateway_start_dispatches_to_correct_variant() {
        let cli = Cli::try_parse_from(["swage", "gateway", "start"]).unwrap();
        assert!(!cli.server_mode);
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        assert!(matches!(gw.command, GatewayCommand::Start(_)));
    }

    #[test]
    fn gateway_stop_dispatches_to_correct_variant() {
        let cli = Cli::try_parse_from(["swage", "gateway", "stop"]).unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        assert!(matches!(gw.command, GatewayCommand::Stop(_)));
    }

    #[test]
    fn gateway_status_dispatches_to_correct_variant() {
        let cli = Cli::try_parse_from(["swage", "gateway", "status"]).unwrap();
        let Some(Command::Gateway(gw)) = cli.command else {
            panic!("expected Gateway command");
        };
        assert!(matches!(gw.command, GatewayCommand::Status));
    }

    #[test]
    fn completions_dispatches_to_correct_variant() {
        let cli = Cli::try_parse_from(["swage", "generate-completion", "bash"]).unwrap();
        assert!(matches!(cli.command, Some(Command::GenerateCompletion(_))));
    }

    #[test]
    fn no_subcommand_produces_none_command() {
        let cli = Cli::try_parse_from(["swage"]).unwrap();
        assert!(cli.command.is_none());
        assert!(!cli.server_mode);
    }
}

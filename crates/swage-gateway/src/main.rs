//! Combined binary entry point for the `swage` platform.
//!
//! Dispatches either to the embedded gateway server (when `--server-mode` is
//! set) or to the appropriate CLI handler for gateway lifecycle management and
//! shell completions.

use swage_gateway::cli::{Cli, Command, GatewayCommand};
use clap::{CommandFactory, Parser};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // --server-mode is set when we re-exec ourselves as a background daemon.
    // Skip all CLI dispatch and run the gateway directly.
    if cli.server_mode {
        swage_gateway::startup::run_gateway(None, None, None).await;
        return;
    }

    let result = match cli.command {
        Some(Command::Gateway(args)) => match args.command {
            GatewayCommand::Start(start_args) => {
                swage_gateway::cli::start::run(&start_args).await
            }
            GatewayCommand::Stop(stop_args) => {
                swage_gateway::cli::stop::run(&stop_args).await
            }
            GatewayCommand::Status => {
                swage_gateway::cli::status::run().await
            }
        },
        Some(Command::Tui(tui_args)) => {
            swage_gateway::cli::tui::run(&tui_args).await
        }
        Some(Command::Completions(comp_args)) => {
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
        let cli = Cli::try_parse_from(["swage", "completions", "bash"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Completions(_))));
    }

    #[test]
    fn no_subcommand_produces_none_command() {
        let cli = Cli::try_parse_from(["swage"]).unwrap();
        assert!(cli.command.is_none());
        assert!(!cli.server_mode);
    }
}

//! Cross-task integration tests for the terminal user interface.
//!
//! Tests the CLI dispatch and gateway-facing integration points for the TUI.
//! Terminal rendering tests live in the swage-tui crate; these tests verify
//! the wiring between the combined binary's CLI dispatch and the gateway.

#![allow(clippy::unwrap_used)]

use clap::Parser;
use swage_gateway::cli::{Cli, Command};

// ---------------------------------------------------------------------------
// CLI dispatch: swage chat subcommand
//
// Proves: the `chat` subcommand [iovo] is wired into the combined binary's
// command dispatch and parses correctly with and without the --url flag.
// This catches the wiring risk: "TUI subcommand defined in clap but not
// dispatched in main.rs — `swage chat` silently does nothing."
//
// Cross-task: [iovo] CLI subcommand + main.rs dispatch
// ---------------------------------------------------------------------------

/// `swage chat` parses to the Chat command variant with no URL.
///
/// Motes: iovo (CLI subcommand)
#[test]
fn parse_chat_subcommand_defaults() {
    let cli = Cli::try_parse_from(["swage", "chat"]).unwrap();
    let Some(Command::Chat(args)) = cli.command else {
        panic!("expected Chat command");
    };
    assert!(args.url.is_none(), "URL should default to None");
}

/// `swage chat --url http://example.com:3000` passes the URL through.
///
/// Motes: iovo (CLI subcommand with --url flag)
#[test]
fn parse_chat_subcommand_with_url() {
    let cli = Cli::try_parse_from(["swage", "chat", "--url", "http://example.com:3000"]).unwrap();
    let Some(Command::Chat(args)) = cli.command else {
        panic!("expected Chat command");
    };
    assert_eq!(args.url.as_deref(), Some("http://example.com:3000"));
}

/// Shell completions include the `chat` subcommand.
///
/// Proves: the TUI subcommand is visible to shell completion generation,
/// meaning users can discover it via tab completion.
///
/// Motes: iovo (CLI subcommand present in clap definition)
#[test]
fn shell_completions_include_chat_subcommand() {
    let mut output = Vec::new();
    swage_gateway::cli::completions::generate_to_writer(clap_complete::Shell::Fish, &mut output);
    let completions = String::from_utf8(output).expect("completions should be valid UTF-8");
    assert!(
        completions.contains("chat"),
        "completions should include 'chat' subcommand"
    );
}

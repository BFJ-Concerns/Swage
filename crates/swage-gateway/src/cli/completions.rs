//! `swage completions <shell>` — generate shell autocompletion scripts.
//!
//! Uses clap_complete to generate completions from the command definition,
//! ensuring they stay in sync with the actual CLI structure automatically.

use clap::CommandFactory;

use super::{Cli, CompletionsArgs};

/// Write completions for `shell` to `writer`.
///
/// Derives the command definition from the `Cli` struct directly, so the
/// output is always in sync with the actual binary structure — no manual
/// maintenance required.
pub fn generate_to_writer(shell: clap_complete::Shell, writer: &mut dyn std::io::Write) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "swage", writer);
}

/// Generate completions for the requested shell and print to stdout.
///
/// Clap validates the shell name before this function is called, so no
/// additional validation is needed here — invalid shell names are rejected
/// at parse time.
pub fn run(args: &CompletionsArgs) {
    generate_to_writer(args.shell, &mut std::io::stdout());
}

// ---------------------------------------------------------------------------
// Tests (written first — TDD)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // generate_to_writer must produce output for bash that contains key tokens.
    #[test]
    fn bash_completions_contain_gateway_subcommand() {
        let mut buf = Vec::<u8>::new();
        generate_to_writer(clap_complete::Shell::Bash, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("gateway"),
            "bash completions should mention 'gateway'"
        );
    }

    // The output for zsh should contain the start, stop, and status subcommands.
    #[test]
    fn zsh_completions_contain_lifecycle_subcommands() {
        let mut buf = Vec::<u8>::new();
        generate_to_writer(clap_complete::Shell::Zsh, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("start"),  "zsh completions should mention 'start'");
        assert!(output.contains("stop"),   "zsh completions should mention 'stop'");
        assert!(output.contains("status"), "zsh completions should mention 'status'");
    }

    // Fish completions should include the completions subcommand itself.
    #[test]
    fn fish_completions_contain_completions_token() {
        let mut buf = Vec::<u8>::new();
        generate_to_writer(clap_complete::Shell::Fish, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("completions"),
            "fish completions should mention 'completions'"
        );
    }

    // Output must be non-empty for every supported shell.
    #[test]
    fn all_shells_produce_non_empty_output() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
        ] {
            let mut buf = Vec::<u8>::new();
            generate_to_writer(shell, &mut buf);
            assert!(!buf.is_empty(), "completions output for {shell:?} should not be empty");
        }
    }

    // run() should write to stdout without panicking.
    // We test via generate_to_writer (run() is a thin wrapper) — already covered above.
}

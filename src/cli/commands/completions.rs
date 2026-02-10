//! Generate shell completions.

use crate::cli::app::Cli;
use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::io;

/// Generate shell completions for the given shell.
pub fn run_completions(shell: Shell) {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "engramdb", &mut io::stdout());
}

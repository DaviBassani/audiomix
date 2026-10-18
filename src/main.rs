use std::io::IsTerminal;

use anyhow::{Context, Result};
use clap::Parser;

use audiomix::backend::PactlBackend;
use audiomix::cli::Cli;
use audiomix::{commands, tui};

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("erro: {err:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let backend = PactlBackend::new();

    match cli.command {
        Some(command) => commands::dispatch(&backend, command),
        None => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                anyhow::bail!(
                    "a interface precisa de um terminal interativo na entrada e na saída; use `audiomix --help` para subcomandos"
                );
            }
            tui::run().context("modo interativo")
        }
    }
}

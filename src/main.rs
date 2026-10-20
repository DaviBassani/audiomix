#[cfg(target_os = "linux")]
use std::io::IsTerminal;

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::Result;
#[cfg(target_os = "linux")]
use clap::Parser;

#[cfg(target_os = "linux")]
use audiomix::backend::PactlBackend;
#[cfg(target_os = "linux")]
use audiomix::cli::Cli;
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "windows")]
fn run() -> Result<()> {
    audiomix::windows_app::run()
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn run() -> Result<()> {
    anyhow::bail!("AudioMix suporta Linux e Windows; esta plataforma nao e suportada")
}

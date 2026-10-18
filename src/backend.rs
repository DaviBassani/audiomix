//! Camada de acesso ao `pactl`.
//!
//! O backend é abstraído por um trait para permitir injeção em testes. A
//! implementação real invoca o binário com um vetor de argumentos
//! (`std::process::Command`), sem nunca passar por um shell.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// Executa `pactl` com os argumentos indicados.
pub trait Backend {
    /// Retorna o stdout em caso de sucesso; erro (com stderr) em caso de falha.
    fn run(&self, args: &[&str]) -> Result<String>;
}

/// Backend real: invoca `pactl` diretamente, sem shell.
pub struct PactlBackend {
    /// Caminho do binário. Pode ser sobrescrito por `AUDIOMIX_PACTL` (testes).
    pub bin: String,
    timeout: Duration,
}

impl Default for PactlBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl PactlBackend {
    pub fn new() -> Self {
        let bin = std::env::var("AUDIOMIX_PACTL").unwrap_or_else(|_| "pactl".to_string());
        Self {
            bin,
            timeout: Duration::from_secs(10),
        }
    }
}

/// Kills every process in the dedicated group created for this invocation.
/// ESRCH is expected when the group is already empty.
fn kill_process_group(pid: u32) {
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

impl Backend for PactlBackend {
    fn run(&self, args: &[&str]) -> Result<String> {
        let mut child = Command::new(&self.bin)
            .args(args)
            .env("LC_ALL", "C")
            // Each invocation gets its own process group so a wrapper that
            // leaves descendants behind can be cleaned up without touching
            // our own group or unrelated processes.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "não foi possível executar `{}` (pactl instalado?)",
                    self.bin
                )
            })?;
        // Drain both pipes while waiting so a large device list cannot deadlock.
        let group_id = child.id();
        let mut stdout = child
            .stdout
            .take()
            .context("stdout do pactl indisponivel")?;
        let mut stderr = child
            .stderr
            .take()
            .context("stderr do pactl indisponivel")?;
        let out_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let err_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let status = match child.wait_timeout(self.timeout) {
            Ok(Some(status)) => {
                // The direct child exited, but a wrapper may still hold the
                // pipes open through a descendant. Kill its dedicated group
                // before joining the reader threads.
                kill_process_group(group_id);
                status
            }
            result => {
                kill_process_group(group_id);
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                match result {
                    Ok(None) => bail!("`{}` excedeu o limite de {} segundos; confira o servidor de audio e atualize novamente", self.bin, self.timeout.as_secs_f32()),
                    Err(error) => return Err(error).context("falha ao aguardar pactl"),
                    Ok(Some(_)) => unreachable!(),
                }
            }
        };
        let stdout = out_reader
            .join()
            .map_err(|_| anyhow!("leitura stdout interrompida"))??;
        let stderr = err_reader
            .join()
            .map_err(|_| anyhow!("leitura stderr interrompida"))??;
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            let msg = stderr.trim();
            if msg.is_empty() {
                bail!(
                    "`{} {}` falhou (código {:?})",
                    self.bin,
                    args.join(" "),
                    status.code()
                );
            }
            bail!("`{} {}` falhou: {}", self.bin, args.join(" "), msg);
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_process_is_reaped_and_returns_a_timeout_error() {
        let backend = PactlBackend {
            bin: "sleep".into(),
            timeout: Duration::from_millis(80),
        };
        let started = std::time::Instant::now();
        let error = backend.run(&["5"]).unwrap_err();
        assert!(error.to_string().contains("limite"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn wrapper_descendant_holding_stdout_cannot_delay_a_normal_exit() {
        let backend = PactlBackend {
            bin: "sh".into(),
            timeout: Duration::from_secs(5),
        };
        let started = std::time::Instant::now();
        let output = backend.run(&["-c", "sleep 5 & exit 0"]).unwrap();
        assert!(output.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "reader threads waited for the wrapper descendant"
        );
    }

    #[test]
    fn wrapper_descendant_is_killed_with_the_timed_out_group() {
        let backend = PactlBackend {
            bin: "sh".into(),
            timeout: Duration::from_millis(80),
        };
        let started = std::time::Instant::now();
        let error = backend.run(&["-c", "sleep 5 & wait"]).unwrap_err();
        assert!(error.to_string().contains("limite"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout path waited for the wrapper descendant"
        );
    }

    #[test]
    fn process_output_and_failure_diagnostics_are_preserved() {
        let backend = PactlBackend {
            bin: "printf".into(),
            timeout: Duration::from_secs(2),
        };
        assert_eq!(backend.run(&["%s", "hello"]).unwrap(), "hello");
        let backend = PactlBackend {
            bin: "false".into(),
            timeout: Duration::from_secs(2),
        };
        assert!(backend.run(&[]).is_err());
    }
}

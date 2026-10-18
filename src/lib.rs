//! audiomix — utilitário de linha de comando em português para combinar
//! saídas de áudio no Linux através do `pactl` (PulseAudio / PipeWire-Pulse).
//!
//! Todas as chamadas ao backend são feitas por argumentos vetoriais
//! (`std::process::Command`), nunca por um shell.

pub mod backend;
pub mod cli;
pub mod combine;
pub mod commands;
pub mod devices;
pub mod model;
pub mod selector;
pub mod tui;

//! Audio routing with a Linux pactl backend and an experimental native
//! Windows WASAPI loopback backend. Each platform exposes its real capabilities.

#[cfg(target_os = "linux")]
pub mod backend;
#[cfg(target_os = "linux")]
pub mod cli;
#[cfg(target_os = "linux")]
pub mod combine;
#[cfg(target_os = "linux")]
pub mod commands;
#[cfg(target_os = "linux")]
pub mod devices;
#[cfg(target_os = "linux")]
pub mod model;
#[cfg(target_os = "linux")]
pub mod selector;
#[cfg(target_os = "linux")]
pub mod tui;
pub mod windows_app;
pub mod windows_audio;

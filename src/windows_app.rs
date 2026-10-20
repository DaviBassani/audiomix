//! Windows terminal application (WASAPI mirroring).
//!
//! This module owns only the interactive presentation and the CLI parser for
//! Windows. The native audio core lives in [`crate::windows_audio`] and is used
//! through the small [`CoreApi`] seam so every keyboard decision and rendering
//! path can be tested on Linux without WASAPI.
//!
//! Deliberate limits of this Windows front end:
//!
//! * the source is always the *current default* render endpoint and is locked;
//! * targets are secondary render endpoints only;
//! * the program never changes the Windows default device or Bluetooth
//!   profiles, and it does not emulate the Linux `pactl` group engine;
//! * a mirror session exists only inside this process and is stopped and
//!   joined before the terminal is released.

use std::collections::BTreeSet;
use std::io::{stdout, IsTerminal};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::windows_audio::{self, AudioErrorKind, Endpoint, MirrorEvent, MirrorStats};

// ---------------------------------------------------------------------------
// Palette (mirrors the Linux audiomix navy/teal/amber interface)
// ---------------------------------------------------------------------------

const NAVY: Color = Color::Rgb(10, 18, 32);
const PANEL: Color = Color::Rgb(15, 27, 45);
const PANEL_ALT: Color = Color::Rgb(20, 35, 56);
const TEAL: Color = Color::Rgb(66, 196, 184);
const AMBER: Color = Color::Rgb(240, 181, 94);
const TEXT: Color = Color::Rgb(218, 227, 240);
const MUTED: Color = Color::Rgb(132, 153, 180);
const RED: Color = Color::Rgb(232, 116, 116);
const GREEN: Color = Color::Rgb(112, 208, 158);

pub const MIN_WIDTH: u16 = 55;
pub const MIN_HEIGHT: u16 = 18;

fn bg_panel() -> Style {
    Style::default().bg(PANEL).fg(TEXT)
}

fn muted() -> Style {
    Style::default().fg(MUTED)
}

fn accent() -> Style {
    Style::default().fg(TEAL).add_modifier(Modifier::BOLD)
}

fn amber() -> Style {
    Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
}

/// Removes control characters from a value for display while leaving the
/// caller's raw identifier untouched.
pub fn sanitize_display(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

fn clip(value: &str, width: u16) -> String {
    let max = width as usize;
    let mut used = 0usize;
    let mut out = String::new();
    for grapheme in Span::raw(sanitize_display(value)).styled_graphemes(Style::default()) {
        let cells = unicode_width::UnicodeWidthStr::width(grapheme.symbol);
        if used + cells > max {
            break;
        }
        out.push_str(grapheme.symbol);
        used += cells;
    }
    out
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// Windows command line. Kept public so parser behaviour is testable without a
/// terminal or an audio device.
#[derive(Debug, Parser)]
#[command(
    name = "audiomix",
    version,
    about = "Espelhamento de audio WASAPI no Windows (fonte: saida padrao atual)",
    long_about = None
)]
pub struct WindowsCli {
    #[command(subcommand)]
    pub command: Option<WindowsCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum WindowsCommand {
    /// Lista as saidas de renderizacao nativas (nome, id e marcacao de padrao).
    List {
        /// Emite JSON com id, nome, padrao, canais e taxa de amostragem.
        #[arg(long)]
        json: bool,
    },
    /// Inicia o espelhamento em primeiro plano (Ctrl-C encerra com limpeza).
    Mirror {
        /// Id ou nome exato da saida padrao atual.
        #[arg(long, value_name = "SELETOR")]
        source: String,
        /// Uma ou mais saidas secundarias (ids ou nomes exatos).
        #[arg(value_name = "ALVO", required = true, num_args = 1..)]
        targets: Vec<String>,
    },
    /// Nao suportado no Windows: o padrao e definido nas Configuracoes.
    Default,
    /// Nao suportado no Windows: nao ha perfis/pares A2DP gerenciados aqui.
    Stereo,
    /// Nao suportado no Windows: grupos/pactl sao exclusivos do backend Linux.
    Groups,
}

impl WindowsCommand {
    pub fn unsupported_message(&self) -> Option<&'static str> {
        match self {
            Self::Default => Some(
                "comando 'default' nao e suportado no Windows; escolha o dispositivo padrao em Configuracoes > Sistema > Som",
            ),
            Self::Stereo => Some(
                "comando 'stereo' nao e suportado no Windows; perfis e pares Bluetooth/A2DP nao sao alterados por este programa",
            ),
            Self::Groups => Some(
                "comando 'groups' nao e suportado no Windows; grupos PulseAudio/PipeWire sao exclusivos do backend Linux",
            ),
            _ => None,
        }
    }
}

/// Entry point used by `main` on Windows.
pub fn run() -> Result<()> {
    let cli = WindowsCli::parse();
    match cli.command {
        None => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                use clap::CommandFactory;
                let mut command = WindowsCli::command();
                let _ = command.print_help();
                bail!(
                    "a interface precisa de um terminal interativo na entrada e na saida; use `audiomix list [--json]` ou `audiomix mirror --source SELETOR ALVO...`"
                );
            }
            run_tui(Arc::new(RealCore), None, false)
        }
        Some(WindowsCommand::List { json }) => cli_list(json),
        Some(WindowsCommand::Mirror { source, targets }) => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                bail!("o modo `mirror` precisa de um terminal interativo para exibir o estado e encerrar com Ctrl-C");
            }
            // Resolve selectors before entering the alternate screen so bad
            // arguments produce a normal, actionable command-line error.
            let endpoints = RealCore
                .list_endpoints()
                .map_err(anyhow::Error::msg)
                .context("listar saidas de audio do Windows")?;
            let preset = Preset::resolve(&endpoints, &source, &targets)?;
            run_tui(Arc::new(RealCore), Some(preset), true)
        }
        Some(command) => {
            let message = command
                .unsupported_message()
                .expect("unsupported subcommand has a message");
            bail!("{message}");
        }
    }
}

fn cli_list(json: bool) -> Result<()> {
    let endpoints = RealCore
        .list_endpoints()
        .map_err(anyhow::Error::msg)
        .context("listar saidas de audio do Windows")?;

    if json {
        let values: Vec<serde_json::Value> = endpoints
            .iter()
            .map(|endpoint| {
                serde_json::json!({
                    "id": endpoint.id,
                    "name": endpoint.name,
                    "is_default": endpoint.is_default,
                    "channels": endpoint.channels,
                    "sample_rate": endpoint.sample_rate,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&values)?);
        return Ok(());
    }

    if endpoints.is_empty() {
        println!("Nenhuma saida de renderizacao ativa encontrada.");
        return Ok(());
    }
    for endpoint in endpoints {
        let marker = if endpoint.is_default {
            "[padrao]"
        } else {
            "        "
        };
        let format = match (endpoint.channels, endpoint.sample_rate) {
            (Some(channels), Some(rate)) => format!("  {rate} Hz / {channels} ch"),
            _ => String::new(),
        };
        println!(
            "{marker} {}  id={}{format}",
            sanitize_display(&endpoint.name),
            sanitize_display(&endpoint.id)
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core seam
// ---------------------------------------------------------------------------

/// Presentation-level event. It is intentionally independent from WASAPI so
/// tests can drive the complete UI state machine on any platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    Endpoints(Result<Vec<Endpoint>, String>),
    Started {
        source: Endpoint,
        targets: Vec<Endpoint>,
    },
    Stats(MirrorStats),
    Stopped,
    Error {
        message: String,
        device_lost: bool,
    },
}

/// Minimal audio-core contract used by the front end.
pub trait CoreApi: Send + Sync {
    fn list_endpoints(&self) -> Result<Vec<Endpoint>, String>;

    /// Starts a session and blocks until it stops. Implementations must send
    /// `Started` only after every native client is running. The session wrapper
    /// alone emits `Stopped`, after this method returns and releases resources.
    fn start_mirror(
        &self,
        source: &str,
        targets: &[String],
        stop: Arc<AtomicBool>,
        events: Sender<UiEvent>,
    ) -> Result<(), String>;
}

/// Adapter over [`crate::windows_audio`].
pub struct RealCore;

impl CoreApi for RealCore {
    fn list_endpoints(&self) -> Result<Vec<Endpoint>, String> {
        windows_audio::list_endpoints().map_err(|error| error.message().to_owned())
    }

    fn start_mirror(
        &self,
        source: &str,
        targets: &[String],
        stop: Arc<AtomicBool>,
        events: Sender<UiEvent>,
    ) -> Result<(), String> {
        let (core_tx, core_rx) = mpsc::channel();
        let session = windows_audio::start_mirror(source, targets, stop.clone(), Some(core_tx))
            .map_err(|error| error.message().to_owned())?;

        loop {
            match core_rx.recv_timeout(Duration::from_millis(150)) {
                Ok(MirrorEvent::Started { source, targets }) => {
                    let _ = events.send(UiEvent::Started { source, targets });
                }
                Ok(MirrorEvent::Fatal { error }) => {
                    let _ = events.send(UiEvent::Error {
                        device_lost: error.kind() == AudioErrorKind::DeviceInvalidated,
                        message: error.message().to_owned(),
                    });
                    break;
                }
                Ok(MirrorEvent::Stopped) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = events.send(UiEvent::Stats(session.stats()));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let _ = session.stop_and_wait();
        Ok(())
    }
}

/// Spawns a mirror session on a worker thread. Public for deterministic tests
/// with a non-interactive [`CoreApi`] mock.
pub fn spawn_session(
    core: Arc<dyn CoreApi>,
    source: String,
    targets: Vec<String>,
    stop: Arc<AtomicBool>,
    events: Sender<UiEvent>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("audiomix-win-session".to_owned())
        .spawn(move || {
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                core.start_mirror(&source, &targets, stop.clone(), events.clone())
            }))
            .unwrap_or_else(|_| {
                stop.store(true, Ordering::SeqCst);
                Err("Falha interna na sessao de audio; os recursos foram encerrados.".into())
            });
            if let Err(message) = result {
                let _ = events.send(UiEvent::Error {
                    message,
                    device_lost: false,
                });
            }
            // Liveness guarantee: even a core implementation that forgets to
            // send `Stopped` releases the UI instead of leaving it stuck.
            let _ = events.send(UiEvent::Stopped);
        })
}

fn spawn_enumeration(core: Arc<dyn CoreApi>, events: Sender<UiEvent>) {
    let fallback = events.clone();
    let spawned = thread::Builder::new()
        .name("audiomix-win-enum".to_owned())
        .spawn(move || {
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| core.list_endpoints()))
                .unwrap_or_else(|_| {
                    Err(
                        "Falha interna ao enumerar saidas; pressione r para tentar novamente."
                            .into(),
                    )
                });
            let _ = events.send(UiEvent::Endpoints(result));
        });
    if let Err(error) = spawned {
        let _ = fallback.send(UiEvent::Endpoints(Err(format!(
            "nao foi possivel criar a thread de enumeracao: {error}"
        ))));
    }
}

// ---------------------------------------------------------------------------
// Pure state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Ready,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl Phase {
    fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Loading => "Carregando saidas...",
            Self::Ready => "Pronto",
            Self::Starting => "Iniciando (aguardando WASAPI)...",
            Self::Running => "Espelhando",
            Self::Stopping => "Parando...",
            Self::Failed => "Falhou - veja ? para o erro completo",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    Confirm { scroll: u16 },
    Help { scroll: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    Start {
        source: String,
        targets: Vec<String>,
    },
    Stop,
    Refresh,
    Quit,
}

/// Concrete source/target ids resolved from CLI selectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub source_id: String,
    pub target_ids: Vec<String>,
}

impl Preset {
    pub fn new(source_id: impl Into<String>, target_ids: Vec<String>) -> Self {
        Self {
            source_id: source_id.into(),
            target_ids,
        }
    }

    /// Resolves ids or exact names. Names are matched case-insensitively only
    /// when unambiguous.
    pub fn resolve(
        endpoints: &[Endpoint],
        source_selector: &str,
        target_selectors: &[String],
    ) -> Result<Self> {
        let source = resolve_selector(endpoints, source_selector, "fonte")?;
        if !source.is_default {
            bail!(
                "a fonte selecionada ({:?}) nao e a saida padrao atual; escolha a fonte padrao em Configuracoes > Sistema > Som do Windows",
                source.name
            );
        }
        if target_selectors.is_empty() {
            bail!("selecione ao menos uma saida secundaria; o espelhamento precisa de 2 saidas no total");
        }

        let mut ids: Vec<String> = Vec::new();
        for selector in target_selectors {
            let endpoint = resolve_selector(endpoints, selector, "alvo")?;
            if endpoint.id == source.id {
                bail!(
                    "a saida {:?} e a fonte e nao pode ser um alvo",
                    endpoint.name
                );
            }
            if endpoint.is_default {
                bail!(
                    "a saida {:?} e a padrao; alvos devem ser somente saidas secundarias",
                    endpoint.name
                );
            }
            if ids.iter().any(|id| id == &endpoint.id) {
                bail!("o alvo {:?} foi selecionado mais de uma vez", endpoint.name);
            }
            ids.push(endpoint.id.clone());
        }

        if endpoints.len() < 2 {
            bail!("o espelhamento precisa de ao menos 2 saidas ativas");
        }
        Ok(Self {
            source_id: source.id.clone(),
            target_ids: ids,
        })
    }
}

fn resolve_selector<'a>(
    endpoints: &'a [Endpoint],
    selector: &str,
    role: &str,
) -> Result<&'a Endpoint> {
    if let Some(endpoint) = endpoints.iter().find(|endpoint| endpoint.id == selector) {
        return Ok(endpoint);
    }
    let case_insensitive: Vec<&Endpoint> = endpoints
        .iter()
        .filter(|endpoint| endpoint.name.eq_ignore_ascii_case(selector))
        .collect();
    match case_insensitive.as_slice() {
        [endpoint] => Ok(endpoint),
        [] => bail!(
            "{role} {selector:?} nao encontrada na lista de saidas ativas; use `audiomix list`"
        ),
        _ => bail!("{role} {selector:?} e ambigua; use o id nativo em `audiomix list`"),
    }
}

/// Pure application state. No terminal or audio handle lives here.
pub struct App {
    version: String,
    endpoints: Vec<Endpoint>,
    source_id: Option<String>,
    marked: BTreeSet<String>,
    cursor: usize,
    phase: Phase,
    session_in_flight: bool,
    modal: Option<Modal>,
    stats: MirrorStats,
    history: Vec<String>,
    notice: Option<String>,
    refresh_in_flight: bool,
    quit_pending: bool,
    should_quit: bool,
    stop: Arc<AtomicBool>,
    preset: Option<Preset>,
    explicit_run: bool,
    auto_start: Option<UiAction>,
}

impl App {
    pub fn new(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            endpoints: Vec::new(),
            source_id: None,
            marked: BTreeSet::new(),
            cursor: 0,
            phase: Phase::Loading,
            session_in_flight: false,
            modal: None,
            stats: MirrorStats::default(),
            history: Vec::new(),
            notice: None,
            refresh_in_flight: true,
            quit_pending: false,
            should_quit: false,
            stop: Arc::new(AtomicBool::new(false)),
            preset: None,
            explicit_run: false,
            auto_start: None,
        }
    }

    pub fn with_preset(mut self, preset: Preset, explicit_run: bool) -> Self {
        self.preset = Some(preset);
        self.explicit_run = explicit_run;
        self
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }

    pub fn source_id(&self) -> Option<&str> {
        self.source_id.as_deref()
    }

    pub fn marked_targets(&self) -> Vec<String> {
        self.endpoints
            .iter()
            .filter(|endpoint| self.marked.contains(&endpoint.id))
            .map(|endpoint| endpoint.id.clone())
            .collect()
    }

    pub fn is_marked(&self, id: &str) -> bool {
        self.marked.contains(id)
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    pub fn stats(&self) -> &MirrorStats {
        &self.stats
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn quit_pending(&self) -> bool {
        self.quit_pending
    }

    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    pub fn take_auto_start(&mut self) -> Option<UiAction> {
        self.auto_start.take()
    }

    fn secondary(&self) -> Vec<&Endpoint> {
        self.endpoints
            .iter()
            .filter(|endpoint| Some(endpoint.id.as_str()) != self.source_id.as_deref())
            .collect()
    }

    /// Applies a fresh enumeration. Marks survive only when the native endpoint
    /// set and the default source are unchanged; any disconnect or remap clears
    /// them. Read-only by contract.
    pub fn apply_endpoints(&mut self, mut endpoints: Vec<Endpoint>) {
        self.refresh_in_flight = false;

        let mut seen = BTreeSet::new();
        let before = endpoints.len();
        endpoints.retain(|endpoint| seen.insert(endpoint.id.clone()));
        if endpoints.len() != before {
            self.push_history(
                "A lista continha ids nativos duplicados; duplicatas foram ignoradas.",
            );
        }

        let old_default = self.source_id.clone();
        let old_ids: BTreeSet<String> = self
            .endpoints
            .iter()
            .map(|endpoint| endpoint.id.clone())
            .collect();
        let new_default = endpoints
            .iter()
            .find(|endpoint| endpoint.is_default)
            .map(|endpoint| endpoint.id.clone());
        let new_ids: BTreeSet<String> = endpoints
            .iter()
            .map(|endpoint| endpoint.id.clone())
            .collect();

        if !self.endpoints.is_empty() && (old_ids != new_ids || old_default != new_default) {
            if !self.marked.is_empty() {
                self.push_history(
                    "Saidas desconectadas ou remapeadas: as marcacoes foram limpas; revise antes de iniciar.",
                );
            }
            self.marked.clear();
        }

        self.endpoints = endpoints;
        self.source_id = new_default.clone();
        self.marked.retain(|id| {
            Some(id.as_str()) != new_default.as_deref()
                && self.endpoints.iter().any(|endpoint| &endpoint.id == id)
        });
        self.cursor = self.cursor.min(self.secondary().len().saturating_sub(1));

        if !self.session_in_flight && self.phase != Phase::Failed {
            self.phase = Phase::Ready;
        }
        if self.source_id.is_none() {
            self.notice = Some(
                "Nenhuma saida padrao: escolha uma em Configuracoes > Sistema > Som do Windows."
                    .to_owned(),
            );
        } else if self.endpoints.len() < 2 {
            self.notice =
                Some("Menos de duas saidas ativas: conecte outra saida para espelhar.".to_owned());
        } else if self
            .notice
            .as_deref()
            .map(|n| n.starts_with("Nenhuma saida padrao"))
            .unwrap_or(false)
        {
            self.notice = None;
        }

        if let Some(preset) = self.preset.take() {
            self.begin_preset(preset);
        }
    }

    fn begin_preset(&mut self, preset: Preset) {
        let source_ok = self
            .source_id
            .as_deref()
            .map(|id| id == preset.source_id)
            .unwrap_or(false);
        if !source_ok {
            let message = "A fonte pedida nao e mais a saida padrao atual; escolha a fonte em Configuracoes > Sistema > Som do Windows.";
            self.push_history(message);
            self.notice = Some(message.to_owned());
            self.phase = Phase::Failed;
            return;
        }
        let valid: Vec<String> = preset
            .target_ids
            .iter()
            .filter(|id| {
                Some(id.as_str()) != self.source_id.as_deref()
                    && self
                        .endpoints
                        .iter()
                        .any(|endpoint| &endpoint.id == *id && !endpoint.is_default)
            })
            .cloned()
            .collect();
        if valid.len() != preset.target_ids.len() || valid.is_empty() {
            let message = "Um ou mais alvos pedidos nao estao mais disponiveis; a lista mudou.";
            self.push_history(message);
            self.notice = Some(message.to_owned());
            self.phase = Phase::Failed;
            return;
        }
        self.marked = valid.iter().cloned().collect();
        if self.explicit_run {
            match self.validate_start() {
                Ok((source, targets)) => {
                    self.stop = Arc::new(AtomicBool::new(false));
                    self.session_in_flight = true;
                    self.phase = Phase::Starting;
                    self.auto_start = Some(UiAction::Start { source, targets });
                }
                Err(message) => {
                    self.push_history(&message);
                    self.notice = Some(message);
                    self.phase = Phase::Failed;
                }
            }
        }
    }

    pub fn on_core_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Endpoints(result) => match result {
                Ok(endpoints) => self.apply_endpoints(endpoints),
                Err(message) => {
                    self.refresh_in_flight = false;
                    self.phase = Phase::Failed;
                    self.notice =
                        Some("Falha ao listar saidas; pressione r para tentar de novo.".to_owned());
                    self.push_history(&message);
                }
            },
            UiEvent::Started { source, targets } => {
                self.session_in_flight = true;
                self.source_id = if source.is_default {
                    Some(source.id)
                } else {
                    self.source_id.clone()
                };
                self.marked = targets.iter().map(|target| target.id.clone()).collect();
                self.stats = MirrorStats {
                    running: true,
                    targets: targets
                        .iter()
                        .map(|target| crate::windows_audio::TargetHealth {
                            id: target.id.clone(),
                            name: target.name.clone(),
                            frames_rendered: 0,
                            overflow_frames: 0,
                            underrun_frames: 0,
                            last_error: None,
                        })
                        .collect(),
                    ..MirrorStats::default()
                };
                if self.quit_pending {
                    self.phase = Phase::Stopping;
                } else {
                    self.phase = Phase::Running;
                    self.notice = Some(
                        "Espelhamento ativo. O dispositivo padrao do Windows NAO foi alterado."
                            .to_owned(),
                    );
                }
            }
            UiEvent::Stats(stats) => {
                self.stats = stats;
            }
            UiEvent::Stopped => {
                self.session_in_flight = false;
                let was_active = self.phase.is_active();
                self.stats.running = false;
                if self.quit_pending {
                    self.phase = Phase::Stopping;
                    self.should_quit = true;
                } else {
                    if self.phase != Phase::Failed {
                        self.phase = Phase::Ready;
                    }
                    if was_active {
                        self.notice = Some("Espelhamento encerrado.".to_owned());
                    }
                }
            }
            UiEvent::Error {
                message,
                device_lost,
            } => {
                self.stats.running = false;
                self.phase = Phase::Failed;
                self.notice = Some(
                    "O espelhamento falhou; o aplicativo continua ativo. Pressione ? para o erro completo."
                        .to_owned(),
                );
                self.push_history(&message);
                if device_lost {
                    self.marked.clear();
                    self.push_history(
                        "Saida removida ou desativada: as marcacoes foram limpas; use r para reler.",
                    );
                }
                if self.session_in_flight {
                    self.stop.store(true, Ordering::SeqCst);
                }
                if self.quit_pending && !self.session_in_flight {
                    self.should_quit = true;
                }
            }
        }
    }

    fn push_history(&mut self, message: &str) {
        const LIMIT: usize = 100;
        self.history.push(message.to_owned());
        if self.history.len() > LIMIT {
            self.history.remove(0);
        }
    }

    fn validate_start(&self) -> Result<(String, Vec<String>), String> {
        let source = self.source_id.clone().ok_or_else(|| {
            "Nenhuma saida padrao: escolha uma em Configuracoes > Sistema > Som do Windows."
                .to_owned()
        })?;
        let source_endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == source)
            .ok_or_else(|| "A saida padrao nao esta mais na lista; pressione r.".to_owned())?;
        if !source_endpoint.is_default {
            return Err(
                "A fonte travada nao e mais a saida padrao do Windows; pressione r e confira."
                    .to_owned(),
            );
        }
        if self.endpoints.len() < 2 {
            return Err("O espelhamento precisa de ao menos 2 saidas ativas.".to_owned());
        }
        let targets = self.marked_targets();
        if targets.is_empty() {
            return Err(
                "Marque ao menos uma saida secundaria com Espaco antes de revisar.".to_owned(),
            );
        }
        for target in &targets {
            if *target == source {
                return Err("A fonte nao pode ser um alvo.".to_owned());
            }
            match self
                .endpoints
                .iter()
                .find(|endpoint| &endpoint.id == target)
            {
                Some(endpoint) if !endpoint.is_default => {}
                Some(_) => {
                    return Err("Alvos devem ser somente saidas secundarias.".to_owned());
                }
                None => return Err("Um alvo marcado nao esta mais disponivel; use r.".to_owned()),
            }
        }
        Ok((source, targets))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<UiAction> {
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'));
        if ctrl_c {
            return Some(self.request_quit());
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        if let Some(modal) = self.modal.clone() {
            return self.handle_modal_key(modal, key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => Some(self.request_quit()),
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help { scroll: 0 });
                None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_cursor(1);
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_cursor(-1);
                None
            }
            KeyCode::Char(' ') => {
                self.toggle_cursor();
                None
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.begin_confirm();
                None
            }
            KeyCode::Char('x') | KeyCode::Char('X') => self.request_stop(),
            KeyCode::Char('r') | KeyCode::Char('R') => self.request_refresh(),
            KeyCode::Esc => {
                self.modal = None;
                None
            }
            _ => None,
        }
    }

    fn handle_modal_key(&mut self, modal: Modal, key: KeyEvent) -> Option<UiAction> {
        match modal {
            Modal::Confirm { scroll } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => match self.validate_start() {
                    Ok((source, targets)) => {
                        self.stop = Arc::new(AtomicBool::new(false));
                        self.session_in_flight = true;
                        self.modal = None;
                        self.phase = Phase::Starting;
                        self.quit_pending = false;
                        self.notice = Some(
                                "Iniciando: o estado muda para Espelhando somente depois que os clientes WASAPI iniciarem."
                                    .to_owned(),
                            );
                        Some(UiAction::Start { source, targets })
                    }
                    Err(message) => {
                        self.modal = None;
                        self.push_history(&message);
                        self.notice = Some(message);
                        None
                    }
                },
                KeyCode::Esc
                | KeyCode::Char('n')
                | KeyCode::Char('N')
                | KeyCode::Char('q')
                | KeyCode::Char('Q') => {
                    self.modal = None;
                    None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.modal = Some(Modal::Confirm {
                        scroll: scroll.saturating_add(1),
                    });
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.modal = Some(Modal::Confirm {
                        scroll: scroll.saturating_sub(1),
                    });
                    None
                }
                KeyCode::PageDown => {
                    self.modal = Some(Modal::Confirm {
                        scroll: scroll.saturating_add(5),
                    });
                    None
                }
                KeyCode::PageUp => {
                    self.modal = Some(Modal::Confirm {
                        scroll: scroll.saturating_sub(5),
                    });
                    None
                }
                _ => None,
            },
            Modal::Help { scroll } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                    self.modal = None;
                    None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.modal = Some(Modal::Help {
                        scroll: scroll.saturating_add(1),
                    });
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.modal = Some(Modal::Help {
                        scroll: scroll.saturating_sub(1),
                    });
                    None
                }
                KeyCode::PageDown => {
                    self.modal = Some(Modal::Help {
                        scroll: scroll.saturating_add(8),
                    });
                    None
                }
                KeyCode::PageUp => {
                    self.modal = Some(Modal::Help {
                        scroll: scroll.saturating_sub(8),
                    });
                    None
                }
                _ => None,
            },
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.secondary().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let current = self.cursor as isize;
        self.cursor = (current + delta).clamp(0, len as isize - 1) as usize;
    }

    fn toggle_cursor(&mut self) {
        if self.session_in_flight {
            self.notice = Some("Pare o espelhamento (x) antes de alterar alvos.".to_owned());
            return;
        }
        if self.refresh_in_flight {
            self.notice = Some("Aguarde a releitura das saidas.".to_owned());
            return;
        }
        let Some(endpoint) = self.secondary().get(self.cursor).cloned().cloned() else {
            self.notice = Some("Nao ha saida secundaria para marcar.".to_owned());
            return;
        };
        if endpoint.is_default || Some(endpoint.id.as_str()) == self.source_id.as_deref() {
            self.notice = Some("A saida padrao e a fonte travada e nao pode ser alvo.".to_owned());
            return;
        }
        if !self.marked.remove(&endpoint.id) {
            self.marked.insert(endpoint.id.clone());
        }
        self.notice = None;
    }

    fn begin_confirm(&mut self) {
        if self.session_in_flight {
            self.notice = Some("O espelhamento ja esta ativo; use x para parar.".to_owned());
            return;
        }
        if self.refresh_in_flight {
            self.notice = Some("Aguarde a releitura das saidas.".to_owned());
            return;
        }
        match self.validate_start() {
            Ok(_) => self.modal = Some(Modal::Confirm { scroll: 0 }),
            Err(message) => {
                self.push_history(&message);
                self.notice = Some(message);
            }
        }
    }

    fn request_stop(&mut self) -> Option<UiAction> {
        if self.session_in_flight && self.phase != Phase::Stopping {
            self.phase = Phase::Stopping;
            self.stop.store(true, Ordering::SeqCst);
            self.notice =
                Some("Parando o espelhamento e liberando os clientes WASAPI...".to_owned());
            Some(UiAction::Stop)
        } else if self.phase == Phase::Stopping {
            None
        } else {
            self.notice = Some("Nenhum espelhamento ativo; x nao tem efeito.".to_owned());
            None
        }
    }

    fn request_refresh(&mut self) -> Option<UiAction> {
        if self.session_in_flight {
            self.notice = Some("Pare o espelhamento (x) antes de reler as saidas.".to_owned());
            return None;
        }
        if self.refresh_in_flight {
            self.notice = Some("A releitura ja esta em andamento.".to_owned());
            return None;
        }
        self.refresh_in_flight = true;
        self.notice = Some("Reler as saidas (somente leitura)...".to_owned());
        Some(UiAction::Refresh)
    }

    fn request_quit(&mut self) -> UiAction {
        if self.session_in_flight {
            self.quit_pending = true;
            self.phase = Phase::Stopping;
            self.stop.store(true, Ordering::SeqCst);
            UiAction::Stop
        } else {
            self.should_quit = true;
            UiAction::Quit
        }
    }

    pub fn handle_terminal_key(
        &mut self,
        key: KeyEvent,
        width: u16,
        height: u16,
    ) -> Option<UiAction> {
        if width < 55 || height < 18 {
            if matches!(key.code, KeyCode::Char('q'))
                || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
            {
                return Some(self.request_quit());
            }
            if key.code == KeyCode::Esc {
                self.modal = None;
            }
            return None;
        }
        self.handle_key(key)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(NAVY)), area);

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(area);

    draw_header(frame, rows[0], app);
    draw_source(frame, rows[1], app);
    draw_body(frame, rows[2], app);
    draw_status(frame, rows[3], app);
    draw_footer(frame, rows[4], area.width);

    if app.modal.is_some() {
        draw_modal(frame, area, app);
    }
}

fn draw_too_small(frame: &mut Frame<'_>, area: Rect) {
    let text = format!(
        "Terminal muito pequeno\nRedimensione para pelo menos {MIN_WIDTH} x {MIN_HEIGHT}\nTamanho atual: {} x {}\nq ou Ctrl-C para sair",
        area.width, area.height
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(accent())
        .style(bg_panel());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(TEXT))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let phase_style = match app.phase {
        Phase::Running => Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        Phase::Failed => Style::default().fg(RED).add_modifier(Modifier::BOLD),
        Phase::Starting | Phase::Stopping => amber(),
        _ => accent(),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("AUDIOMIX", amber()),
            Span::styled("  Windows / WASAPI", accent()),
            Span::styled(format!("   v{}", app.version()), muted()),
        ]),
        Line::from(Span::styled(
            "Fonte travada: saida padrao atual   Alvos: somente saidas secundarias",
            muted(),
        )),
        Line::from(vec![
            Span::styled("Estado: ", muted()),
            Span::styled(app.phase.label(), phase_style),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(NAVY).fg(TEXT)),
        area,
    );
}

fn draw_source(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let line = match app
        .source_id
        .as_deref()
        .and_then(|id| app.endpoints.iter().find(|endpoint| endpoint.id == id))
    {
        Some(endpoint) => Line::from(vec![
            Span::styled("Origem [travada] ", amber()),
            Span::styled(
                clip(&endpoint.name, area.width.saturating_sub(24)),
                Style::default().fg(TEXT),
            ),
            Span::styled(
                format!("  id={}", clip(&endpoint.id, area.width.saturating_sub(12))),
                muted(),
            ),
        ]),
        None => Line::from(vec![
            Span::styled("Origem [indisponivel] ", Style::default().fg(RED)),
            Span::styled(
                clip(
                    "nenhuma saida padrao; escolha uma em Configuracoes do Windows",
                    area.width.saturating_sub(24),
                ),
                muted(),
            ),
        ]),
    };
    frame.render_widget(Paragraph::new(line).style(Style::default().bg(NAVY)), area);
}

fn draw_body(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let panes = if area.width >= 80 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area)
    };

    draw_list(frame, panes[0], app);
    draw_details(frame, panes[1], app);
}

fn draw_list(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = format!(" Saidas de audio ({} marcada(s)) ", app.marked.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(TEAL))
        .style(bg_panel())
        .title(Span::styled(title, accent()));
    let inner_width = block.inner(area).width;
    let mut items = Vec::new();
    let mut secondary_index = 0usize;

    for endpoint in &app.endpoints {
        let is_source = Some(endpoint.id.as_str()) == app.source_id.as_deref();
        if is_source {
            items.push(ListItem::new(endpoint_line(
                endpoint,
                inner_width,
                true,
                false,
                false,
            )));
            continue;
        }
        let marked = app.marked.contains(&endpoint.id);
        let cursor = secondary_index == app.cursor && !app.phase.is_active();
        items.push(ListItem::new(endpoint_line(
            endpoint,
            inner_width,
            false,
            marked,
            cursor,
        )));
        secondary_index += 1;
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "Nenhuma saida de renderizacao ativa.",
            muted(),
        ))));
    }

    let list = List::new(items).block(block).style(bg_panel());
    frame.render_widget(list, area);
}

fn endpoint_line(
    endpoint: &Endpoint,
    width: u16,
    is_source: bool,
    marked: bool,
    cursor: bool,
) -> Line<'static> {
    let prefix = if cursor { "> " } else { "  " };
    let tag = if is_source {
        "[origem] "
    } else if marked {
        "[x] "
    } else {
        "[ ] "
    };
    let default_tag = if endpoint.is_default {
        "  [padrao]"
    } else {
        ""
    };
    let available = width
        .saturating_sub((prefix.len() + tag.len() + default_tag.len()) as u16)
        .saturating_sub(10);
    let name = clip(&endpoint.name, available.max(6));
    let id_budget = width
        .saturating_sub(
            (prefix.len() + tag.len() + default_tag.len() + name.chars().count()) as u16,
        )
        .saturating_sub(1);
    let id = clip(&endpoint.id, id_budget.max(4));

    let spans = vec![
        Span::styled(prefix.to_owned(), if cursor { amber() } else { muted() }),
        Span::styled(
            tag.to_owned(),
            if is_source {
                amber()
            } else if marked {
                accent()
            } else {
                muted()
            },
        ),
        Span::styled(
            name,
            if is_source {
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            },
        ),
        Span::styled(default_tag.to_owned(), amber()),
        Span::styled(format!(" {id}"), muted()),
    ];
    Line::from(spans)
}

fn draw_details(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(TEAL))
        .style(bg_panel())
        .title(Span::styled(" Detalhes e roteamento ", accent()));
    let inner = block.inner(area);
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(Span::styled("Fonte (travada)", amber())));
    match app
        .source_id
        .as_deref()
        .and_then(|id| app.endpoints.iter().find(|endpoint| endpoint.id == id))
    {
        Some(source) => {
            lines.push(Line::from(Span::styled(
                sanitize_display(&source.name),
                Style::default().fg(TEXT),
            )));
            lines.push(Line::from(Span::styled(
                format!("  id={}", sanitize_display(&source.id)),
                muted(),
            )));
        }
        None => lines.push(Line::from(Span::styled(
            "Escolha uma saida padrao nas Configuracoes do Windows.",
            Style::default().fg(RED),
        ))),
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Alvos secundarios marcados ({})", app.marked.len()),
        amber(),
    )));
    let targets = app.marked_targets();
    if targets.is_empty() {
        lines.push(Line::from(Span::styled(
            "Nenhum. Use j/k e Espaco para marcar.",
            muted(),
        )));
    } else {
        for id in targets {
            if let Some(target) = app.endpoints.iter().find(|endpoint| endpoint.id == id) {
                lines.push(Line::from(vec![
                    Span::styled("- ", accent()),
                    Span::styled(sanitize_display(&target.name), Style::default().fg(TEXT)),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("  id={}", sanitize_display(&target.id)),
                    muted(),
                )));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Roteamento", amber())));
    lines.push(Line::from(Span::styled(
        "Captura SOMENTE a saida padrao atual e renderiza SOMENTE nos alvos marcados.",
        Style::default().fg(TEXT),
    )));
    lines.push(Line::from(Span::styled(
        "O padrao do Windows NAO e alterado; para trocar a fonte use Configuracoes > Sistema > Som.",
        muted(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Nao suportado nesta versao do Windows",
        amber(),
    )));
    lines.push(Line::from(Span::styled(
        "- definir/trocar o dispositivo padrao",
        muted(),
    )));
    lines.push(Line::from(Span::styled("- perfis Bluetooth/A2DP", muted())));
    lines.push(Line::from(Span::styled(
        "- grupos, pactl ou loopback do backend Linux",
        muted(),
    )));

    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines)
            .style(bg_panel())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let detail = if app.phase == Phase::Running {
        format!(
            "capturado {} | renderizado {} | quedas {} | underrun {}",
            app.stats.frames_captured,
            app.stats.frames_rendered,
            app.stats.overflow_frames,
            app.stats.underrun_frames
        )
    } else if let Some(notice) = &app.notice {
        sanitize_display(notice)
    } else {
        "Espaco marca saidas secundarias; c revisa a rota antes de iniciar.".to_owned()
    };
    let line = Line::from(vec![
        Span::styled(
            format!("{} | ", app.phase.label()),
            Style::default().fg(TEXT),
        ),
        Span::styled(detail, muted()),
    ]);
    frame.render_widget(
        Paragraph::new(line)
            .style(Style::default().bg(PANEL_ALT))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, width: u16) {
    let (first, second) = if width >= 76 {
        (
            "j/k mover   Espaco marcar   c revisar   y iniciar na revisao   x parar",
            "r atualizar (somente leitura)   ? ajuda   q/Ctrl-C sair",
        )
    } else {
        (
            "j/k mover  Espaco marcar  c revisar  x parar",
            "y so na revisao  r atualizar  ? ajuda  q sair",
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(first.to_owned(), muted())),
            Line::from(Span::styled(second.to_owned(), muted())),
        ])
        .style(bg_panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn modal_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(6).clamp(48, 86).min(area.width);
    let height = area.height.saturating_sub(4).clamp(10, 22).min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn modal_footer(modal: &Modal) -> Vec<Line<'static>> {
    match modal {
        Modal::Confirm { .. } => vec![
            Line::from(vec![
                Span::styled("y iniciar", amber()),
                Span::styled("   Esc/n cancelar   PgUp/PgDn rolar", muted()),
            ]),
            Line::from(Span::styled(
                "Nada e alterado antes de y; o padrao do Windows nao muda.",
                muted(),
            )),
        ],
        Modal::Help { .. } => vec![
            Line::from(vec![
                Span::styled("j/k, PgUp/PgDn", amber()),
                Span::styled(" rolar   Esc/q/? fechar", muted()),
            ]),
            Line::from(Span::styled(
                "Ctrl-C encerra o aplicativo e para o espelhamento.",
                muted(),
            )),
        ],
    }
}

pub fn clamp_scroll(app: &mut App, area: Rect) {
    if area.width < 55 || area.height < 18 {
        return;
    }
    let Some(modal) = app.modal.clone() else {
        return;
    };
    let inner = Block::bordered().inner(modal_rect(area));
    let footer = Paragraph::new(modal_footer(&modal)).wrap(Wrap { trim: false });
    let height = inner
        .height
        .saturating_sub(footer.line_count(inner.width) as u16);
    let body = match modal {
        Modal::Confirm { .. } => confirm_lines(app),
        Modal::Help { .. } => help_lines(app),
    };
    let max_scroll = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .line_count(inner.width)
        .saturating_sub(height as usize)
        .min(u16::MAX as usize) as u16;
    match &mut app.modal {
        Some(Modal::Confirm { scroll }) | Some(Modal::Help { scroll }) => {
            *scroll = (*scroll).min(max_scroll)
        }
        None => {}
    }
}

fn draw_modal(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(modal) = &app.modal else {
        return;
    };
    let rect = modal_rect(area);
    frame.render_widget(Clear, rect);

    let (title, scroll, body) = match modal {
        Modal::Confirm { scroll } => (" Revisar espelhamento ", *scroll, confirm_lines(app)),
        Modal::Help { scroll } => (" Ajuda ", *scroll, help_lines(app)),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .style(Style::default().bg(PANEL_ALT).fg(TEXT))
        .title(Span::styled(title, amber()));
    let inner = block.inner(rect);
    let footer = Paragraph::new(modal_footer(modal))
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(PANEL_ALT));
    let footer_height = (footer.line_count(inner.width) as u16).min(inner.height.saturating_sub(1));
    let body_area = Rect {
        height: inner.height.saturating_sub(footer_height),
        ..inner
    };
    let footer_area = Rect {
        y: inner.y + inner.height.saturating_sub(footer_height),
        height: footer_height,
        ..inner
    };

    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(body)
            .style(Style::default().bg(PANEL_ALT).fg(TEXT))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        body_area,
    );

    frame.render_widget(footer, footer_area);
}

fn confirm_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Revise a fonte e os alvos completos", amber())),
        Line::from(""),
        Line::from(Span::styled("Fonte (saida padrao atual)", amber())),
    ];
    if let Some(source) = app
        .source_id
        .as_deref()
        .and_then(|id| app.endpoints.iter().find(|endpoint| endpoint.id == id))
    {
        lines.push(Line::from(Span::styled(
            sanitize_display(&source.name),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("ID: {}", sanitize_display(&source.id)),
            muted(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Nenhuma saida padrao disponivel.",
            Style::default().fg(RED),
        )));
    }
    lines.push(Line::from(""));
    let targets = app.marked_targets();
    lines.push(Line::from(Span::styled(
        format!("Alvos secundarios ({})", targets.len()),
        amber(),
    )));
    for id in &targets {
        if let Some(target) = app.endpoints.iter().find(|endpoint| &endpoint.id == id) {
            lines.push(Line::from(vec![
                Span::styled("- ", accent()),
                Span::styled(sanitize_display(&target.name), Style::default().fg(TEXT)),
            ]));
            lines.push(Line::from(Span::styled(
                format!("  ID: {}", sanitize_display(&target.id)),
                muted(),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Ao iniciar", amber())));
    lines.push(Line::from("  - captura SOMENTE a saida padrao atual"));
    lines.push(Line::from("  - renderiza SOMENTE nos alvos marcados"));
    lines.push(Line::from("  - NAO altera o dispositivo padrao do Windows"));
    lines.push(Line::from("  - NAO altera perfis Bluetooth/A2DP"));
    lines.push(Line::from("  - NAO usa grupos, pactl ou loopback do Linux"));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "A sessao existe somente neste processo e para ao sair.",
        muted(),
    )));
    lines
}

fn help_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Teclas", amber())),
        Line::from("j/k, setas   mover a selecao de saidas secundarias"),
        Line::from("Espaco       marcar/desmarcar uma saida secundaria"),
        Line::from("c            abrir a revisao explicita de fonte e alvos"),
        Line::from("y            iniciar SOMENTE na tela de revisao"),
        Line::from("x            parar o espelhamento atual (sem volume global)"),
        Line::from("r            reler as saidas (somente leitura)"),
        Line::from("?            abrir/fechar esta ajuda"),
        Line::from("q, Ctrl-C    sair; para o espelhamento e preserva o terminal"),
        Line::from(""),
        Line::from(Span::styled("O que o AudioMix Windows NAO faz", amber())),
        Line::from("- nao define nem troca o dispositivo padrao"),
        Line::from("- nao altera perfis ou pares Bluetooth/A2DP"),
        Line::from("- nao cria grupos, nem usa pactl/loopback do Linux"),
        Line::from("Escolha a fonte padrao em Configuracoes > Sistema > Som do Windows."),
        Line::from(""),
        Line::from(Span::styled("Historico recente", amber())),
    ];
    if app.history.is_empty() {
        lines.push(Line::from(Span::styled("(vazio)", muted())));
    } else {
        for entry in app.history.iter().rev().take(20) {
            lines.push(Line::from(Span::styled(
                sanitize_display(entry),
                Style::default().fg(TEXT),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Limite desta versao: sem controle de volume, sem troca de padrao e sem perfis.",
        muted(),
    )));
    lines
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut out = stdout();
    let _ = execute!(out, LeaveAlternateScreen, cursor::Show);
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("ativar modo raw do terminal")?;
        // Construct the guard before the fallible alternate-screen call so a
        // partial initialisation cannot leave the terminal in raw mode.
        let guard = Self;
        execute!(stdout(), EnterAlternateScreen, cursor::Hide)
            .context("entrar na tela alternativa")?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn run_tui(core: Arc<dyn CoreApi>, preset: Option<Preset>, explicit_run: bool) -> Result<()> {
    let ui_thread = thread::current().id();
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if thread::current().id() == ui_thread {
            restore_terminal();
            previous_hook(info);
        }
    }));

    let guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("inicializar terminal")?;

    let (tx, rx) = mpsc::channel::<UiEvent>();
    let mut app = App::new(env!("CARGO_PKG_VERSION"));
    if let Some(preset) = preset {
        app = app.with_preset(preset, explicit_run);
    }
    spawn_enumeration(core.clone(), tx.clone());

    let result = event_loop(&mut terminal, &mut app, &rx, &tx, core);
    restore_terminal();
    drop(guard);
    result
}

/// Runs the same terminal lifecycle with a supplied audio adapter. This seam
/// permits PTY acceptance checks without opening any real audio endpoint.
pub fn run_with_core(core: Arc<dyn CoreApi>) -> Result<()> {
    run_tui(core, None, false)
}

struct SessionTask {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for SessionTask {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn launch_session(
    app: &mut App,
    core: Arc<dyn CoreApi>,
    source: String,
    targets: Vec<String>,
    tx: &Sender<UiEvent>,
) -> Option<SessionTask> {
    let stop = app.stop_flag();
    match spawn_session(core, source, targets, stop.clone(), tx.clone()) {
        Ok(handle) => Some(SessionTask {
            stop,
            worker: Some(handle),
        }),
        Err(error) => {
            app.on_core_event(UiEvent::Error {
                message: format!("falha ao criar thread de espelhamento: {error}"),
                device_lost: false,
            });
            app.on_core_event(UiEvent::Stopped);
            None
        }
    }
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    rx: &mpsc::Receiver<UiEvent>,
    tx: &Sender<UiEvent>,
    core: Arc<dyn CoreApi>,
) -> Result<()> {
    let mut session: Option<SessionTask> = None;
    loop {
        let mut stopped = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, UiEvent::Stopped) {
                stopped = true;
            }
            app.on_core_event(event);
        }
        if stopped {
            drop(session.take());
        }

        if let Some(UiAction::Start { source, targets }) = app.take_auto_start() {
            if session.is_none() {
                session = launch_session(app, core.clone(), source, targets, tx);
            }
        }

        if app.should_quit() {
            break;
        }

        terminal
            .draw(|frame| {
                clamp_scroll(app, frame.area());
                draw(frame, app);
            })
            .context("desenhar a interface do Windows")?;

        if event::poll(Duration::from_millis(80)).context("consultar eventos do terminal")? {
            match event::read().context("ler evento do terminal")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let size = terminal.size().context("ler tamanho do terminal")?;
                    if let Some(action) = app.handle_terminal_key(key, size.width, size.height) {
                        match action {
                            UiAction::Start { source, targets } => {
                                if session.is_none() {
                                    session =
                                        launch_session(app, core.clone(), source, targets, tx);
                                }
                            }
                            UiAction::Stop => {}
                            UiAction::Refresh => spawn_enumeration(core.clone(), tx.clone()),
                            UiAction::Quit => {}
                        }
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }

    drop(session);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests for the pure helpers that live naturally beside the module.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_sanitizing_keeps_ids_raw() {
        assert_eq!(sanitize_display("a\u{1b}[31m\nb"), "a[31mb");
        let id = "raw\u{7}id";
        assert_eq!(sanitize_display(id), "rawid");
        assert_eq!(id.len(), 6);
    }

    #[test]
    fn cli_unsupported_commands_are_explicit() {
        let cli = WindowsCli::try_parse_from(["audiomix", "groups"]).unwrap();
        assert!(cli
            .command
            .as_ref()
            .and_then(WindowsCommand::unsupported_message)
            .is_some());
        let cli = WindowsCli::try_parse_from(["audiomix", "list", "--json"]).unwrap();
        assert_eq!(cli.command, Some(WindowsCommand::List { json: true }));
    }
}

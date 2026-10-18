//! Interface de terminal persistente do audiomix.
//!
//! A interface nunca executa mutações diretamente: todas as operações passam
//! por `commands::*` em uma thread de trabalho, depois de uma confirmação
//! explícita do usuário. O estado é mantido em `App`, que não depende de um
//! terminal real e pode ser testado com `ratatui::backend::TestBackend`.

use std::collections::BTreeSet;
use std::io::{self, stdout};
use std::panic;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
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
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap,
};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthStr;

use crate::backend::{Backend, PactlBackend};
use crate::combine::{valid_slug, OwnedGroup};
use crate::commands;
use crate::devices::{availability_of, display_name, kind_of, Availability, Kind};
use crate::model::Sink;

// ---------------------------------------------------------------------------
// Paleta e constantes
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

const MIN_WIDTH: u16 = 55;
const MIN_HEIGHT: u16 = 18;

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

// ---------------------------------------------------------------------------
// Estado do servidor e snapshot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    pub connected: bool,
    pub name: Option<String>,
    pub version: Option<String>,
}

impl ServerInfo {
    fn label(&self) -> String {
        if !self.connected {
            return "Servidor: indisponível (falha de conexão)".to_string();
        }
        let name = self.name.as_deref().unwrap_or("PulseAudio/PipeWire");
        match self.version.as_deref() {
            Some(v) if !v.is_empty() => format!("Servidor: {name} {v}"),
            _ => format!("Servidor: {name}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub sinks: Vec<Sink>,
    pub owned_groups: Vec<OwnedGroup>,
    pub default_sink: Option<String>,
    pub server: ServerInfo,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceRow {
    pub index: u32,
    pub name: String,
    pub friendly: String,
    pub kind: Kind,
    pub layout: String,
    pub availability: Availability,
    pub is_default: bool,
    pub selectable: bool,
}

#[derive(Debug, Clone)]
pub enum GroupRow {
    Owned {
        slug: String,
        sink_name: String,
        friendly: String,
        children: Vec<String>,
        module_id: Option<u32>,
        is_default: bool,
    },
    Foreign {
        name: String,
        friendly: String,
        is_default: bool,
    },
}

impl GroupRow {
    pub fn key(&self) -> String {
        match self {
            GroupRow::Owned { slug, .. } => format!("owned:{slug}"),
            GroupRow::Foreign { name, .. } => format!("foreign:{name}"),
        }
    }

    pub fn is_owned(&self) -> bool {
        matches!(self, GroupRow::Owned { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Devices,
    Groups,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::Devices => 0,
            Tab::Groups => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Error,
    Warning,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Success => "ok",
            Level::Error => "erro",
            Level::Warning => "aviso",
        }
    }

    fn color(self) -> Color {
        match self {
            Level::Info => TEAL,
            Level::Success => GREEN,
            Level::Error => RED,
            Level::Warning => AMBER,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub level: Level,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct FallbackChoice {
    pub name: String,
    pub friendly: String,
}

#[derive(Debug, Clone)]
pub enum Modal {
    Help,
    CombineName {
        name: String,
        error: Option<String>,
    },
    CombineOptions {
        name: String,
        outputs: Vec<String>,
        make_default: bool,
        do_move: bool,
        row: usize,
    },
    ConfirmCombine {
        name: String,
        outputs: Vec<String>,
        make_default: bool,
        do_move: bool,
    },
    ConfirmDefault {
        target: String,
        friendly: String,
        move_streams: bool,
    },
    ConfirmBluetooth {
        target: String,
        friendly: String,
    },
    RemoveFallback {
        slug: String,
        sink_name: String,
        friendly: String,
        candidates: Vec<FallbackChoice>,
        cursor: usize,
    },
    ConfirmRemove {
        slug: String,
        sink_name: String,
        friendly: String,
        fallback: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobSpec {
    Refresh,
    Combine {
        name: String,
        outputs: Vec<String>,
        make_default: bool,
        do_move: bool,
    },
    SetDefault {
        target: String,
        move_streams: bool,
    },
    Bluetooth {
        target: String,
    },
    Remove {
        slug: String,
        fallback: Option<String>,
    },
}

impl JobSpec {
    pub fn label(&self) -> &'static str {
        match self {
            JobSpec::Refresh => "atualizando dispositivos",
            JobSpec::Combine { .. } => "combinando saídas",
            JobSpec::SetDefault { .. } => "definindo saída padrão",
            JobSpec::Bluetooth { .. } => "ativando perfil A2DP",
            JobSpec::Remove { .. } => "removendo grupo",
        }
    }
}

#[derive(Debug)]
pub enum JobOutcome {
    Refresh(Result<Snapshot, String>),
    Mutation {
        messages: Result<Vec<String>, String>,
        snapshot: Result<Snapshot, String>,
    },
}

#[derive(Debug)]
pub struct JobDone {
    pub spec: JobSpec,
    pub outcome: JobOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    Submit(JobSpec),
    Quit,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct App {
    pub version: String,
    pub tab: Tab,
    pub sinks: Vec<Sink>,
    pub owned_groups: Vec<OwnedGroup>,
    pub devices: Vec<DeviceRow>,
    pub groups: Vec<GroupRow>,
    pub selected: Vec<String>,
    pub device_cursor: usize,
    pub group_cursor: usize,
    pub default_sink: Option<String>,
    pub server: ServerInfo,
    pub modal: Option<Modal>,
    pub modal_scroll: u16,
    pub details_scroll: u16,
    pub notification: Option<Notification>,
    pub history: Vec<String>,
    pub job: Option<JobSpec>,
    pub exit_requested: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            tab: Tab::Devices,
            sinks: Vec::new(),
            owned_groups: Vec::new(),
            devices: Vec::new(),
            groups: Vec::new(),
            selected: Vec::new(),
            device_cursor: 0,
            group_cursor: 0,
            default_sink: None,
            server: ServerInfo::default(),
            modal: None,
            modal_scroll: 0,
            details_scroll: 0,
            notification: None,
            history: Vec::new(),
            job: None,
            exit_requested: false,
            should_quit: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn notify(&mut self, level: Level, text: impl Into<String>) {
        let text: String = text.into();
        let text = sanitize_display(&text);
        self.notification = Some(Notification {
            level,
            text: text.clone(),
        });
        self.history.push(format!("[{}] {text}", level.tag()));
        if self.history.len() > 20 {
            let drop = self.history.len() - 20;
            self.history.drain(0..drop);
        }
    }

    /// Aplica uma leitura nova, preservando foco e marcações por nome estável.
    /// Marcações de saídas desconectadas ou agora inelegíveis são removidas.
    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        let old_focus = self
            .devices
            .get(self.device_cursor)
            .map(|row| row.name.clone());
        let old_group = self.groups.get(self.group_cursor).map(GroupRow::key);

        self.sinks = snapshot.sinks;
        self.owned_groups = snapshot.owned_groups;
        self.default_sink = snapshot.default_sink;
        self.server = snapshot.server;

        let default = self.default_sink.clone();
        self.devices = self
            .sinks
            .iter()
            .map(|sink| {
                let kind = kind_of(sink);
                let availability = availability_of(sink);
                DeviceRow {
                    index: sink.index,
                    name: sink.name.clone(),
                    friendly: display_name(sink),
                    kind,
                    layout: sink.layout_pt().to_string(),
                    availability,
                    is_default: default.as_deref() == Some(sink.name.as_str()),
                    selectable: crate::devices::selectable(sink),
                }
            })
            .collect();

        let friendly_of = |name: &str| {
            self.sinks
                .iter()
                .find(|s| s.name == name)
                .map(display_name)
                .unwrap_or_else(|| name.to_string())
        };

        let mut rows: Vec<GroupRow> = self
            .owned_groups
            .iter()
            .map(|group| GroupRow::Owned {
                slug: group.slug.clone(),
                sink_name: group.sink_name.clone(),
                friendly: format!("Grupo {}", group.slug),
                children: group.slaves.iter().map(|n| friendly_of(n)).collect(),
                module_id: group.module_id,
                is_default: default.as_deref() == Some(group.sink_name.as_str()),
            })
            .collect();

        let owned_names: BTreeSet<&str> = self
            .owned_groups
            .iter()
            .map(|g| g.sink_name.as_str())
            .collect();
        for sink in &self.sinks {
            if kind_of(sink) == Kind::Virtual && !owned_names.contains(sink.name.as_str()) {
                rows.push(GroupRow::Foreign {
                    name: sink.name.clone(),
                    friendly: display_name(sink),
                    is_default: default.as_deref() == Some(sink.name.as_str()),
                });
            }
        }
        self.groups = rows;

        let selectable: BTreeSet<&str> = self
            .devices
            .iter()
            .filter(|row| row.selectable)
            .map(|row| row.name.as_str())
            .collect();
        self.selected
            .retain(|name| selectable.contains(name.as_str()));

        self.device_cursor = old_focus
            .and_then(|name| self.devices.iter().position(|row| row.name == name))
            .unwrap_or_else(|| {
                self.devices
                    .iter()
                    .position(|d| {
                        d.selectable && matches!(d.kind, Kind::Headphones | Kind::Bluetooth)
                    })
                    .or_else(|| self.devices.iter().position(|d| d.selectable))
                    .unwrap_or(0)
            });
        self.group_cursor = old_group
            .and_then(|key| self.groups.iter().position(|row| row.key() == key))
            .unwrap_or_else(|| self.group_cursor.min(self.groups.len().saturating_sub(1)));
        self.details_scroll = 0;
    }

    fn begin_job(&mut self, spec: JobSpec) -> UiAction {
        if self.job.is_none() {
            let label = spec.label();
            self.job = Some(spec.clone());
            self.notify(Level::Info, format!("Executando: {label}"));
        }
        UiAction::Submit(spec)
    }

    fn request_exit(&mut self) -> Option<UiAction> {
        if self.busy() {
            self.exit_requested = true;
            self.notify(
                Level::Info,
                "A operação em andamento será concluída antes de sair",
            );
            None
        } else {
            self.should_quit = true;
            Some(UiAction::Quit)
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        self.details_scroll = 0;
        let len = match self.tab {
            Tab::Devices => self.devices.len(),
            Tab::Groups => self.groups.len(),
        };
        if len == 0 {
            return;
        }
        let cursor = match self.tab {
            Tab::Devices => &mut self.device_cursor,
            Tab::Groups => &mut self.group_cursor,
        };
        let next = (*cursor as isize + delta).clamp(0, len as isize - 1);
        *cursor = next as usize;
    }

    fn current_device(&self) -> Option<&DeviceRow> {
        self.devices.get(self.device_cursor)
    }

    fn current_group(&self) -> Option<&GroupRow> {
        self.groups.get(self.group_cursor)
    }

    fn toggle_device(&mut self) {
        let Some(row) = self.current_device().cloned() else {
            return;
        };
        if !row.selectable {
            let reason = match row.kind {
                Kind::Own | Kind::Virtual => "Saída somente leitura; não pode ser combinada.",
                _ if row.availability == Availability::Unavailable => {
                    "Saída indisponível; conecte o dispositivo antes de combinar."
                }
                _ => "Esta saída não é elegível para combinação.",
            };
            self.notify(Level::Warning, reason);
            return;
        }
        if let Some(pos) = self.selected.iter().position(|name| name == &row.name) {
            self.selected.remove(pos);
            self.notify(
                Level::Info,
                format!(
                    "Desmarcada: {} ({} marcadas)",
                    row.friendly,
                    self.selected.len()
                ),
            );
        } else {
            self.selected.push(row.name.clone());
            self.notify(
                Level::Info,
                format!(
                    "Marcada: {} ({} marcadas)",
                    row.friendly,
                    self.selected.len()
                ),
            );
        }
    }

    fn open_combine(&mut self) {
        if self.selected.len() < 2 {
            self.notify(
                Level::Warning,
                "Marque ao menos 2 saídas físicas elegíveis com Espaço.",
            );
            return;
        }
        self.modal = Some(Modal::CombineName {
            name: String::new(),
            error: None,
        });
    }

    fn open_default(&mut self) {
        if self.tab == Tab::Groups {
            match self.current_group().cloned() {
                Some(GroupRow::Owned {
                    sink_name,
                    friendly,
                    ..
                }) => {
                    self.modal = Some(Modal::ConfirmDefault {
                        target: sink_name,
                        friendly,
                        move_streams: false,
                    });
                }
                _ => self.notify(
                    Level::Warning,
                    "Escolha um grupo proprio para definir como padrao.",
                ),
            }
            return;
        }
        let Some(row) = self.current_device().cloned() else {
            return;
        };
        if row.availability == Availability::Unavailable {
            self.notify(Level::Warning, "A saída em foco está indisponível.");
            return;
        }
        if row.kind == Kind::Virtual {
            self.notify(
                Level::Warning,
                "Saída virtual somente leitura; escolha uma saída física ou um grupo próprio.",
            );
            return;
        }
        self.modal = Some(Modal::ConfirmDefault {
            target: row.name,
            friendly: row.friendly,
            move_streams: false,
        });
    }

    fn open_bluetooth(&mut self) {
        if self.tab != Tab::Devices {
            self.notify(
                Level::Warning,
                "Use Tab para escolher um dispositivo Bluetooth na aba Dispositivos.",
            );
            return;
        }
        let Some(row) = self.current_device().cloned() else {
            return;
        };
        if row.kind != Kind::Bluetooth {
            self.notify(Level::Warning, "A saída em foco não é Bluetooth.");
            return;
        }
        if row.availability == Availability::Unavailable {
            self.notify(Level::Warning, "O dispositivo Bluetooth está indisponível.");
            return;
        }
        self.modal = Some(Modal::ConfirmBluetooth {
            target: row.name,
            friendly: row.friendly,
        });
    }

    fn open_remove(&mut self) {
        if self.tab != Tab::Groups {
            self.notify(
                Level::Warning,
                "Use Tab para escolher o grupo na aba Grupos antes de remover.",
            );
            return;
        }
        let Some(row) = self.current_group().cloned() else {
            return;
        };
        let GroupRow::Owned {
            slug,
            sink_name,
            friendly,
            ..
        } = row
        else {
            self.notify(
                Level::Warning,
                "Somente grupos próprios do audiomix podem ser removidos; virtuais estrangeiros são somente leitura.",
            );
            return;
        };

        if self.default_sink.as_deref() == Some(sink_name.as_str()) {
            let candidates: Vec<FallbackChoice> = self
                .devices
                .iter()
                .filter(|d| {
                    d.name != sink_name
                        && !matches!(d.kind, Kind::Own | Kind::Virtual)
                        && d.availability != Availability::Unavailable
                })
                .map(|d| FallbackChoice {
                    name: d.name.clone(),
                    friendly: d.friendly.clone(),
                })
                .collect();
            if candidates.is_empty() {
                self.notify(
                    Level::Error,
                    "O grupo é a saída padrão e não há saída disponível fora dele para fallback. Remoção bloqueada.",
                );
                return;
            }
            self.modal = Some(Modal::RemoveFallback {
                slug,
                sink_name,
                friendly,
                candidates,
                cursor: 0,
            });
        } else {
            self.modal = Some(Modal::ConfirmRemove {
                slug,
                sink_name,
                friendly,
                fallback: None,
            });
        }
    }

    pub fn handle_terminal_key(
        &mut self,
        key: KeyEvent,
        width: u16,
        height: u16,
    ) -> Option<UiAction> {
        if width < MIN_WIDTH || height < MIN_HEIGHT {
            if is_ctrl_c(&key) || is_char(&key, 'q') {
                return self.request_exit();
            }
            if key.code == KeyCode::Esc {
                self.modal = None;
                self.modal_scroll = 0;
            }
            self.notify(
                Level::Warning,
                "Amplie o terminal para ver e confirmar a operacao.",
            );
            return None;
        }
        self.handle_key(key)
    }

    /// Redutor de teclas. Não executa mutações: no máximo devolve a ação que
    /// deve ser iniciada pela camada de execução.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<UiAction> {
        if matches!(key.code, KeyCode::PageDown | KeyCode::PageUp) {
            let scroll = if self.modal.is_some() {
                &mut self.modal_scroll
            } else {
                &mut self.details_scroll
            };
            *scroll = if key.code == KeyCode::PageDown {
                scroll.saturating_add(4)
            } else {
                scroll.saturating_sub(4)
            };
            return None;
        }
        if self.modal.is_some() {
            let previous = self.modal.as_ref().map(std::mem::discriminant);
            let action = self.handle_modal_key(key);
            if self.modal.as_ref().map(std::mem::discriminant) != previous {
                self.modal_scroll = 0;
            }
            return action;
        }
        self.modal_scroll = 0;

        if is_ctrl_c(&key) || is_char(&key, 'q') {
            return self.request_exit();
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }

        if is_char(&key, '?') {
            self.modal = Some(Modal::Help);
            return None;
        }

        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.details_scroll = 0;
                self.tab = match self.tab {
                    Tab::Devices => Tab::Groups,
                    Tab::Groups => Tab::Devices,
                };
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                None
            }
            KeyCode::Char(' ') => {
                if self.tab == Tab::Devices {
                    self.toggle_device();
                }
                None
            }
            KeyCode::Char('r') => {
                if self.busy() {
                    self.notify(
                        Level::Warning,
                        "Aguarde a operação atual antes de atualizar.",
                    );
                    None
                } else {
                    Some(self.begin_job(JobSpec::Refresh))
                }
            }
            KeyCode::Char('c') => {
                if self.busy() {
                    self.notify(
                        Level::Warning,
                        "Aguarde a operação atual antes de combinar.",
                    );
                    None
                } else {
                    self.open_combine();
                    None
                }
            }
            KeyCode::Char('d') => {
                if self.busy() {
                    self.notify(
                        Level::Warning,
                        "Aguarde a operação atual antes de alterar o padrão.",
                    );
                    None
                } else {
                    self.open_default();
                    None
                }
            }
            KeyCode::Char('s') => {
                if self.busy() {
                    self.notify(
                        Level::Warning,
                        "Aguarde a operação atual antes de alterar o perfil.",
                    );
                    None
                } else {
                    self.open_bluetooth();
                    None
                }
            }
            KeyCode::Char('x') => {
                if self.busy() {
                    self.notify(Level::Warning, "Aguarde a operação atual antes de remover.");
                    None
                } else {
                    self.open_remove();
                    None
                }
            }
            KeyCode::Esc => {
                self.modal = None;
                None
            }
            _ => None,
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> Option<UiAction> {
        let modal = self.modal.take()?;

        if is_ctrl_c(&key) {
            self.notify(Level::Info, "Operação cancelada.");
            return self.request_exit();
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.modal = Some(modal);
            return None;
        }

        match modal {
            Modal::Help => {
                if matches!(key.code, KeyCode::Esc) || is_char(&key, '?') || is_char(&key, 'q') {
                    None
                } else {
                    self.modal = Some(Modal::Help);
                    None
                }
            }
            Modal::CombineName { mut name, error } => match key.code {
                KeyCode::Esc => {
                    self.notify(Level::Info, "Criação do grupo cancelada.");
                    None
                }
                KeyCode::Enter => {
                    if valid_slug(&name) {
                        let outputs = self.selected.clone();
                        if outputs.len() < 2 {
                            self.modal = Some(Modal::CombineName {
                                name,
                                error: Some(
                                    "São necessárias ao menos 2 saídas marcadas.".to_string(),
                                ),
                            });
                        } else {
                            self.modal = Some(Modal::CombineOptions {
                                name,
                                outputs,
                                make_default: true,
                                do_move: true,
                                row: 0,
                            });
                        }
                    } else {
                        self.modal = Some(Modal::CombineName {
                            name,
                            error: Some(slug_error()),
                        });
                    }
                    None
                }
                KeyCode::Backspace => {
                    name.pop();
                    self.modal = Some(Modal::CombineName { name, error: None });
                    None
                }
                KeyCode::Char(c) if !c.is_control() && name.chars().count() < 40 => {
                    name.push(c);
                    self.modal = Some(Modal::CombineName { name, error: None });
                    None
                }
                _ => {
                    let _ = error;
                    self.modal = Some(Modal::CombineName { name, error: None });
                    None
                }
            },
            Modal::CombineOptions {
                name,
                outputs,
                mut make_default,
                mut do_move,
                mut row,
            } => match key.code {
                KeyCode::Esc => {
                    self.notify(Level::Info, "Criação do grupo cancelada.");
                    None
                }
                KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k') => {
                    row = 1 - row.min(1);
                    self.modal = Some(Modal::CombineOptions {
                        name,
                        outputs,
                        make_default,
                        do_move,
                        row,
                    });
                    None
                }
                KeyCode::Char(' ') => {
                    if row == 0 {
                        make_default = !make_default;
                    } else {
                        do_move = !do_move;
                    }
                    self.modal = Some(Modal::CombineOptions {
                        name,
                        outputs,
                        make_default,
                        do_move,
                        row,
                    });
                    None
                }
                KeyCode::Enter => {
                    self.modal = Some(Modal::ConfirmCombine {
                        name,
                        outputs,
                        make_default,
                        do_move,
                    });
                    None
                }
                _ => {
                    self.modal = Some(Modal::CombineOptions {
                        name,
                        outputs,
                        make_default,
                        do_move,
                        row,
                    });
                    None
                }
            },
            Modal::ConfirmCombine {
                name,
                outputs,
                make_default,
                do_move,
            } => {
                if is_char(&key, 'y') || is_char(&key, 'Y') {
                    Some(self.begin_job(JobSpec::Combine {
                        name,
                        outputs,
                        make_default,
                        do_move,
                    }))
                } else if matches!(key.code, KeyCode::Esc)
                    || is_char(&key, 'n')
                    || is_char(&key, 'N')
                {
                    self.notify(Level::Info, "Criação do grupo cancelada.");
                    None
                } else {
                    self.modal = Some(Modal::ConfirmCombine {
                        name,
                        outputs,
                        make_default,
                        do_move,
                    });
                    None
                }
            }
            Modal::ConfirmDefault {
                target,
                friendly,
                mut move_streams,
            } => {
                if is_char(&key, 'y') || is_char(&key, 'Y') {
                    Some(self.begin_job(JobSpec::SetDefault {
                        target,
                        move_streams,
                    }))
                } else if matches!(key.code, KeyCode::Esc)
                    || is_char(&key, 'n')
                    || is_char(&key, 'N')
                {
                    self.notify(Level::Info, "Alteração da saída padrão cancelada.");
                    None
                } else {
                    if matches!(key.code, KeyCode::Char(' ')) {
                        move_streams = !move_streams;
                    }
                    self.modal = Some(Modal::ConfirmDefault {
                        target,
                        friendly,
                        move_streams,
                    });
                    None
                }
            }
            Modal::ConfirmBluetooth { target, friendly } => {
                if is_char(&key, 'y') || is_char(&key, 'Y') {
                    Some(self.begin_job(JobSpec::Bluetooth { target }))
                } else if matches!(key.code, KeyCode::Esc)
                    || is_char(&key, 'n')
                    || is_char(&key, 'N')
                {
                    self.notify(Level::Info, "Ativação de A2DP cancelada.");
                    None
                } else {
                    self.modal = Some(Modal::ConfirmBluetooth { target, friendly });
                    None
                }
            }
            Modal::RemoveFallback {
                slug,
                sink_name,
                friendly,
                candidates,
                mut cursor,
            } => match key.code {
                KeyCode::Esc => {
                    self.notify(Level::Info, "Remoção cancelada.");
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = cursor.saturating_sub(1);
                    self.modal = Some(Modal::RemoveFallback {
                        slug,
                        sink_name,
                        friendly,
                        candidates,
                        cursor,
                    });
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor = (cursor + 1).min(candidates.len().saturating_sub(1));
                    self.modal = Some(Modal::RemoveFallback {
                        slug,
                        sink_name,
                        friendly,
                        candidates,
                        cursor,
                    });
                    None
                }
                KeyCode::Enter => {
                    let fallback = candidates.get(cursor).map(|c| c.name.clone());
                    if fallback.is_none() {
                        self.modal = Some(Modal::RemoveFallback {
                            slug,
                            sink_name,
                            friendly,
                            candidates,
                            cursor,
                        });
                        return None;
                    }
                    self.modal = Some(Modal::ConfirmRemove {
                        slug,
                        sink_name,
                        friendly,
                        fallback,
                    });
                    None
                }
                _ => {
                    self.modal = Some(Modal::RemoveFallback {
                        slug,
                        sink_name,
                        friendly,
                        candidates,
                        cursor,
                    });
                    None
                }
            },
            Modal::ConfirmRemove {
                slug,
                sink_name,
                friendly,
                fallback,
            } => {
                if is_char(&key, 'y') || is_char(&key, 'Y') {
                    Some(self.begin_job(JobSpec::Remove { slug, fallback }))
                } else if matches!(key.code, KeyCode::Esc)
                    || is_char(&key, 'n')
                    || is_char(&key, 'N')
                {
                    self.notify(Level::Info, "Remoção cancelada.");
                    None
                } else {
                    self.modal = Some(Modal::ConfirmRemove {
                        slug,
                        sink_name,
                        friendly,
                        fallback,
                    });
                    None
                }
            }
        }
    }

    pub fn on_job_done(&mut self, done: JobDone) {
        self.job = None;
        match done.outcome {
            JobOutcome::Refresh(Ok(snapshot)) => {
                let warnings = snapshot.warnings.clone();
                self.apply_snapshot(snapshot);
                if warnings.is_empty() {
                    self.notify(Level::Success, "Dispositivos atualizados.");
                } else {
                    self.notify(Level::Warning, warnings.join(" "));
                }
            }
            JobOutcome::Refresh(Err(err)) => {
                self.server.connected = false;
                self.notify(Level::Error, format!("Falha ao atualizar: {err}"));
            }
            JobOutcome::Mutation { messages, snapshot } => match messages {
                Ok(messages) => {
                    let summary = if messages.is_empty() {
                        "Operação concluída.".to_string()
                    } else {
                        messages.join(" ")
                    };
                    match snapshot {
                        Ok(snapshot) => {
                            let warnings = snapshot.warnings.clone();
                            self.apply_snapshot(snapshot);
                            if warnings.is_empty() {
                                self.notify(Level::Success, summary);
                            } else {
                                self.notify(
                                    Level::Success,
                                    format!("{summary} {}", warnings.join(" ")),
                                );
                            }
                        }
                        Err(err) => {
                            self.server.connected = false;
                            self.notify(
                                Level::Error,
                                format!("{summary} Mas o estado não pôde ser atualizado: {err}"),
                            );
                        }
                    }
                }
                Err(err) => {
                    let refresh_error = match snapshot {
                        Ok(snapshot) => {
                            self.apply_snapshot(snapshot);
                            String::new()
                        }
                        Err(error) => {
                            self.server.connected = false;
                            format!(" Estado desatualizado: {error}")
                        }
                    };
                    self.notify(
                        Level::Error,
                        format!("Operação falhou: {err}{refresh_error}"),
                    );
                }
            },
        }

        if self.exit_requested {
            self.should_quit = true;
        }
    }
}

fn slug_error() -> String {
    "Nome inválido. Use a-z, 0-9, _ e -, até 40 caracteres, sem separador no início ou fim."
        .to_string()
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_char(key: &KeyEvent, c: char) -> bool {
    matches!(key.code, KeyCode::Char(got) if got == c)
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

// ---------------------------------------------------------------------------
// Trabalho em background
// ---------------------------------------------------------------------------

fn run_snapshot(backend: &dyn Backend) -> Result<Snapshot, String> {
    let sinks = commands::sinks(backend).map_err(|e| format!("{e:#}"))?;
    let owned = commands::owned_groups(backend, &sinks).map_err(|e| format!("{e:#}"))?;
    let default = commands::default_sink(backend).map_err(|e| format!("{e:#}"))?;

    let mut warnings = Vec::new();
    let server = match backend.run(&["-f", "json", "info"]) {
        Ok(raw) => match parse_server(&raw) {
            Some(info) => info,
            None => {
                let info = ServerInfo {
                    connected: false,
                    name: None,
                    version: None,
                };
                warnings.push(
                    "Servidor: resposta de info ilegível; verifique PulseAudio/PipeWire."
                        .to_string(),
                );
                info
            }
        },
        Err(err) => {
            warnings.push(format!(
                "Servidor: falha de conexão ao ler pactl info ({err:#})."
            ));
            ServerInfo {
                connected: false,
                name: None,
                version: None,
            }
        }
    };

    Ok(Snapshot {
        sinks,
        owned_groups: owned,
        default_sink: default,
        server,
        warnings,
    })
}

fn parse_server(raw: &str) -> Option<ServerInfo> {
    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let server = value.get("server").unwrap_or(&value);
    let name = server
        .get("server_name")
        .or_else(|| server.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let version = server
        .get("server_version")
        .or_else(|| server.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    name.as_ref()?;
    Some(ServerInfo {
        connected: true,
        name,
        version,
    })
}

fn execute_job(spec: JobSpec, backend: &dyn Backend) -> JobOutcome {
    match spec {
        JobSpec::Refresh => JobOutcome::Refresh(run_snapshot(backend)),
        JobSpec::Combine {
            name,
            outputs,
            make_default,
            do_move,
        } => {
            let messages = commands::combine(backend, &name, &outputs, make_default, do_move)
                .map(normalize_messages)
                .map_err(|e| format!("{e:#}"));
            JobOutcome::Mutation {
                messages,
                snapshot: run_snapshot(backend),
            }
        }
        JobSpec::SetDefault {
            target,
            move_streams,
        } => {
            let messages = commands::set_default(backend, &target, move_streams)
                .map(normalize_messages)
                .map_err(|e| format!("{e:#}"));
            JobOutcome::Mutation {
                messages,
                snapshot: run_snapshot(backend),
            }
        }
        JobSpec::Bluetooth { target } => {
            let messages = commands::activate_bluetooth_for(backend, &target)
                .map(normalize_messages)
                .map_err(|e| format!("{e:#}"));
            JobOutcome::Mutation {
                messages,
                snapshot: run_snapshot(backend),
            }
        }
        JobSpec::Remove { slug, fallback } => {
            let messages = commands::remove(backend, &slug, fallback.as_deref())
                .map(normalize_messages)
                .map_err(|e| format!("{e:#}"));
            JobOutcome::Mutation {
                messages,
                snapshot: run_snapshot(backend),
            }
        }
    }
}

fn normalize_messages(messages: Vec<String>) -> Vec<String> {
    messages
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect()
}

fn spawn_job(spec: JobSpec, tx: Sender<JobDone>) {
    thread::spawn(move || {
        // Cada trabalho usa seu próprio backend, sem compartilhar estado.
        let backend = PactlBackend::new();
        let outcome = execute_job_safely(spec.clone(), &backend);
        let _ = tx.send(JobDone { spec, outcome });
    });
}

// ---------------------------------------------------------------------------
// Terminal
// ---------------------------------------------------------------------------

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut out = stdout();
    let _ = execute!(out, LeaveAlternateScreen, cursor::Show);
}

fn run_snapshot_safely(backend: &dyn Backend) -> Result<Snapshot, String> {
    panic::catch_unwind(panic::AssertUnwindSafe(|| run_snapshot(backend))).unwrap_or_else(|_| {
        Err("Falha interna ao reler o estado; pressione r para conferir.".into())
    })
}

fn execute_job_safely(spec: JobSpec, backend: &dyn Backend) -> JobOutcome {
    let was_refresh = matches!(spec, JobSpec::Refresh);
    let message = "Falha interna no trabalho. O estado pode ter mudado; pressione r para conferir antes de repetir.";
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| execute_job(spec, backend)));
    match result {
        Ok(outcome) => outcome,
        // A panicking mutation must not be downgraded to a refresh failure:
        // the command may already have changed audio state.
        Err(_) if was_refresh => JobOutcome::Refresh(Err(message.into())),
        Err(_) => JobOutcome::Mutation {
            messages: Err(message.into()),
            snapshot: run_snapshot_safely(backend),
        },
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("ativar modo raw do terminal")?;
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

/// Executa a interface persistente. Sempre restaura o terminal, inclusive em
/// erro ou panic.
pub fn run() -> Result<()> {
    let ui_thread = thread::current().id();
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if thread::current().id() == ui_thread {
            restore_terminal();
            previous_hook(info);
        }
    }));

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("inicializar terminal")?;

    let (tx, rx) = mpsc::channel::<JobDone>();
    let mut app = App::new(env!("CARGO_PKG_VERSION"));

    app.begin_job(JobSpec::Refresh);
    spawn_job(JobSpec::Refresh, tx.clone());

    let result = event_loop(&mut terminal, &mut app, &rx, &tx);
    restore_terminal();
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &Receiver<JobDone>,
    tx: &Sender<JobDone>,
) -> Result<()> {
    loop {
        while let Ok(done) = rx.try_recv() {
            app.on_job_done(done);
        }
        if app.should_quit {
            break;
        }

        terminal
            .draw(|frame| {
                clamp_scroll(app, frame.area());
                draw(frame, app);
            })
            .context("desenhar TUI")?;

        if event::poll(Duration::from_millis(80)).context("consultar eventos do terminal")? {
            match event::read().context("ler evento do terminal")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let size = terminal.size().context("ler tamanho do terminal")?;
                    if let Some(action) = app.handle_terminal_key(key, size.width, size.height) {
                        match action {
                            UiAction::Quit => break,
                            UiAction::Submit(spec) => spawn_job(spec, tx.clone()),
                        }
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Desenho
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
            Constraint::Length(4),
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(area);

    draw_header(frame, rows[0], app);
    draw_tabs(frame, rows[1], app);
    draw_body(frame, rows[2], app);
    draw_notification(frame, rows[3], app);
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(TEAL))
        .style(bg_panel())
        .title(Span::styled(" audiomix ", accent()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    let title = Line::from(vec![
        Span::styled("audiomix", accent()),
        Span::styled("  audio workspace", Style::default().fg(TEXT)),
        Span::styled(format!("  v{}", app.version), muted()),
    ]);
    frame.render_widget(Paragraph::new(title), lines[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(lines[1]);

    let loading = app.devices.is_empty() && matches!(app.job, Some(JobSpec::Refresh));
    let server_label = if loading {
        "Servidor: consultando...".to_string()
    } else {
        app.server.label()
    };
    let server_style = if app.server.connected || loading {
        muted()
    } else {
        Style::default().fg(RED)
    };
    frame.render_widget(
        Paragraph::new(clip(&server_label, cols[0].width))
            .style(server_style)
            .alignment(Alignment::Left),
        cols[0],
    );

    let default_label = match app.default_sink.as_deref() {
        Some(name) => {
            let friendly = app
                .sinks
                .iter()
                .find(|s| s.name == name)
                .map(display_name)
                .unwrap_or_else(|| name.to_string());
            format!("Padrão atual: {friendly}")
        }
        None if loading => "Padrão atual: consultando...".to_string(),
        None => "Padrão atual: nenhum".to_string(),
    };
    frame.render_widget(
        Paragraph::new(clip(&default_label, cols[1].width))
            .style(Style::default().fg(AMBER))
            .alignment(Alignment::Right),
        cols[1],
    );
}

fn draw_tabs(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let tabs = Tabs::new(vec![" Dispositivos ", " Grupos "])
        .select(app.tab.index())
        .divider(Span::styled("  ", muted()))
        .style(muted())
        .highlight_style(amber());
    frame.render_widget(tabs, area);
}

fn draw_body(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (list, details) = body_areas(area);
    draw_list(frame, list, app);
    draw_details(frame, details, app);
}

fn body_areas(area: Rect) -> (Rect, Rect) {
    let wide = area.width >= 78;
    let areas = Layout::default()
        .direction(if wide {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if wide {
            [Constraint::Percentage(56), Constraint::Percentage(44)]
        } else {
            [Constraint::Percentage(40), Constraint::Percentage(60)]
        })
        .split(area);
    (areas[0], areas[1])
}

fn draw_list(frame: &mut Frame<'_>, area: Rect, app: &App) {
    match app.tab {
        Tab::Devices => draw_device_list(frame, area, app),
        Tab::Groups => draw_group_list(frame, area, app),
    }
}

fn list_highlight() -> Style {
    // Fundo escuro de foco: mantém todos os spans legíveis, inclusive âmbar e
    // cinza, sem depender de o style do highlight sobrescrever cada span.
    Style::default().bg(PANEL_ALT).add_modifier(Modifier::BOLD)
}

fn draw_device_list(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = if app.devices.is_empty() {
        " Dispositivos ".to_string()
    } else {
        format!(
            " Dispositivos ({}) {} marcada(s) ",
            app.devices.len(),
            app.selected.len()
        )
    };
    let block = panelled_block(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.devices.is_empty() {
        let text = if app.busy() {
            "Carregando saídas de áudio..."
        } else {
            "Nenhuma saída encontrada. Pressione r para atualizar."
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(muted())
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    // Em listas muito baixas (55x18) usa uma linha por saída para manter
    // mais de um dispositivo navegável, sem perder tipo/layout/disponibilidade.
    let compact_rows = inner.height < 5;
    let items: Vec<ListItem> = app
        .devices
        .iter()
        .enumerate()
        .map(|(item_index, row)| {
            let selected = app.selected.iter().any(|n| n == &row.name);
            let (mark, mark_style) = if row.selectable {
                if selected {
                    ("[x]", amber())
                } else {
                    ("[ ]", muted())
                }
            } else {
                ("[-]", Style::default().fg(Color::Rgb(90, 108, 132)))
            };
            let mut first = vec![
                Span::styled(
                    if item_index == app.device_cursor {
                        "▌ "
                    } else {
                        "  "
                    },
                    if item_index == app.device_cursor {
                        amber()
                    } else {
                        muted()
                    },
                ),
                Span::styled(mark, mark_style),
                Span::raw(" "),
                Span::styled(sanitize_display(&row.friendly), Style::default().fg(TEXT)),
            ];
            if row.is_default {
                first.push(Span::styled("  [padrão]", amber()));
            }
            if !row.selectable {
                first.push(Span::styled("  leitura", muted()));
            }
            let second = format!(
                "    {}  ·  {}  ·  {}  ·  #{}",
                short_kind(row.kind),
                row.layout,
                row.availability.label_pt(),
                row.index
            );
            if compact_rows {
                let mut first = first;
                first.push(Span::styled(
                    format!(
                        "  {} {} {}",
                        short_kind(row.kind),
                        row.layout,
                        row.availability.label_pt()
                    ),
                    muted(),
                ));
                ListItem::new(Line::from(first))
            } else {
                ListItem::new(vec![
                    Line::from(first),
                    Line::from(Span::styled(second, muted())),
                ])
            }
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.device_cursor));
    let list = List::new(items)
        .block(Block::default().style(bg_panel()))
        .highlight_style(list_highlight());
    frame.render_stateful_widget(list, inner, &mut state);
}

fn draw_group_list(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = panelled_block(format!(" Grupos ({}) ", app.groups.len()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.groups.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "Nenhum grupo próprio ou virtual. Marque 2 ou mais saídas e pressione c.",
            )
            .style(muted())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let compact_rows = inner.height < 5;
    let items: Vec<ListItem> = app
        .groups
        .iter()
        .enumerate()
        .map(|(item_index, row)| match row {
            GroupRow::Owned {
                friendly,
                children,
                is_default,
                ..
            } => {
                let mut first = vec![
                    Span::styled(
                        if item_index == app.group_cursor {
                            "▌ "
                        } else {
                            "  "
                        },
                        if item_index == app.group_cursor {
                            amber()
                        } else {
                            muted()
                        },
                    ),
                    Span::styled("[grupo] ", amber()),
                    Span::styled(sanitize_display(friendly), Style::default().fg(TEXT)),
                ];
                if *is_default {
                    first.push(Span::styled("  [padrão]", amber()));
                }
                let children = if children.is_empty() {
                    "sem saídas associadas".to_string()
                } else {
                    sanitize_display(&children.join(", "))
                };
                if compact_rows {
                    let mut first = first;
                    first.push(Span::styled(format!("  {children}"), muted()));
                    ListItem::new(Line::from(first))
                } else {
                    ListItem::new(vec![
                        Line::from(first),
                        Line::from(Span::styled(format!("    {children}"), muted())),
                    ])
                }
            }
            GroupRow::Foreign {
                friendly,
                is_default,
                ..
            } => {
                let mut first = vec![
                    Span::styled(
                        if item_index == app.group_cursor {
                            "▌ "
                        } else {
                            "  "
                        },
                        if item_index == app.group_cursor {
                            amber()
                        } else {
                            muted()
                        },
                    ),
                    Span::styled("[leitura] ", muted()),
                    Span::styled(sanitize_display(friendly), muted()),
                ];
                if *is_default {
                    first.push(Span::styled("  [padrão]", muted()));
                }
                if compact_rows {
                    ListItem::new(Line::from(first))
                } else {
                    ListItem::new(vec![
                        Line::from(first),
                        Line::from(Span::styled(
                            "    virtual não gerenciado pelo audiomix",
                            muted(),
                        )),
                    ])
                }
            }
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.group_cursor));
    let list = List::new(items)
        .block(Block::default().style(bg_panel()))
        .highlight_style(list_highlight());
    frame.render_stateful_widget(list, inner, &mut state);
}

fn draw_details(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = " Detalhes ".to_string();
    let block = panelled_block(title.clone());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let logical = match app.tab {
        Tab::Devices => device_details(app),
        Tab::Groups => group_details(app),
    };
    let paragraph = Paragraph::new(logical)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(TEXT));
    let view = inner.height as usize;
    let max_scroll = paragraph.line_count(inner.width).saturating_sub(view);
    let scroll = (app.details_scroll as usize).min(max_scroll);

    if max_scroll > 0 {
        let hint = if scroll == 0 {
            " Detalhes  PgDn "
        } else if scroll >= max_scroll {
            " Detalhes  PgUp "
        } else {
            " Detalhes  PgUp/PgDn "
        };
        let block = panelled_block(hint.to_string());
        frame.render_widget(block, area);
    }
    frame.render_widget(paragraph.scroll((scroll as u16, 0)), inner);
}

fn device_details(app: &App) -> Vec<Line<'static>> {
    let Some(row) = app.current_device() else {
        return vec![Line::from(Span::styled(
            "Nenhuma saída selecionada.",
            muted(),
        ))];
    };

    let default_text = if row.is_default { "sim" } else { "não" };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(sanitize_display(&row.friendly), accent()),
            if row.is_default {
                Span::styled("  [padrão]", amber())
            } else {
                Span::raw("")
            },
        ]),
        Line::from(vec![
            Span::styled(format!("{} marcada(s)", app.selected.len()), amber()),
            Span::styled(
                format!(
                    "   padrão: {default_text}   {}   {}",
                    short_kind(row.kind),
                    row.layout
                ),
                muted(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Roteamento: ", amber()),
            Span::styled("simultâneo; confirme antes de aplicar.", muted()),
        ]),
    ];

    if app.selected.is_empty() {
        lines.push(Line::from(Span::styled("Marcadas: nenhuma", muted())));
    } else {
        let names = app
            .selected
            .iter()
            .map(|name| {
                app.sinks
                    .iter()
                    .find(|s| &s.name == name)
                    .map(display_name)
                    .unwrap_or_else(|| sanitize_display(name))
            })
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(vec![
            Span::styled(format!("Marcadas ({}): ", app.selected.len()), amber()),
            Span::raw(names),
        ]));
    }

    if matches!(row.kind, Kind::Own | Kind::Virtual) {
        lines.push(Line::from(Span::styled(
            "Somente leitura: virtuais e grupos existentes não são combináveis.",
            Style::default().fg(AMBER),
        )));
    } else if row.availability == Availability::Unavailable {
        lines.push(Line::from(Span::styled(
            "Indisponível: conecte o dispositivo para combinar.",
            Style::default().fg(RED),
        )));
    }

    // Metadados técnicos por último: não consomem o espaço acionável.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Metadados", amber())));
    lines.push(Line::from(vec![
        Span::styled("Tipo: ", muted()),
        Span::raw(format!(
            "{}   Layout: {}   Disponibilidade: {}",
            short_kind(row.kind),
            row.layout,
            row.availability.label_pt()
        )),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Índice: ", muted()),
        Span::raw(format!("#{}   Padrão: {default_text}", row.index)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Técnico: ", muted()),
        Span::styled(sanitize_display(&row.name), muted()),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "c combinar   d padrão   s A2DP",
        muted(),
    )));
    lines
}

fn group_details(app: &App) -> Vec<Line<'static>> {
    let Some(row) = app.current_group() else {
        return vec![Line::from(Span::styled(
            "Nenhum grupo selecionado.",
            muted(),
        ))];
    };
    match row {
        GroupRow::Owned {
            slug,
            sink_name,
            friendly,
            children,
            module_id,
            is_default,
        } => {
            let default_text = if *is_default { "sim" } else { "não" };
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(sanitize_display(friendly), accent()),
                    if *is_default {
                        Span::styled("  [padrão]", amber())
                    } else {
                        Span::raw("")
                    },
                ]),
                Line::from(vec![
                    Span::styled("Grupo próprio", amber()),
                    Span::styled(
                        format!(
                            "   padrão: {default_text}   {} saída(s)",
                            children.len()
                        ),
                        muted(),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("Roteamento: ", amber()),
                    Span::styled(
                        "áudio distribuído para as saídas listadas; remoção exige fallback se for o padrão.",
                        muted(),
                    ),
                ]),
            ];
            if children.is_empty() {
                lines.push(Line::from(Span::styled("Nenhuma saída listada.", muted())));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("- ", muted()),
                    Span::raw(sanitize_display(&children.join(", "))),
                ]));
            }
            lines.push(Line::from(if *is_default {
                Span::styled(
                    "x remover exige fallback disponível.",
                    Style::default().fg(AMBER),
                )
            } else {
                Span::styled("x remover grupo próprio", muted())
            }));

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Metadados", amber())));
            lines.push(Line::from(vec![
                Span::styled("Identificador: ", muted()),
                Span::raw(format!("audiomix_{slug}")),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Módulo: ", muted()),
                Span::raw(
                    module_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "desconhecido".to_string()),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Saída técnica: ", muted()),
                Span::styled(sanitize_display(sink_name), muted()),
            ]));
            lines
        }
        GroupRow::Foreign {
            name,
            friendly,
            is_default,
        } => {
            let default_text = if *is_default { "sim" } else { "não" };
            vec![
                Line::from(Span::styled(sanitize_display(friendly), accent())),
                Line::from(Span::styled(
                    "Saída virtual somente leitura",
                    Style::default().fg(AMBER),
                )),
                Line::from(vec![
                    Span::styled("Padrão: ", muted()),
                    Span::styled(default_text.to_string(), amber()),
                ]),
                Line::from(Span::styled(
                    "Este grupo não possui o marcador audiomix. Nunca será removido por esta interface.",
                    muted(),
                )),
                Line::from(""),
                Line::from(Span::styled("Metadados", amber())),
                Line::from(vec![
                    Span::styled("Técnico: ", muted()),
                    Span::styled(sanitize_display(name), muted()),
                ]),
            ]
        }
    }
}

fn draw_notification(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (text, style) = if let Some(job) = &app.job {
        let suffix = if app.exit_requested {
            "  (saindo ao concluir)"
        } else {
            ""
        };
        (
            format!("Executando: {}{suffix}", job.label()),
            Style::default().fg(AMBER),
        )
    } else if let Some(note) = &app.notification {
        (
            format!("[{}] {}", note.level.tag(), note.text),
            Style::default().fg(note.level.color()),
        )
    } else {
        ("Pronto.".to_string(), muted())
    };
    frame.render_widget(
        Paragraph::new(clip(&text, area.width))
            .style(style)
            .alignment(Alignment::Left),
        area,
    );
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, width: u16) {
    let (first, second) = if width >= 72 {
        (
            "j/k ou setas mover   Espaço marcar   Tab aba   ? ajuda",
            "c combinar   d padrão   s A2DP   x remover   r atualizar   q sair",
        )
    } else {
        (
            "j/k mover   Espaço marcar   Tab aba   ? ajuda",
            "c comb   d padrão   s A2DP   x rem   r atual   q sair",
        )
    };
    let lines = vec![
        Line::from(Span::styled(first.to_string(), muted())),
        Line::from(Span::styled(second.to_string(), muted())),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(bg_panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn panelled_block(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(TEAL))
        .style(bg_panel())
        .title(Span::styled(title, Style::default().fg(TEAL)))
}

struct ModalLayout {
    title: &'static str,
    body: Vec<Line<'static>>,
    footer: Vec<Line<'static>>,
    width: u16,
    focus_body_line: Option<usize>,
}

struct ModalView {
    title: &'static str,
    body: Paragraph<'static>,
    footer: Paragraph<'static>,
    rect: Rect,
    footer_height: u16,
    scroll: usize,
}

fn draw_modal(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(modal) = &app.modal else {
        return;
    };
    let view = modal_view(area, app, modal);
    let rect = view.rect;
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .style(Style::default().bg(PANEL_ALT).fg(TEXT))
        .title(Span::styled(format!(" {} ", view.title), amber()));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(view.footer_height)])
        .split(inner);
    frame.render_widget(view.body.scroll((view.scroll as u16, 0)), parts[0]);
    frame.render_widget(view.footer, parts[1]);
}

fn modal_view(area: Rect, app: &App, modal: &Modal) -> ModalView {
    let layout = modal_content(app, modal);
    let max_width = area.width.saturating_sub(4).max(20);
    let width = layout.width.min(max_width);
    let inner_width = width.saturating_sub(2).max(1);
    let focus_visual = layout.focus_body_line.and_then(|index| {
        layout.body.get(index).map(|line| {
            let prefix = Paragraph::new(layout.body[..index].to_vec())
                .wrap(Wrap { trim: false })
                .line_count(inner_width);
            let length = Paragraph::new(vec![line.clone()])
                .wrap(Wrap { trim: false })
                .line_count(inner_width)
                .max(1);
            (prefix, length)
        })
    });

    let body = Paragraph::new(layout.body)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(TEXT));
    let mut footer_lines = layout.footer;
    let body_lines = body.line_count(inner_width);
    let footer_rows = Paragraph::new(footer_lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(inner_width);

    let max_height = area.height.saturating_sub(2).max(6);
    let max_inner = max_height.saturating_sub(2) as usize;
    let body_view_guess = max_inner.saturating_sub(footer_rows).max(1);
    let needs_scroll = body_lines > body_view_guess;
    if needs_scroll {
        footer_lines.push(Line::from(Span::styled(
            "PgUp/PgDn rolar",
            Style::default().fg(AMBER),
        )));
    }

    let footer = Paragraph::new(footer_lines)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(TEXT));
    let footer_rows = footer.line_count(inner_width);
    let desired = body_lines.saturating_add(footer_rows).saturating_add(2);
    let height = if body_lines > max_inner.saturating_sub(footer_rows) || needs_scroll {
        max_height
    } else {
        (desired as u16).min(max_height).max(6)
    };
    let inner_height = height.saturating_sub(2) as usize;
    let footer_height = footer_rows.min(inner_height.saturating_sub(1));
    let body_view = inner_height.saturating_sub(footer_height).max(1);
    let max_scroll = body_lines.saturating_sub(body_view);

    let mut scroll = (app.modal_scroll as usize).min(max_scroll);
    if let Some((prefix, focus_len)) = focus_visual {
        if prefix < scroll {
            scroll = prefix;
        } else if prefix + focus_len > scroll + body_view {
            scroll = prefix + focus_len - body_view;
        }
        scroll = scroll.min(max_scroll);
    }

    ModalView {
        title: layout.title,
        body,
        footer,
        rect: centered(area, width, height),
        footer_height: footer_height as u16,
        scroll,
    }
}

pub fn clamp_scroll(app: &mut App, area: Rect) {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return;
    }
    if let Some(modal) = app.modal.as_ref() {
        app.modal_scroll = modal_view(area, app, modal).scroll.min(u16::MAX as usize) as u16;
    }
    let rows = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .split(area);
    let (_, details) = body_areas(rows[2]);
    let inner = panelled_block(String::new()).inner(details);
    let lines = match app.tab {
        Tab::Devices => device_details(app),
        Tab::Groups => group_details(app),
    };
    let maximum = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .line_count(inner.width)
        .saturating_sub(inner.height as usize);
    app.details_scroll = app
        .details_scroll
        .min(maximum.min(u16::MAX as usize) as u16);
}

fn modal_content(app: &App, modal: &Modal) -> ModalLayout {
    match modal {
        Modal::Help => {
            let mut body = vec![
                Line::from(Span::styled("Atalhos", amber())),
                Line::from(""),
                Line::from("j/k ou setas   navegar na lista"),
                Line::from("Espaço         marcar ou desmarcar saída física"),
                Line::from("Tab            alternar Dispositivos / Grupos"),
                Line::from("c              combinar saídas marcadas"),
                Line::from("d              definir saída padrão"),
                Line::from("s              ativar A2DP em saída Bluetooth"),
                Line::from("x              remover grupo próprio"),
                Line::from("r              atualizar leituras do servidor"),
                Line::from("?              abrir ou fechar esta ajuda"),
                Line::from("q / Ctrl-C     sair (conclui operação em andamento)"),
                Line::from(""),
                Line::from(Span::styled("Histórico recente", amber())),
            ];
            if app.history.is_empty() {
                body.push(Line::from(Span::styled("Nenhum evento ainda.", muted())));
            } else {
                for item in app.history.iter().rev() {
                    body.push(Line::from(Span::styled(sanitize_display(item), muted())));
                }
            }
            ModalLayout {
                title: "Ajuda",
                body,
                footer: vec![Line::from(Span::styled("Esc ou ? fecha", muted()))],
                width: 70,
                focus_body_line: None,
            }
        }
        Modal::CombineName { name, error } => {
            let mut body = vec![
                Line::from(Span::styled(
                    "Criar grupo combinado",
                    Style::default().fg(TEXT),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Nome do grupo (slug): use a-z, 0-9, _ e -.",
                    muted(),
                )),
                Line::from(vec![
                    Span::styled("Nome: ", muted()),
                    Span::styled(sanitize_display(name), amber()),
                ]),
            ];
            if let Some(err) = error {
                body.push(Line::from(Span::styled(
                    err.clone(),
                    Style::default().fg(RED),
                )));
            }
            ModalLayout {
                title: "Combinar saídas",
                body,
                footer: vec![Line::from(Span::styled(
                    "Enter continuar   Esc cancelar",
                    muted(),
                ))],
                width: 58,
                focus_body_line: None,
            }
        }
        Modal::CombineOptions {
            name,
            outputs,
            make_default,
            do_move,
            row,
        } => {
            let option = |on: bool, focused: bool, label: String| {
                let prefix = if focused { "> " } else { "  " };
                let box_mark = if on { "[x]" } else { "[ ]" };
                let style = if focused {
                    amber()
                } else {
                    Style::default().fg(TEXT)
                };
                Line::from(vec![
                    Span::styled(format!("{prefix}{box_mark} "), style),
                    Span::styled(label, style),
                ])
            };
            let body = vec![
                Line::from(Span::styled(
                    format!("Grupo: {}", sanitize_display(name)),
                    accent(),
                )),
                Line::from(Span::styled(
                    format!("{} saída(s) marcada(s)", outputs.len()),
                    muted(),
                )),
                Line::from(""),
                option(
                    *make_default,
                    *row == 0,
                    "Definir como saída padrão".to_string(),
                ),
                option(
                    *do_move,
                    *row == 1,
                    "Mover transmissões dos aplicativos agora".to_string(),
                ),
                Line::from(""),
                Line::from(Span::styled(
                    "Nenhuma alteração ocorre antes da confirmação final.",
                    muted(),
                )),
            ];
            ModalLayout {
                title: "Opções do grupo",
                body,
                footer: vec![Line::from(Span::styled(
                    "Tab/Espaço alterna   Enter revisar   Esc cancelar",
                    muted(),
                ))],
                width: 66,
                focus_body_line: None,
            }
        }
        Modal::ConfirmCombine {
            name,
            outputs,
            make_default,
            do_move,
        } => {
            let mut body = vec![
                Line::from(Span::styled(
                    "Confirme a criação do grupo",
                    Style::default().fg(TEXT),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Grupo: ", muted()),
                    Span::styled(format!("audiomix_{}", sanitize_display(name)), amber()),
                ]),
                Line::from(Span::styled("Saídas escolhidas:", amber())),
            ];
            for output in outputs {
                let sink = app.sinks.iter().find(|s| &s.name == output);
                let friendly = sink
                    .map(display_name)
                    .unwrap_or_else(|| sanitize_display(output));
                let index = sink
                    .map(|s| format!("#{}", s.index))
                    .unwrap_or_else(|| "#?".to_string());
                body.push(Line::from(vec![
                    Span::styled("- ", muted()),
                    Span::styled(friendly, Style::default().fg(TEXT)),
                    Span::styled(format!("  {index}"), muted()),
                ]));
                // O identificador exato fica visível por rolagem; nomes reais
                // podem exceder 100 colunas e quebrar em várias linhas.
                body.push(Line::from(Span::styled(
                    format!("  {}", sanitize_display(output)),
                    muted(),
                )));
            }
            let options = format!(
                "Padrão: {}   Mover transmissões: {}",
                if *make_default { "sim" } else { "não" },
                if *do_move { "sim" } else { "não" }
            );
            ModalLayout {
                title: "Revisar combinação",
                body,
                footer: vec![
                    Line::from(Span::styled(options, amber())),
                    Line::from(Span::styled(
                        "y confirmar   Esc/n cancelar",
                        Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                    )),
                ],
                width: 72,
                focus_body_line: None,
            }
        }
        Modal::ConfirmDefault {
            target,
            friendly,
            move_streams,
        } => {
            let body = vec![
                Line::from(Span::styled(
                    "Definir saída padrão",
                    Style::default().fg(TEXT),
                )),
                Line::from(""),
                Line::from(Span::styled(sanitize_display(friendly), accent())),
                Line::from(Span::styled(sanitize_display(target), muted())),
                Line::from(""),
                Line::from(vec![
                    Span::styled(if *move_streams { "[x] " } else { "[ ] " }, amber()),
                    Span::raw("Mover transmissões atuais dos aplicativos"),
                ]),
                Line::from(Span::styled("Espaço alterna a opção.", muted())),
            ];
            ModalLayout {
                title: "Saída padrão",
                body,
                footer: vec![Line::from(Span::styled(
                    "y confirmar   Esc/n cancelar",
                    Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                ))],
                width: 62,
                focus_body_line: None,
            }
        }
        Modal::ConfirmBluetooth { target, friendly } => {
            let _ = target;
            let body = vec![
                Line::from(Span::styled("Ativar A2DP", Style::default().fg(TEXT))),
                Line::from(""),
                Line::from(Span::styled(sanitize_display(friendly), accent())),
                Line::from(""),
                Line::from(Span::styled(
                    "Aviso: no modo A2DP o microfone do fone fica desativado.",
                    Style::default().fg(AMBER),
                )),
                Line::from(Span::styled(
                    "Mudanças de perfil podem exigir recriar grupos próprios que usem esta saída.",
                    Style::default().fg(AMBER),
                )),
            ];
            ModalLayout {
                title: "Bluetooth",
                body,
                footer: vec![Line::from(Span::styled(
                    "y confirmar   Esc/n cancelar",
                    Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                ))],
                width: 64,
                focus_body_line: None,
            }
        }
        Modal::RemoveFallback {
            slug,
            sink_name,
            friendly,
            candidates,
            cursor,
        } => {
            let _ = sink_name;
            let mut body = vec![
                Line::from(Span::styled(
                    format!("O grupo {} é a saída padrão.", sanitize_display(friendly)),
                    Style::default().fg(TEXT),
                )),
                Line::from(Span::styled(
                    format!("audiomix_{slug} precisa de um fallback seguro antes da remoção."),
                    muted(),
                )),
                Line::from(""),
                Line::from(Span::styled("Escolha a saída de fallback:", amber())),
            ];
            for (index, candidate) in candidates.iter().enumerate() {
                let focused = index == *cursor;
                body.push(Line::from(Span::styled(
                    format!(
                        "{} {}  ({})",
                        if focused { ">" } else { " " },
                        sanitize_display(&candidate.friendly),
                        sanitize_display(&candidate.name)
                    ),
                    if focused {
                        Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(TEXT)
                    },
                )));
            }
            ModalLayout {
                title: "Fallback necessário",
                body,
                footer: vec![Line::from(Span::styled(
                    "j/k escolher   Enter revisar   Esc cancelar",
                    muted(),
                ))],
                width: 68,
                focus_body_line: Some(4 + *cursor),
            }
        }
        Modal::ConfirmRemove {
            slug,
            sink_name,
            friendly,
            fallback,
        } => {
            let mut body = vec![
                Line::from(Span::styled(
                    "Remover grupo próprio",
                    Style::default().fg(TEXT),
                )),
                Line::from(""),
                Line::from(Span::styled(sanitize_display(friendly), accent())),
                Line::from(Span::styled(
                    format!("audiomix_{slug}  ({})", sanitize_display(sink_name)),
                    muted(),
                )),
                Line::from(""),
            ];
            match fallback {
                Some(name) => {
                    let friendly = app
                        .sinks
                        .iter()
                        .find(|s| &s.name == name)
                        .map(display_name)
                        .unwrap_or_else(|| sanitize_display(name));
                    body.push(Line::from(vec![
                        Span::styled("Fallback: ", muted()),
                        Span::styled(friendly, amber()),
                    ]));
                }
                None => {
                    body.push(Line::from(Span::styled(
                        "As transmissões serão redirecionadas para a saída padrão atual.",
                        muted(),
                    )));
                }
            }
            ModalLayout {
                title: "Revisar remoção",
                body,
                footer: vec![Line::from(Span::styled(
                    "y confirmar   Esc/n cancelar",
                    Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                ))],
                width: 66,
                focus_body_line: None,
            }
        }
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn short_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Bluetooth => "Bluetooth",
        Kind::Headphones => "Fones",
        Kind::Speakers => "Caixas",
        Kind::Usb => "USB",
        Kind::Hdmi => "HDMI",
        Kind::Virtual => "Virtual",
        Kind::Own => "Grupo",
        Kind::Unknown => "Outro",
    }
}

/// Remove controles de valores exibidos, sem alterar identificadores
/// canônicos usados nos comandos do backend.
fn sanitize_display(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

/// Recorta por células, nunca no meio de um caractere largo (CJK etc.).
fn clip(value: &str, width: u16) -> String {
    let max = width as usize;
    let mut used = 0;
    let mut out = String::new();
    let value = sanitize_display(value);
    let span = Span::raw(value);
    for grapheme in span.styled_graphemes(Style::default()) {
        let cells = grapheme.symbol.width();
        if used + cells > max {
            break;
        }
        out.push_str(grapheme.symbol);
        used += cells;
        if used >= max {
            break;
        }
    }
    out
}

#[cfg(test)]
mod server_contract_tests {
    use super::*;

    #[test]
    fn reads_real_pactl_info_keys_and_rejects_empty_objects() {
        let server = parse_server(
            r#"{"server_name":"PulseAudio (on PipeWire 1.0.5)","server_version":"15.0.0"}"#,
        )
        .unwrap();
        assert_eq!(
            server.name.as_deref(),
            Some("PulseAudio (on PipeWire 1.0.5)")
        );
        assert_eq!(server.version.as_deref(), Some("15.0.0"));
        assert!(server.connected);
        assert!(parse_server("{}").is_none());
        assert!(parse_server("[]").is_none());
        assert!(parse_server("bad json").is_none());
    }

    #[test]
    fn unexpected_worker_panic_becomes_recoverable_app_error() {
        struct PanickingBackend;
        impl Backend for PanickingBackend {
            fn run(&self, _args: &[&str]) -> Result<String> {
                panic!("simulated worker failure")
            }
        }
        let mut app = App::new("test");
        app.job = Some(JobSpec::Refresh);
        app.on_job_done(JobDone {
            spec: JobSpec::Refresh,
            outcome: execute_job_safely(JobSpec::Refresh, &PanickingBackend),
        });
        assert!(!app.busy());
        assert!(!app.should_quit);
        assert_eq!(app.notification.unwrap().level, Level::Error);
    }

    struct PanicAfterMutationBackend {
        mutated: std::cell::Cell<bool>,
    }

    impl PanicAfterMutationBackend {
        fn new() -> Self {
            Self {
                mutated: std::cell::Cell::new(false),
            }
        }
    }

    impl Backend for PanicAfterMutationBackend {
        fn run(&self, args: &[&str]) -> Result<String> {
            match args {
                ["-f", "json", "list", "sinks"] => Ok(r#"[{"index":1,"name":"alsa_output.test","state":"RUNNING","properties":{"device.description":"Test"}}]"#.into()),
                ["-f", "json", "list", "modules"] => Ok("[]".into()),
                ["list", "short", "modules"] => Ok(String::new()),
                ["get-default-sink"] => Ok("alsa_output.test".into()),
                ["-f", "json", "info"] => {
                    Ok(r#"{"server_name":"test","server_version":"1"}"#.into())
                }
                ["set-default-sink", _] => {
                    self.mutated.set(true);
                    panic!("simulated panic after mutation")
                }
                _ => Ok(String::new()),
            }
        }
    }

    #[test]
    fn mutation_panic_keeps_mutation_semantics_and_refreshes_state() {
        let backend = PanicAfterMutationBackend::new();
        let spec = JobSpec::SetDefault {
            target: "alsa_output.test".into(),
            move_streams: false,
        };
        let outcome = execute_job_safely(spec.clone(), &backend);
        assert!(backend.mutated.get());

        let mut app = App::new("test");
        app.job = Some(spec.clone());
        app.on_job_done(JobDone { spec, outcome });

        assert!(!app.busy());
        assert!(!app.should_quit);
        assert_eq!(app.notification.as_ref().unwrap().level, Level::Error);
        assert!(app
            .notification
            .as_ref()
            .unwrap()
            .text
            .contains("Operação falhou"));
        assert_eq!(app.devices.len(), 1);
        assert!(app.devices[0].name == "alsa_output.test");
    }

    #[test]
    fn mutation_panic_with_panicking_snapshot_still_clears_the_job() {
        struct AlwaysPanickingBackend;
        impl Backend for AlwaysPanickingBackend {
            fn run(&self, _args: &[&str]) -> Result<String> {
                panic!("simulated read failure")
            }
        }
        let spec = JobSpec::SetDefault {
            target: "alsa_output.test".into(),
            move_streams: false,
        };
        let outcome = execute_job_safely(spec.clone(), &AlwaysPanickingBackend);
        match &outcome {
            JobOutcome::Mutation {
                messages: Err(_),
                snapshot: Err(_),
            } => {}
            _ => panic!("mutation panic must keep Mutation semantics"),
        }

        let mut app = App::new("test");
        app.job = Some(spec.clone());
        app.on_job_done(JobDone { spec, outcome });

        assert!(!app.busy());
        assert!(!app.should_quit);
        assert_eq!(app.notification.as_ref().unwrap().level, Level::Error);
        assert!(!app.server.connected);
    }

    #[test]
    fn normal_and_focused_text_palettes_meet_readability_floor() {
        fn luminance(color: Color) -> f64 {
            let Color::Rgb(r, g, b) = color else {
                panic!("palette must use explicit RGB")
            };
            [r, g, b]
                .into_iter()
                .zip([0.2126, 0.7152, 0.0722])
                .map(|(c, weight)| {
                    let value = f64::from(c) / 255.0;
                    weight
                        * if value <= 0.04045 {
                            value / 12.92
                        } else {
                            ((value + 0.055) / 1.055).powf(2.4)
                        }
                })
                .sum()
        }
        for foreground in [TEXT, MUTED, TEAL, AMBER, RED, GREEN] {
            for background in [NAVY, PANEL, PANEL_ALT] {
                let ratio = (luminance(foreground) + 0.05) / (luminance(background) + 0.05);
                assert!(
                    ratio >= 4.5,
                    "Unreadable color pair: {foreground:?}/{background:?}: {ratio}"
                );
            }
        }
    }
}

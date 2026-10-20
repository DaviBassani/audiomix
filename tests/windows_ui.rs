//! Cross-platform tests for the pure Windows terminal UI.
//!
//! These tests never initialise WASAPI: they drive `App` and rendering with
//! `ratatui::backend::TestBackend` and use a non-interactive `CoreApi` mock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[cfg(not(windows))]
use audiomix::windows_app::RealCore;
use audiomix::windows_app::{
    draw, sanitize_display, spawn_session, App, CoreApi, Modal, Phase, Preset, UiAction, UiEvent,
    WindowsCli, WindowsCommand,
};
use audiomix::windows_audio::{Endpoint, MirrorStats, TargetHealth};
use clap::Parser;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn endpoint(id: &str, name: &str, is_default: bool) -> Endpoint {
    Endpoint {
        id: id.to_owned(),
        name: name.to_owned(),
        is_default,
        channels: Some(2),
        sample_rate: Some(48_000),
    }
}

fn endpoints() -> Vec<Endpoint> {
    vec![
        endpoint("spk-id", "Alto-falantes", true),
        endpoint("hdmi-id", "Monitor HDMI", false),
        endpoint("usb-id", "Fone USB", false),
    ]
}

fn app() -> App {
    let mut app = App::new("0.2.0");
    app.apply_endpoints(endpoints());
    app
}

fn key(app: &mut App, code: KeyCode) -> Option<UiAction> {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(app: &mut App, code: KeyCode) -> Option<UiAction> {
    app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL))
}

fn mark_first_target(app: &mut App) {
    key(app, KeyCode::Char(' '));
    assert_eq!(app.marked_targets(), vec!["hdmi-id".to_owned()]);
}

#[test]
fn stopped_session_can_restart_with_a_fresh_cancellation_token() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    let previous = app.stop_flag();
    key(&mut app, KeyCode::Char('x'));
    assert!(previous.load(Ordering::SeqCst));
    app.on_core_event(UiEvent::Stopped);
    key(&mut app, KeyCode::Char('c'));
    assert!(matches!(
        key(&mut app, KeyCode::Char('y')),
        Some(UiAction::Start { .. })
    ));
    assert!(!Arc::ptr_eq(&previous, &app.stop_flag()));
    assert!(!app.stop_flag().load(Ordering::SeqCst));
    assert!(previous.load(Ordering::SeqCst));
}

#[test]
fn failed_session_cannot_restart_or_exit_before_resource_cleanup() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    app.on_core_event(UiEvent::Error {
        message: "failure".into(),
        device_lost: false,
    });
    key(&mut app, KeyCode::Char('c'));
    assert!(app.modal().is_none());
    assert_eq!(key(&mut app, KeyCode::Char('q')), Some(UiAction::Stop));
    assert!(!app.should_quit());
    assert!(app.stop_flag().load(Ordering::SeqCst));
    app.on_core_event(UiEvent::Stopped);
    assert!(app.should_quit());
}

#[test]
fn hidden_confirmation_after_resize_does_not_start_audio() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    let yes = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(app.handle_terminal_key(yes, 40, 12).is_none());
    assert_eq!(app.phase(), Phase::Ready);
    assert!(app.modal().is_some());
    assert!(app.handle_terminal_key(yes, 80, 24).is_some());
}

#[test]
fn scroll_is_bounded_and_confirmation_footer_remains_visible() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    for _ in 0..100 {
        key(&mut app, KeyCode::PageDown);
    }
    audiomix::windows_app::clamp_scroll(&mut app, ratatui::layout::Rect::new(0, 0, 55, 18));
    let Some(Modal::Confirm { scroll }) = app.modal() else {
        panic!("missing confirmation")
    };
    let at_bottom = *scroll;
    assert!(at_bottom > 0 && at_bottom < 100);
    let text = render(&app, 55, 18);
    assert!(text.contains("y iniciar"));
    assert!(text.contains("muda."));
    key(&mut app, KeyCode::PageUp);
    let Some(Modal::Confirm { scroll }) = app.modal() else {
        panic!("missing confirmation")
    };
    assert_eq!(*scroll, at_bottom.saturating_sub(5));
}

#[test]
fn panicking_audio_adapter_reports_an_error_and_one_completion() {
    struct PanicCore;
    impl CoreApi for PanicCore {
        fn list_endpoints(&self) -> Result<Vec<Endpoint>, String> {
            Ok(endpoints())
        }
        fn start_mirror(
            &self,
            _: &str,
            _: &[String],
            _: Arc<AtomicBool>,
            _: mpsc::Sender<UiEvent>,
        ) -> Result<(), String> {
            panic!("mock panic")
        }
    }
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = spawn_session(
        Arc::new(PanicCore),
        "spk-id".into(),
        vec!["usb-id".into()],
        stop.clone(),
        tx,
    )
    .unwrap();
    worker.join().unwrap();
    let events: Vec<_> = rx.try_iter().collect();
    assert!(events.iter().any(|e| matches!(e, UiEvent::Error { .. })));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, UiEvent::Stopped))
            .count(),
        1
    );
    assert!(stop.load(Ordering::SeqCst));
}

fn started_event() -> UiEvent {
    UiEvent::Started {
        source: endpoint("spk-id", "Alto-falantes", true),
        targets: vec![endpoint("hdmi-id", "Monitor HDMI", false)],
    }
}

fn render(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn default_source_is_locked_and_targets_are_secondary_only() {
    let mut app = app();
    assert_eq!(app.source_id(), Some("spk-id"));
    assert_eq!(app.phase(), Phase::Ready);

    mark_first_target(&mut app);
    assert!(!app.is_marked("spk-id"));
    assert!(app.is_marked("hdmi-id"));

    // Cursor starts on the first secondary row; the locked source is never a
    // target even after moving through every row.
    for _ in 0..8 {
        key(&mut app, KeyCode::Char('k'));
        key(&mut app, KeyCode::Char(' '));
    }
    assert!(!app.is_marked("spk-id"));
    assert!(app.marked_targets().iter().all(|id| id != "spk-id"));
}

#[test]
fn duplicate_native_ids_are_defensively_deduplicated() {
    let mut app = App::new("0.2.0");
    app.apply_endpoints(vec![
        endpoint("same", "A", true),
        endpoint("same", "A duplicado", true),
        endpoint("target", "B", false),
    ]);
    assert_eq!(app.endpoints().len(), 2);
    assert!(app.history().iter().any(|m| m.contains("duplicados")));
    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.marked_targets(), vec!["target".to_owned()]);
}

#[test]
fn review_is_explicit_and_y_has_no_hidden_effect() {
    let mut app = app();
    mark_first_target(&mut app);

    // y outside the review screen changes nothing and starts nothing.
    assert_eq!(key(&mut app, KeyCode::Char('y')), None);
    assert!(app.modal().is_none());
    assert_eq!(app.phase(), Phase::Ready);
    assert_eq!(key(&mut app, KeyCode::Enter), None);

    assert_eq!(key(&mut app, KeyCode::Char('c')), None);
    assert!(matches!(app.modal(), Some(Modal::Confirm { .. })));
    // Enter is not a hidden confirmation.
    assert_eq!(key(&mut app, KeyCode::Enter), None);
    assert_eq!(app.phase(), Phase::Ready);
    assert!(matches!(app.modal(), Some(Modal::Confirm { .. })));

    assert_eq!(
        key(&mut app, KeyCode::Char('y')),
        Some(UiAction::Start {
            source: "spk-id".to_owned(),
            targets: vec!["hdmi-id".to_owned()],
        })
    );
    assert_eq!(app.phase(), Phase::Starting);
    assert!(app.modal().is_none());
}

#[test]
fn running_phase_requires_the_started_event() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    assert_eq!(app.phase(), Phase::Starting);
    assert!(!app.stats().running);

    app.on_core_event(started_event());
    assert_eq!(app.phase(), Phase::Running);
    assert!(app.stats().running);
    assert_eq!(app.marked_targets(), vec!["hdmi-id".to_owned()]);
}

#[test]
fn cancel_keeps_marks_and_never_starts() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(key(&mut app, KeyCode::Esc), None);
    assert!(app.modal().is_none());
    assert_eq!(app.phase(), Phase::Ready);
    assert_eq!(app.marked_targets(), vec!["hdmi-id".to_owned()]);

    key(&mut app, KeyCode::Char('c'));
    assert_eq!(key(&mut app, KeyCode::Char('n')), None);
    assert!(app.modal().is_none());
    assert_eq!(app.marked_targets(), vec!["hdmi-id".to_owned()]);
}

#[test]
fn review_refuses_without_a_secondary_target() {
    let mut app = app();
    key(&mut app, KeyCode::Char('c'));
    assert!(app.modal().is_none());
    assert!(app.notice().unwrap().contains("Marque ao menos uma"));
    assert_eq!(app.phase(), Phase::Ready);
}

#[test]
fn busy_session_blocks_mutations_and_x_stops() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    app.on_core_event(started_event());

    let before = app.marked_targets();
    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.marked_targets(), before);
    key(&mut app, KeyCode::Char('c'));
    assert!(app.modal().is_none());
    assert_eq!(key(&mut app, KeyCode::Char('r')), None);
    assert!(app.phase() == Phase::Running);

    assert_eq!(key(&mut app, KeyCode::Char('x')), Some(UiAction::Stop));
    assert_eq!(app.phase(), Phase::Stopping);
    assert!(app.stop_flag().load(Ordering::SeqCst));
    app.on_core_event(UiEvent::Stopped);
    assert_eq!(app.phase(), Phase::Ready);
    assert!(!app.should_quit());
}

#[test]
fn ctrl_c_requests_stop_and_quits_only_after_stopped() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    app.on_core_event(started_event());

    assert_eq!(ctrl(&mut app, KeyCode::Char('c')), Some(UiAction::Stop));
    assert!(app.quit_pending());
    assert!(!app.should_quit());
    assert_eq!(app.phase(), Phase::Stopping);

    app.on_core_event(UiEvent::Stopped);
    assert!(app.should_quit());
}

#[test]
fn idle_q_quits_without_touching_audio() {
    let mut app = app();
    assert_eq!(key(&mut app, KeyCode::Char('q')), Some(UiAction::Quit));
    assert!(app.should_quit());
    assert!(!app.stop_flag().load(Ordering::SeqCst));
}

#[test]
fn error_keeps_the_app_alive_and_keeps_the_full_message() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    app.on_core_event(started_event());
    let full = "Falha completa: endpoint removido durante a reproducao; \
                selecione outra saida e tente novamente (codigo 0x88890004)"
        .to_string();
    app.on_core_event(UiEvent::Error {
        message: full.clone(),
        device_lost: false,
    });

    assert_eq!(app.phase(), Phase::Failed);
    assert!(!app.should_quit());
    assert!(!app.stats().running);
    assert!(app.history().iter().any(|entry| entry == &full));
    assert!(app.marked_targets().len() == 1);

    key(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.modal(), Some(Modal::Help { .. })));
}

#[test]
fn disconnect_clears_marks_and_refresh_remap_clears_marks() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    app.on_core_event(started_event());

    app.on_core_event(UiEvent::Error {
        message: "A saida foi removida ou desativada.".to_owned(),
        device_lost: true,
    });
    assert!(app.marked_targets().is_empty());
    assert!(app
        .history()
        .iter()
        .any(|m| m.contains("marcacoes foram limpas")));

    let mut app = self::app();
    mark_first_target(&mut app);
    app.apply_endpoints(vec![
        endpoint("spk-id", "Alto-falantes", true),
        endpoint("hdmi-remapped", "Monitor HDMI (novo id)", false),
        endpoint("usb-id", "Fone USB", false),
    ]);
    assert!(app.marked_targets().is_empty());
    assert!(app
        .history()
        .iter()
        .any(|m| m.contains("desconectadas ou remapeadas")));
}

#[test]
fn default_source_change_clears_marks_and_relocks_source() {
    let mut app = app();
    mark_first_target(&mut app);
    app.apply_endpoints(vec![
        endpoint("spk-id", "Alto-falantes", false),
        endpoint("hdmi-id", "Monitor HDMI", true),
        endpoint("usb-id", "Fone USB", false),
    ]);
    assert_eq!(app.source_id(), Some("hdmi-id"));
    assert!(app.marked_targets().is_empty());
}

#[test]
fn explicit_preset_auto_starts_without_confirmation() {
    let mut app =
        App::new("0.2.0").with_preset(Preset::new("spk-id", vec!["usb-id".to_owned()]), true);
    app.apply_endpoints(endpoints());
    assert!(app.modal().is_none());
    assert_eq!(app.phase(), Phase::Starting);
    assert_eq!(
        app.take_auto_start(),
        Some(UiAction::Start {
            source: "spk-id".to_owned(),
            targets: vec!["usb-id".to_owned()],
        })
    );
    assert_eq!(app.take_auto_start(), None);
}

#[test]
fn preset_resolution_uses_ids_names_and_rejects_invalid_targets() {
    let preset = Preset::resolve(
        &endpoints(),
        "alto-falantes",
        &["monitor hdmi".to_owned(), "usb-id".to_owned()],
    )
    .unwrap();
    assert_eq!(preset.source_id, "spk-id");
    assert_eq!(preset.target_ids, vec!["hdmi-id", "usb-id"]);

    assert!(Preset::resolve(&endpoints(), "hdmi-id", &["usb-id".to_owned()]).is_err());
    assert!(Preset::resolve(
        &endpoints(),
        "spk-id",
        &["hdmi-id".to_owned(), "hdmi-id".to_owned()]
    )
    .is_err());
    assert!(Preset::resolve(&endpoints(), "spk-id", &["spk-id".to_owned()]).is_err());
    assert!(Preset::resolve(&endpoints(), "spk-id", &[]).is_err());
}

#[test]
fn renders_complete_layout_at_80x24() {
    let mut app = app();
    mark_first_target(&mut app);
    let text = render(&app, 80, 24);
    assert!(text.contains("AUDIOMIX"));
    assert!(text.contains("Windows / WASAPI"));
    assert!(text.contains("Origem [travada]"));
    assert!(text.contains("Alto-falantes"));
    assert!(text.contains("Saidas de audio"));
    assert!(text.contains("Detalhes e roteamento"));
    assert!(text.contains("SOMENTE"));
    assert!(text.contains("Configuracoes"));
    assert!(text.contains("y iniciar na revisao"));
    assert!(text.contains("q/Ctrl-C sair"));
    assert!(text.contains("[x] Monitor HDMI"));
}

#[test]
fn renders_meaningfully_at_55x18() {
    let mut app = app();
    mark_first_target(&mut app);
    let text = render(&app, 55, 18);
    assert!(text.contains("AUDIOMIX"));
    assert!(text.contains("Origem [travada]"));
    assert!(text.contains("Alvos"));
    assert!(text.contains("x parar"));
    assert!(text.contains("q sair"));
    assert!(!text.contains("Terminal muito pequeno"));
}

#[test]
fn smaller_size_shows_resize_guidance_and_no_hidden_action() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    let text = render(&app, 40, 12);
    assert!(text.contains("Terminal muito pequeno"));
    assert!(text.contains("Redimensione"));
    assert!(text.contains("55 x 18"));
    assert!(text.contains("Ctrl-C para sair"));
    assert!(!text.contains("y iniciar"));
}

#[test]
fn confirmation_wraps_long_names_and_keeps_footer_visible() {
    let mut app = App::new("0.2.0");
    let long_name = "Fone USB com um nome extremamente longo que precisa quebrar em varias linhas sem perder o final FIM";
    let long_id = "\\\\?\\SWD#MMDEVAPI#{0.0.0.00000000}.#{long-native-id-that-must-not-be-clipped-in-the-footer}";
    app.apply_endpoints(vec![
        endpoint("spk-id", "Alto-falantes", true),
        endpoint(long_id, long_name, false),
    ]);
    key(&mut app, KeyCode::Char(' '));
    key(&mut app, KeyCode::Char('c'));
    assert!(matches!(app.modal(), Some(Modal::Confirm { .. })));

    let text = render(&app, 55, 18);
    assert!(text.contains("Revisar espelhamento"));
    assert!(text.contains("Fone USB com um nome"));
    assert!(text.contains("y iniciar"));
    assert!(text.contains("Esc/n cancelar"));
    assert!(text.contains("Nada e alterado"));
    key(&mut app, KeyCode::PageDown);
    let scrolled = render(&app, 55, 18);
    assert!(scrolled.contains("FIM"));
    assert!(scrolled.contains("y iniciar"));
}

#[test]
fn display_strings_are_sanitized_but_ids_stay_raw() {
    let raw_id = "id\u{1b}[31m\u{7}raw";
    let mut app = App::new("0.2.0");
    app.apply_endpoints(vec![
        endpoint("spk-id", "Alto-falantes", true),
        endpoint(raw_id, "Alvo\u{1b}[31m\nNome", false),
    ]);
    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.marked_targets(), vec![raw_id.to_owned()]);
    let text = render(&app, 80, 24);
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("Alvo[31mNome"));
    assert_eq!(sanitize_display(raw_id), "id[31mraw");
}

#[test]
fn help_is_scrollable_and_shows_full_errors_and_unsupported_capabilities() {
    let mut app = app();
    key(&mut app, KeyCode::Char('?'));
    let text = render(&app, 80, 24);
    assert!(text.contains("Ajuda"));
    assert!(text.contains("O que o AudioMix Windows NAO faz"));
    assert!(text.contains("Bluetooth/A2DP"));
    assert!(text.contains("pactl"));
    key(&mut app, KeyCode::Char('?'));
    assert!(app.modal().is_none());

    let full = "ERRO-INTEGRAL-".to_string() + &"detalhe ".repeat(40) + "FINAL-DO-ERRO";
    app.on_core_event(UiEvent::Error {
        message: full.clone(),
        device_lost: false,
    });
    key(&mut app, KeyCode::Char('?'));
    for _ in 0..2 {
        key(&mut app, KeyCode::PageDown);
    }
    let text = render(&app, 80, 24);
    assert!(text.contains("ERRO-INTEGRAL-"));
}

#[test]
fn stats_are_rendered_while_running() {
    let mut app = app();
    mark_first_target(&mut app);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('y'));
    app.on_core_event(started_event());
    app.on_core_event(UiEvent::Stats(MirrorStats {
        running: true,
        frames_captured: 1234,
        frames_rendered: 1200,
        overflow_frames: 7,
        underrun_frames: 3,
        last_error: None,
        targets: vec![TargetHealth {
            id: "hdmi-id".to_owned(),
            name: "Monitor HDMI".to_owned(),
            frames_rendered: 1200,
            overflow_frames: 7,
            underrun_frames: 3,
            last_error: None,
        }],
    }));
    let text = render(&app, 100, 30);
    assert!(text.contains("capturado 1234"));
    assert!(text.contains("renderizado 1200"));
    assert!(text.contains("quedas 7"));
    assert!(text.contains("underrun 3"));
}

#[test]
fn cli_parses_documented_windows_commands() {
    let cli = WindowsCli::try_parse_from(["audiomix"]).unwrap();
    assert!(cli.command.is_none());

    let cli = WindowsCli::try_parse_from(["audiomix", "list", "--json"]).unwrap();
    assert_eq!(cli.command, Some(WindowsCommand::List { json: true }));

    let cli = WindowsCli::try_parse_from([
        "audiomix",
        "mirror",
        "--source",
        "Alto-falantes",
        "Monitor HDMI",
        "Fone USB",
    ])
    .unwrap();
    assert_eq!(
        cli.command,
        Some(WindowsCommand::Mirror {
            source: "Alto-falantes".to_owned(),
            targets: vec!["Monitor HDMI".to_owned(), "Fone USB".to_owned()],
        })
    );

    assert!(WindowsCli::try_parse_from(["audiomix", "mirror", "--source", "x"]).is_err());
    for command in ["default", "stereo", "groups"] {
        let cli = WindowsCli::try_parse_from(["audiomix", command]).unwrap();
        assert!(cli
            .command
            .as_ref()
            .and_then(WindowsCommand::unsupported_message)
            .is_some());
    }
}

struct MockCore;

impl CoreApi for MockCore {
    fn list_endpoints(&self) -> Result<Vec<Endpoint>, String> {
        Ok(endpoints())
    }

    fn start_mirror(
        &self,
        _source: &str,
        _targets: &[String],
        stop: Arc<AtomicBool>,
        events: mpsc::Sender<UiEvent>,
    ) -> Result<(), String> {
        events
            .send(UiEvent::Started {
                source: endpoint("spk-id", "Alto-falantes", true),
                targets: vec![endpoint("hdmi-id", "Monitor HDMI", false)],
            })
            .map_err(|error| error.to_string())?;
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

#[test]
fn mock_core_session_runs_without_wasapi_and_cleans_up() {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = spawn_session(
        Arc::new(MockCore),
        "spk-id".to_owned(),
        vec!["hdmi-id".to_owned()],
        stop.clone(),
        tx,
    )
    .unwrap();

    let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(first, UiEvent::Started { .. }));

    stop.store(true, Ordering::SeqCst);
    let mut stopped = false;
    while let Ok(event) = rx.recv_timeout(Duration::from_secs(2)) {
        if matches!(event, UiEvent::Stopped) {
            stopped = true;
            break;
        }
    }
    handle.join().unwrap();
    assert!(stopped);
}

#[cfg(not(windows))]
#[test]
fn real_core_fails_explicitly_without_native_wasapi() {
    let error = RealCore.list_endpoints().unwrap_err();
    assert!(error.contains("WASAPI") || error.contains("Windows"));
}

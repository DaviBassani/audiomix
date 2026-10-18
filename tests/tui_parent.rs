use audiomix::combine::parse_owner;
use audiomix::model::{Module, Sink};
use audiomix::tui::{App, JobDone, JobOutcome, JobSpec, Modal, ServerInfo, Snapshot, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn snapshot() -> Snapshot {
    let sinks: Vec<Sink> = serde_json::from_str(include_str!("fixtures/sinks.json")).unwrap();
    let modules: Vec<Module> = serde_json::from_str(include_str!("fixtures/modules.json")).unwrap();
    Snapshot {
        owned_groups: modules
            .iter()
            .filter_map(|m| parse_owner(m, &sinks))
            .collect(),
        sinks,
        default_sink: Some("audiomix_sala".into()),
        server: ServerInfo {
            connected: true,
            name: Some("PipeWire QA".into()),
            version: None,
        },
        warnings: vec![],
    }
}

fn app() -> App {
    let mut app = App::new("0.2.0");
    app.apply_snapshot(snapshot());
    app
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn destructive_shortcut_never_uses_a_hidden_groups_cursor() {
    let mut app = app();
    key(&mut app, KeyCode::Char('x'));
    assert!(app.modal.is_none());
    assert!(!app.busy());
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.tab, Tab::Groups);
    key(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.modal, Some(Modal::RemoveFallback { .. })));
}

#[test]
fn default_shortcut_in_groups_targets_the_visible_group() {
    let mut app = app();
    app.device_cursor = 2; // Unrelated Bluetooth device remains the device cursor.
    app.tab = Tab::Groups;
    key(&mut app, KeyCode::Char('d'));
    assert!(
        matches!(&app.modal, Some(Modal::ConfirmDefault { target, .. }) if target == "audiomix_sala")
    );
    assert!(!app.busy());
}

#[test]
fn bluetooth_shortcut_does_not_target_a_hidden_device() {
    let mut app = app();
    app.device_cursor = 2;
    app.tab = Tab::Groups;
    key(&mut app, KeyCode::Char('s'));
    assert!(app.modal.is_none());
    assert!(!app.busy());
}

#[test]
fn scroll_focus_and_modal_transitions_remain_independent() {
    let mut app = app();
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.details_scroll, 4);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.details_scroll, 0);
    key(&mut app, KeyCode::Char('?'));
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.modal_scroll, 4);
    assert_eq!(app.details_scroll, 0);
    key(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert_eq!(app.modal_scroll, 0);
}

#[test]
fn stale_snapshot_is_explicit_and_a_fresh_snapshot_recovers() {
    let mut app = app();
    app.on_job_done(JobDone {
        spec: JobSpec::Refresh,
        outcome: JobOutcome::Refresh(Err("no server".into())),
    });
    assert!(!app.server.connected);
    assert!(!app.should_quit);
    assert!(!app.busy());
    app.on_job_done(JobDone {
        spec: JobSpec::Refresh,
        outcome: JobOutcome::Refresh(Ok(snapshot())),
    });
    assert!(app.server.connected);
}

#[test]
fn unsuccessful_operation_and_failed_refresh_both_remain_visible() {
    let mut app = app();
    app.on_job_done(JobDone {
        spec: JobSpec::Bluetooth {
            target: "bluez_output.unknown.1".into(),
        },
        outcome: JobOutcome::Mutation {
            messages: Err("profile failed".into()),
            snapshot: Err("refresh failed".into()),
        },
    });
    let note = &app.notification.as_ref().unwrap().text;
    assert!(note.contains("profile failed"));
    assert!(note.contains("refresh failed"));
    assert!(!app.server.connected);
    assert!(!app.busy());
    assert!(!app.should_quit);
}

#[test]
fn foreign_virtual_prefix_is_not_enough_to_enable_default_action() {
    let mut snap = snapshot();
    let foreign = snap
        .sinks
        .iter_mut()
        .find(|s| s.name == "fones_fio_bluetooth")
        .unwrap();
    foreign.name = "audiomix_foreign".into();
    let mut app = App::new("0.2.0");
    app.apply_snapshot(snap);
    app.device_cursor = app
        .devices
        .iter()
        .position(|s| s.name == "audiomix_foreign")
        .unwrap();
    key(&mut app, KeyCode::Char('d'));
    assert!(app.modal.is_none());
    assert!(!app.busy());
}

#[test]
fn a_confirmation_hidden_by_resize_cannot_execute() {
    let mut app = app();
    app.modal = Some(Modal::ConfirmDefault {
        target: "alsa_output.pci-analog".into(),
        friendly: "Wired".into(),
        move_streams: true,
    });
    let yes = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(app.handle_terminal_key(yes, 40, 12).is_none());
    assert!(!app.busy());
    assert!(app.modal.is_some());
    assert!(app.handle_terminal_key(yes, 80, 24).is_some());
    assert!(app.busy());
}

#[test]
fn repeated_scroll_at_the_bottom_does_not_make_page_up_appear_stuck() {
    let mut app = app();
    app.notify(
        audiomix::tui::Level::Error,
        format!("{} diagnostic-tail", "long error ".repeat(80)),
    );
    app.modal = Some(Modal::Help);
    app.modal_scroll = u16::MAX;
    audiomix::tui::clamp_scroll(&mut app, ratatui::layout::Rect::new(0, 0, 80, 24));
    let bottom = app.modal_scroll;
    assert!(bottom > 0 && bottom < 200);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| audiomix::tui::draw(frame, &app))
        .unwrap();
    let content: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        content.contains("diagnostic-tail"),
        "Full diagnostics must remain reachable"
    );
    key(&mut app, KeyCode::PageUp);
    assert_eq!(app.modal_scroll, bottom - 4);
}

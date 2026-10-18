use audiomix::combine::OwnedGroup;
use audiomix::devices::Kind;
use audiomix::tui::{
    App, GroupRow, JobDone, JobOutcome, JobSpec, Level, Modal, ServerInfo, Snapshot, UiAction,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

fn sink(
    index: u32,
    name: &str,
    description: &str,
    extra: serde_json::Value,
) -> audiomix::model::Sink {
    let mut base = json!({
        "index": index,
        "name": name,
        "state": "RUNNING",
        "description": description,
        "sample_specification": "44100Hz 2ch s16le",
        "active_port": "analog-output",
        "ports": [
            {"name": "analog-output", "type": "Headphones", "availability": "available", "description": "Headphones"}
        ],
        "properties": {"device.description": description}
    });
    let obj = base.as_object_mut().unwrap();
    if let Some(extra) = extra.as_object() {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    serde_json::from_value(base).unwrap()
}

fn snapshot(include_bt: bool, include_own: bool) -> Snapshot {
    let mut sinks = vec![
        sink(10, "alsa_output.pci-analog", "Fones com fio", json!({})),
        sink(
            12,
            "virtual_foreign",
            "Loopback Virtual",
            json!({"properties": {"device.description": "Loopback Virtual", "node.virtual": "true"}}),
        ),
    ];
    if include_bt {
        sinks.insert(
            1,
            sink(
                11,
                "bluez_output.AA_BB.1",
                "Fone BT",
                json!({"properties": {"device.description": "Fone BT", "device.api": "bluez5"}}),
            ),
        );
    }
    if include_own {
        sinks.push(sink(
            13,
            "audiomix_sala",
            "Simultaneous output",
            json!({"properties": {"device.description": "Simultaneous output", "audiomix.managed": "true"}}),
        ));
    }
    let owned_groups = if include_own {
        vec![OwnedGroup {
            slug: "sala".to_string(),
            sink_name: "audiomix_sala".to_string(),
            module_id: Some(42),
            slaves: vec!["alsa_output.pci-analog".to_string()],
        }]
    } else {
        Vec::new()
    };
    Snapshot {
        sinks,
        owned_groups,
        default_sink: Some(if include_own {
            "audiomix_sala".to_string()
        } else {
            "alsa_output.pci-analog".to_string()
        }),
        server: ServerInfo {
            connected: true,
            name: Some("PipeWire".to_string()),
            version: Some("1.0".to_string()),
        },
        warnings: Vec::new(),
    }
}

fn fixture_app() -> App {
    let mut app = App::new("0.2.0");
    app.apply_snapshot(snapshot(true, true));
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn mark_two(app: &mut App) {
    assert_eq!(app.device_cursor, 0);
    assert!(app.handle_key(key(KeyCode::Char(' '))).is_none());
    assert!(app.handle_key(key(KeyCode::Down)).is_none());
    assert!(app.handle_key(key(KeyCode::Char(' '))).is_none());
    assert_eq!(app.selected.len(), 2);
}

#[test]
fn space_marks_only_eligible_physical_outputs() {
    let mut app = fixture_app();
    app.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(app.selected, vec!["alsa_output.pci-analog".to_string()]);

    app.device_cursor = 2; // virtual_foreign
    assert!(app.handle_key(key(KeyCode::Char(' '))).is_none());
    assert_eq!(app.selected.len(), 1);
    assert_eq!(app.notification.as_ref().unwrap().level, Level::Warning);
}

#[test]
fn invalid_slug_never_submits_and_typing_q_does_not_quit() {
    let mut app = fixture_app();
    mark_two(&mut app);
    app.handle_key(key(KeyCode::Char('c')));
    assert!(matches!(app.modal, Some(Modal::CombineName { .. })));

    for c in ['S', 'a', 'l', 'a', '!'] {
        let action = app.handle_key(key(KeyCode::Char(c)));
        assert!(action.is_none());
        assert!(!app.should_quit);
    }
    assert!(matches!(
        &app.modal,
        Some(Modal::CombineName { name, .. }) if name == "Sala!"
    ));

    let action = app.handle_key(key(KeyCode::Enter));
    assert!(action.is_none());
    assert!(matches!(
        &app.modal,
        Some(Modal::CombineName { error: Some(_), .. })
    ));
    assert!(app.job.is_none());
}

#[test]
fn q_j_k_are_editable_text_in_slug_and_esc_cancels() {
    let mut app = fixture_app();
    mark_two(&mut app);
    app.handle_key(key(KeyCode::Char('c')));
    for c in ['q', 'j', 'k'] {
        assert!(app.handle_key(key(KeyCode::Char(c))).is_none());
        assert!(!app.should_quit);
    }
    assert!(matches!(
        &app.modal,
        Some(Modal::CombineName { name, .. }) if name == "qjk"
    ));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.modal.is_none());
    assert!(app.job.is_none());
    assert_eq!(app.selected.len(), 2);
}

#[test]
fn combine_requires_explicit_y_after_guidance_and_review() {
    let mut app = fixture_app();
    mark_two(&mut app);
    app.handle_key(key(KeyCode::Char('c')));
    for c in ['s', 'a', 'l', 'a'] {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert!(app.handle_key(key(KeyCode::Enter)).is_none());
    assert!(matches!(
        app.modal,
        Some(Modal::CombineOptions { row: 0, .. })
    ));
    assert!(app.job.is_none());

    // Alterna "mover transmissões" e revisa.
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(app.handle_key(key(KeyCode::Enter)).is_none());
    assert!(matches!(app.modal, Some(Modal::ConfirmCombine { .. })));
    assert!(app.job.is_none());

    assert!(app.handle_key(key(KeyCode::Char('q'))).is_none());
    assert!(app.job.is_none());

    let action = app.handle_key(key(KeyCode::Char('y')));
    match action {
        Some(UiAction::Submit(JobSpec::Combine {
            name,
            outputs,
            make_default,
            do_move,
        })) => {
            assert_eq!(name, "sala");
            assert_eq!(
                outputs,
                vec![
                    "alsa_output.pci-analog".to_string(),
                    "bluez_output.AA_BB.1".to_string()
                ]
            );
            assert!(make_default);
            assert!(!do_move);
        }
        other => panic!("ação inesperada: {other:?}"),
    }
    assert!(app.busy());
    assert_eq!(app.notification.as_ref().unwrap().level, Level::Info);
}

#[test]
fn default_and_bluetooth_confirm_before_submitting() {
    let mut app = fixture_app();
    assert!(app.handle_key(key(KeyCode::Char('d'))).is_none());
    assert!(matches!(
        app.modal,
        Some(Modal::ConfirmDefault {
            move_streams: false,
            ..
        })
    ));
    assert!(app.job.is_none());
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(matches!(
        app.modal,
        Some(Modal::ConfirmDefault {
            move_streams: true,
            ..
        })
    ));
    let action = app.handle_key(key(KeyCode::Char('y')));
    assert!(matches!(
        action,
        Some(UiAction::Submit(JobSpec::SetDefault { .. }))
    ));

    let mut app = fixture_app();
    app.device_cursor = 1; // Bluetooth
    app.handle_key(key(KeyCode::Char('s')));
    assert!(matches!(app.modal, Some(Modal::ConfirmBluetooth { .. })));
    assert!(app.job.is_none());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.modal.is_none());
    assert!(app.job.is_none());
}

#[test]
fn removal_only_accepts_owned_groups_and_requires_fallback_when_default() {
    let mut app = fixture_app();
    app.tab = audiomix::tui::Tab::Groups;
    assert!(matches!(app.groups[0], GroupRow::Owned { .. }));

    // O grupo é o padrão: fallback é obrigatório antes da confirmação.
    assert!(app.handle_key(key(KeyCode::Char('x'))).is_none());
    assert!(matches!(
        app.modal,
        Some(Modal::RemoveFallback { ref candidates, .. }) if !candidates.is_empty()
    ));
    assert!(app.job.is_none());

    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(
        app.modal,
        Some(Modal::ConfirmRemove {
            fallback: Some(_),
            ..
        })
    ));
    let action = app.handle_key(key(KeyCode::Char('y')));
    assert!(matches!(
        action,
        Some(UiAction::Submit(JobSpec::Remove { .. }))
    ));

    // Virtual estrangeiro: leitura apenas, nunca remoção.
    let mut app = fixture_app();
    app.tab = audiomix::tui::Tab::Groups;
    app.group_cursor = 1;
    assert!(matches!(app.groups[1], GroupRow::Foreign { .. }));
    assert!(app.handle_key(key(KeyCode::Char('x'))).is_none());
    assert!(app.modal.is_none());
    assert!(app.job.is_none());
    assert_eq!(app.notification.as_ref().unwrap().level, Level::Warning);
}

#[test]
fn busy_blocks_conflicting_actions_and_defers_exit_until_completion() {
    let mut app = fixture_app();
    mark_two(&mut app);
    app.handle_key(key(KeyCode::Char('c')));
    for c in ['s', 'a', 'l', 'a'] {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Enter));
    let action = app.handle_key(key(KeyCode::Char('y')));
    assert!(matches!(action, Some(UiAction::Submit(_))));
    assert!(app.busy());

    assert!(app.handle_key(key(KeyCode::Char('r'))).is_none());
    assert!(app.handle_key(key(KeyCode::Char('d'))).is_none());
    assert!(app.handle_key(key(KeyCode::Char('x'))).is_none());
    assert!(app.job.is_some());

    assert!(app.handle_key(key(KeyCode::Char('q'))).is_none());
    assert!(app.exit_requested);
    assert!(!app.should_quit);

    app.on_job_done(JobDone {
        spec: JobSpec::Refresh,
        outcome: JobOutcome::Refresh(Ok(snapshot(true, true))),
    });
    assert!(!app.busy());
    assert!(app.should_quit);
}

#[test]
fn backend_error_keeps_app_alive_and_refreshes_state_when_possible() {
    let mut app = fixture_app();
    app.on_job_done(JobDone {
        spec: JobSpec::SetDefault {
            target: "alsa_output.pci-analog".to_string(),
            move_streams: false,
        },
        outcome: JobOutcome::Mutation {
            messages: Err("pactl falhou".to_string()),
            snapshot: Ok(snapshot(false, false)),
        },
    });

    assert!(!app.should_quit);
    assert!(!app.busy());
    assert_eq!(app.notification.as_ref().unwrap().level, Level::Error);
    assert!(app
        .notification
        .as_ref()
        .unwrap()
        .text
        .contains("pactl falhou"));
    assert!(app.history.iter().any(|line| line.contains("pactl falhou")));
    assert!(app.devices.iter().all(|d| d.name != "bluez_output.AA_BB.1"));
}

#[test]
fn refresh_preserves_selection_and_focus_by_stable_name_and_clears_disconnects() {
    let mut app = fixture_app();
    app.selected = vec![
        "alsa_output.pci-analog".to_string(),
        "bluez_output.AA_BB.1".to_string(),
    ];
    app.device_cursor = 1; // Fone BT
    app.tab = audiomix::tui::Tab::Groups;

    app.apply_snapshot(snapshot(false, true));

    assert_eq!(app.selected, vec!["alsa_output.pci-analog".to_string()]);
    assert!(app.device_cursor < app.devices.len());
    assert_ne!(app.devices[app.device_cursor].name, "bluez_output.AA_BB.1");
    assert!(matches!(
        app.groups[app.group_cursor],
        GroupRow::Owned { ref slug, .. } if slug == "sala"
    ));
}

#[test]
fn tui_rejects_native_pulseaudio_filter_and_simultaneous_outputs() {
    let mut snap = snapshot(false, false);
    snap.sinks = vec![
        sink(
            10,
            "filter_output",
            "Filtro",
            json!({"properties": {"device.description": "Filtro", "device.class": "filter"}}),
        ),
        sink(
            11,
            "combined_output",
            "Simultaneous",
            json!({"properties": {"device.description": "Simultaneous output to Fones"}}),
        ),
        sink(12, "alsa_output.ok", "Normal", json!({})),
    ];
    snap.default_sink = Some("alsa_output.ok".to_string());
    let mut app = App::new("0.2.0");
    app.apply_snapshot(snap);

    for index in [0, 1] {
        app.device_cursor = index;
        assert_eq!(app.devices[index].kind, Kind::Virtual);
        assert!(!app.devices[index].selectable);
        assert!(app.handle_key(key(KeyCode::Char(' '))).is_none());
        assert!(app.selected.is_empty());
        assert_eq!(app.notification.as_ref().unwrap().level, Level::Warning);
    }

    app.device_cursor = 2;
    assert!(app.handle_key(key(KeyCode::Char(' '))).is_none());
    assert_eq!(app.selected, vec!["alsa_output.ok".to_string()]);
}

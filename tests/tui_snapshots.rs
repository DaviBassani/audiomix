#![cfg(target_os = "linux")]

use audiomix::combine::OwnedGroup;
use audiomix::model::Sink;
use audiomix::tui::{self, App, Modal, ServerInfo, Snapshot, Tab};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::json;

fn sink(index: u32, name: &str, description: &str, extra: serde_json::Value) -> Sink {
    let mut base = json!({
        "index": index,
        "name": name,
        "state": "RUNNING",
        "description": description,
        "sample_specification": "48000Hz 2ch s16le",
        "active_port": "analog-output",
        "ports": [{"name": "analog-output", "type": "Headphones", "availability": "available"}],
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

fn snapshot() -> Snapshot {
    Snapshot {
        sinks: vec![
            sink(10, "alsa_output.pci-analog", "Fones com fio", json!({})),
            sink(
                11,
                "bluez_output.AA_BB.1",
                "Fone BT",
                json!({"properties": {"device.description": "Fone BT", "device.api": "bluez5"}}),
            ),
            sink(
                12,
                "virtual_foreign",
                "Loopback Virtual",
                json!({"properties": {"device.description": "Loopback Virtual", "node.virtual": "true"}}),
            ),
            sink(
                13,
                "audiomix_sala",
                "Simultaneous output",
                json!({"properties": {"device.description": "Simultaneous output", "audiomix.managed": "true"}}),
            ),
        ],
        owned_groups: vec![OwnedGroup {
            slug: "sala".to_string(),
            sink_name: "audiomix_sala".to_string(),
            module_id: Some(42),
            slaves: vec![
                "alsa_output.pci-analog".to_string(),
                "bluez_output.AA_BB.1".to_string(),
            ],
        }],
        default_sink: Some("audiomix_sala".to_string()),
        server: ServerInfo {
            connected: true,
            name: Some("PipeWire".to_string()),
            version: Some("1.2.7".to_string()),
        },
        warnings: Vec::new(),
    }
}

fn fixture_app() -> App {
    let mut app = App::new("0.2.0");
    app.apply_snapshot(snapshot());
    app
}

fn render(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| tui::draw(frame, app)).unwrap();
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
fn renders_full_layout_at_100x30() {
    let mut app = fixture_app();
    app.selected = vec![
        "alsa_output.pci-analog".to_string(),
        "bluez_output.AA_BB.1".to_string(),
    ];
    let text = render(&app, 100, 30);

    assert!(text.contains("audiomix"));
    assert!(text.contains("audio workspace"));
    assert!(text.contains("0.2.0"));
    assert!(text.contains("Servidor: PipeWire 1.2.7"));
    assert!(text.contains("Padrão atual: Grupo sala"));
    assert!(text.contains("Dispositivos"));
    assert!(text.contains("Grupos"));
    assert!(text.contains("Fones com fio"));
    assert!(text.contains("Fone BT"));
    assert!(text.contains("[padrão]"));
    assert!(text.contains("Detalhes"));
    assert!(text.contains("Marcadas (2)") || text.contains("Saídas marcadas (2)"));
    assert!(text.contains("c combinar"));
    assert!(text.contains("q sair"));
    assert!(text.contains("Roteamento:"));
    assert!(text.contains("Marcadas (2)"));
}

#[test]
fn renders_compact_but_complete_at_80x24() {
    let text = render(&fixture_app(), 80, 24);
    assert!(text.contains("Dispositivos"));
    assert!(text.contains("Detalhes"));
    assert!(text.contains("Bluetooth"));
    assert!(text.contains("estéreo"));
    assert!(text.contains("Fones com fio"));
    assert!(text.contains("q sair"));
}

#[test]
fn stacks_list_and_details_at_55x18() {
    let text = render(&fixture_app(), 55, 18);
    assert!(text.contains("Dispositivos"));
    assert!(text.contains("Detalhes"));
    assert!(text.contains("Fones com fio"));
    assert!(!text.contains("Redimensione"));
}

#[test]
fn tiny_terminal_shows_resize_guidance_without_panicking() {
    let text = render(&fixture_app(), 40, 12);
    assert!(text.contains("Terminal muito pequeno"));
    assert!(text.contains("Redimensione para pelo menos 55 x 18"));
    assert!(text.contains("40 x 12"));
}

#[test]
fn loading_state_is_visible_before_initial_snapshot() {
    let mut app = App::new("0.2.0");
    app.job = Some(audiomix::tui::JobSpec::Refresh);
    let text = render(&app, 80, 24);
    assert!(text.contains("Carregando saídas de áudio"));
    assert!(text.contains("Executando: atualizando dispositivos"));
    assert!(text.contains("Servidor: consultando"));
    assert!(!text.contains("falha de conexão"));
}

#[test]
fn groups_tab_shows_owned_children_and_foreign_virtual_as_read_only() {
    let mut app = fixture_app();
    app.tab = Tab::Groups;
    let text = render(&app, 100, 30);
    assert!(text.contains("Grupos (2)"));
    assert!(text.contains("Grupo sala"));
    assert!(text.contains("Fones com fio"));
    assert!(text.contains("Fone BT"));
    assert!(text.contains("Loopback Virtual"));
    assert!(text.contains("virtual não gerenciado pelo audiomix"));
    assert!(text.contains("[leitura]"));
    assert!(text.contains("Remoção apenas") || text.contains("remover"));
}

#[test]
fn combine_review_modal_shows_exact_outputs_options_and_requires_y() {
    let mut app = fixture_app();
    app.selected = vec![
        "alsa_output.pci-analog".to_string(),
        "bluez_output.AA_BB.1".to_string(),
    ];
    app.modal = Some(Modal::ConfirmCombine {
        name: "sala".to_string(),
        outputs: app.selected.clone(),
        make_default: true,
        do_move: false,
    });
    let text = render(&app, 100, 30);

    assert!(text.contains("Revisar combinação"));
    assert!(text.contains("audiomix_sala"));
    assert!(text.contains("alsa_output.pci-analog"));
    assert!(text.contains("bluez_output.AA_BB.1"));
    assert!(text.contains("Fone BT"));
    assert!(text.contains("Padrão: sim"));
    assert!(text.contains("Mover transmissões: não"));
    assert!(text.contains("y confirmar"));
}

#[test]
fn help_modal_lists_shortcuts_and_history() {
    let mut app = fixture_app();
    app.notify(audiomix::tui::Level::Info, "evento de teste");
    app.modal = Some(Modal::Help);
    let text = render(&app, 100, 30);
    assert!(text.contains("Atalhos"));
    assert!(text.contains("Histórico recente"));
    assert!(text.contains("evento de teste"));
    assert!(text.contains("q / Ctrl-C"));
}

#[test]
fn server_connection_failure_is_reported_in_ui_not_fatal() {
    let mut app = fixture_app();
    let mut snap = snapshot();
    snap.server = ServerInfo {
        connected: false,
        name: None,
        version: None,
    };
    snap.warnings = vec!["Servidor: falha de conexão ao ler pactl info.".to_string()];
    app.apply_snapshot(snap);
    app.notify(audiomix::tui::Level::Warning, "Servidor: falha de conexão");
    let text = render(&app, 80, 24);
    assert!(text.contains("Servidor: indisponível (falha de conexão)"));
    assert!(text.contains("falha de conexão"));
    assert!(!app.should_quit);
}

fn long_alsa_name(index: u32) -> String {
    format!(
        "alsa_output.pci-0000_00_1f.3.analog-stereo.profile-{}-very-long-device-identifier-{}-end",
        "x".repeat(88),
        index
    )
}

fn long_app(count: u32) -> App {
    let sinks = (0..count)
        .map(|i| {
            sink(
                100 + i,
                &long_alsa_name(i),
                &format!("Saída Longa {:02}", i + 1),
                json!({}),
            )
        })
        .collect::<Vec<_>>();
    let mut app = App::new("0.2.0");
    app.apply_snapshot(Snapshot {
        sinks,
        owned_groups: Vec::new(),
        default_sink: Some(long_alsa_name(0)),
        server: ServerInfo {
            connected: true,
            name: Some("PipeWire".to_string()),
            version: Some("1.2.7".to_string()),
        },
        warnings: Vec::new(),
    });
    app
}

#[test]
fn long_targets_scroll_while_confirmation_footer_stays_visible() {
    let mut app = long_app(10);
    app.selected = app.devices.iter().take(8).map(|d| d.name.clone()).collect();
    app.modal = Some(Modal::ConfirmCombine {
        name: "longa".to_string(),
        outputs: app.selected.clone(),
        make_default: true,
        do_move: false,
    });

    for (width, height) in [(80, 24), (55, 18)] {
        app.modal_scroll = 0;
        let top = render(&app, width, height);
        assert!(top.contains("Saídas escolhidas"));
        assert!(
            top.contains("Padrão: sim"),
            "footer options missing at {width}x{height}"
        );
        assert!(
            top.contains("y confirmar"),
            "confirmation missing at {width}x{height}"
        );
        assert!(top.contains("Esc/n cancelar"));
        assert!(top.contains("PgUp/PgDn rolar"));

        app.modal_scroll = u16::MAX;
        let bottom = render(&app, width, height);
        assert!(
            bottom.contains("Saída Longa 08"),
            "last output unreachable at {width}x{height}"
        );
        assert!(
            bottom.contains("y confirmar"),
            "sticky footer scrolled away at {width}x{height}"
        );
    }
}

#[test]
fn fallback_scroll_keeps_focused_candidate_and_footer_visible_beyond_eight_items() {
    let mut app = fixture_app();
    let candidates = (1..=10)
        .map(|i| audiomix::tui::FallbackChoice {
            name: long_alsa_name(i),
            friendly: format!("Saída Fallback {i:02}"),
        })
        .collect::<Vec<_>>();
    app.modal = Some(Modal::RemoveFallback {
        slug: "sala".to_string(),
        sink_name: "audiomix_sala".to_string(),
        friendly: "Grupo sala".to_string(),
        candidates,
        cursor: 9,
    });
    app.modal_scroll = 0;
    let text = render(&app, 55, 18);
    assert!(text.contains("Fallback 10"));
    assert!(text.contains("Enter revisar"));
    assert!(text.contains("Esc cancelar"));
    assert!(text.contains("PgUp/PgDn rolar"));
}

#[test]
fn actionable_details_and_routing_precede_technical_metadata_at_55x18() {
    let mut app = long_app(5);
    app.selected = app.devices.iter().take(2).map(|d| d.name.clone()).collect();
    app.details_scroll = 0;
    let text = render(&app, 55, 18);
    assert!(text.contains("Marcadas (2)"));
    assert!(text.contains("Roteamento:"));
    assert!(text.contains("simultâneo"));

    app.details_scroll = u16::MAX;
    let bottom = render(&app, 80, 24);
    assert!(bottom.contains("Técnico:"));
}

#[test]
fn cjk_names_are_wrapped_by_terminal_cells_without_panicking() {
    let mut snap = snapshot();
    snap.sinks.push(sink(
        90,
        "alsa_output.cjk-device",
        "耳機漢字テスト長い名前デバイス",
        json!({}),
    ));
    snap.default_sink = Some("alsa_output.cjk-device".to_string());
    let mut app = App::new("0.2.0");
    app.apply_snapshot(snap);
    app.device_cursor = app.devices.len() - 1;

    for (width, height) in [(80, 24), (55, 18)] {
        let text = render(&app, width, height);
        // O buffer do TestBackend representa caracteres largos com uma célula
        // de continuação; removemos espaços para comparar o texto visível.
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!text.contains('�'));
        assert!(compact.contains("耳機漢字"));
    }
}

#[test]
fn focused_list_row_uses_high_contrast_panel_background_and_amber_marker() {
    let app = fixture_app();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| tui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();

    let mut focused_row = None;
    for y in 0..24 {
        let row_text: String = (0..80).map(|x| buffer[(x, y)].symbol()).collect();
        let has_marker = (0..80).any(|x| {
            buffer[(x, y)].symbol().starts_with('▌')
                && buffer[(x, y)].fg == ratatui::style::Color::Rgb(240, 181, 94)
        });
        let has_panel_bg =
            (0..80).any(|x| buffer[(x, y)].bg == ratatui::style::Color::Rgb(20, 35, 56));
        if has_marker && has_panel_bg && row_text.contains("Fones com fio") {
            focused_row = Some(y);
            break;
        }
    }
    let y = focused_row.expect("linha focada de alto contraste não encontrada");
    assert_eq!(buffer[(5, y)].bg, ratatui::style::Color::Rgb(20, 35, 56));
    assert_ne!(buffer[(5, y)].fg, ratatui::style::Color::Rgb(240, 181, 94));
}

#[test]
fn control_characters_are_sanitized_at_render_boundaries() {
    let mut snap = snapshot();
    snap.sinks
        .push(sink(90, "evil\u{1b}[31m_output", "Fone literal", json!({})));
    snap.default_sink = Some("missing\u{1b}[31m".to_string());
    snap.server.name = Some("PipeWire\u{1b}[2J".to_string());
    let mut app = App::new("0.2.0");
    app.apply_snapshot(snap);
    app.device_cursor = app.devices.len() - 1;
    app.details_scroll = u16::MAX;
    app.notify(audiomix::tui::Level::Error, "falha\u{1b}[2J no backend");

    let text = render(&app, 100, 30);
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "rendered buffer must not contain terminal controls"
    );
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("Fone literal"));
    assert!(text.contains("Técnico:"));
    assert!(text.contains("evil"));
    assert!(text.contains("falha"));

    app.modal = Some(Modal::Help);
    let help = render(&app, 100, 30);
    assert!(
        !help.chars().any(|c| c.is_control() && c != '\n'),
        "history/modal must not contain terminal controls"
    );
    assert!(help.contains("falha"));
}

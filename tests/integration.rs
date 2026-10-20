#![cfg(target_os = "linux")]

//! Testes de integração com backend `pactl` simulado (injeção) e um teste de
//! subprocesso com um `pactl` falso no PATH.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::Command as StdCommand;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use audiomix::backend::Backend;
use audiomix::commands;
use audiomix::devices::{availability_of, display_name, kind_of, Availability, Kind};

const SINKS: &str = include_str!("fixtures/sinks.json");
const MODULES: &str = include_str!("fixtures/modules.json");
const SHORT: &str = include_str!("fixtures/short_modules.txt");
const SINK_INPUTS: &str = include_str!("fixtures/sink_inputs.json");
const CARDS: &str = include_str!("fixtures/cards.json");

struct FakeBackend {
    sinks: RefCell<Value>,
    modules: RefCell<Value>,
    short: RefCell<String>,
    sink_inputs: RefCell<Value>,
    cards: RefCell<Value>,
    default: RefCell<String>,
    fail: RefCell<Vec<String>>,
    calls: RefCell<Vec<String>>,
    next_id: RefCell<u32>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            sinks: RefCell::new(json!([])),
            modules: RefCell::new(json!([])),
            short: RefCell::new(String::new()),
            sink_inputs: RefCell::new(json!([])),
            cards: RefCell::new(json!([])),
            default: RefCell::new(String::new()),
            fail: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
            next_id: RefCell::new(536870900),
        }
    }

    fn with_fixtures() -> Self {
        let b = Self::new();
        *b.sinks.borrow_mut() = serde_json::from_str(SINKS).unwrap();
        *b.modules.borrow_mut() = serde_json::from_str(MODULES).unwrap();
        *b.short.borrow_mut() = SHORT.to_string();
        *b.sink_inputs.borrow_mut() = serde_json::from_str(SINK_INPUTS).unwrap();
        *b.cards.borrow_mut() = serde_json::from_str(CARDS).unwrap();
        *b.default.borrow_mut() = "audiomix_sala".to_string();
        b
    }

    fn fail_on(&self, needle: &str) {
        self.fail.borrow_mut().push(needle.to_string());
    }

    fn calls_matching(&self, needle: &str) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.contains(needle))
            .cloned()
            .collect()
    }

    fn load_module(&self, argument: &str) -> Result<String> {
        let mut sink_name = String::new();
        for token in argument.split_whitespace() {
            if let Some(v) = token.strip_prefix("sink_name=") {
                sink_name = v.to_string();
            }
        }
        let id = *self.next_id.borrow();
        *self.next_id.borrow_mut() += 1;

        if let Value::Array(items) = &mut *self.modules.borrow_mut() {
            items.push(json!({
                "name": "module-combine-sink",
                "argument": argument,
                "properties": {}
            }));
        }
        if let Value::Array(items) = &mut *self.sinks.borrow_mut() {
            items.push(json!({
                "index": id,
                "name": sink_name,
                "state": "IDLE",
                "active_port": null,
                "properties": { "audiomix.managed": "true" },
                "ports": []
            }));
        }
        self.short
            .borrow_mut()
            .push_str(&format!("{id}\tmodule-combine-sink\t{argument}\n"));
        Ok(id.to_string())
    }
}

impl Backend for FakeBackend {
    fn run(&self, args: &[&str]) -> Result<String> {
        let key = args.join(" ");
        self.calls.borrow_mut().push(key.clone());
        if self.fail.borrow().iter().any(|f| {
            if let Some(needle) = f.strip_prefix("after-load:") {
                *self.next_id.borrow() > 536870900 && key.contains(needle)
            } else {
                key.contains(f.as_str())
            }
        }) {
            bail!("falha simulada: {key}");
        }
        match args {
            ["-f", "json", "list", "sinks"] => Ok(self.sinks.borrow().to_string()),
            ["-f", "json", "list", "modules"] => Ok(self.modules.borrow().to_string()),
            ["-f", "json", "list", "sink-inputs"] => Ok(self.sink_inputs.borrow().to_string()),
            ["-f", "json", "list", "cards"] => Ok(self.cards.borrow().to_string()),
            ["list", "short", "modules"] => Ok(self.short.borrow().clone()),
            ["get-default-sink"] => Ok(self.default.borrow().clone()),
            ["set-default-sink", name] => {
                *self.default.borrow_mut() = name.to_string();
                Ok(String::new())
            }
            ["unload-module", _id] => Ok(String::new()),
            ["set-card-profile", _card, _profile] => Ok(String::new()),
            ["move-sink-input", _idx, _sink] => Ok(String::new()),
            ["load-module", _module, argument] => self.load_module(argument),
            other => bail!("argumentos inesperados no fake: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Forma do JSON e derivação de tipo/disponibilidade
// ---------------------------------------------------------------------------

#[test]
fn json_shape_with_null_active_port_and_profiles_object() {
    let b = FakeBackend::with_fixtures();
    let sinks = commands::sinks(&b).unwrap();
    assert_eq!(sinks.len(), 6);

    let usb = sinks.iter().find(|s| s.index == 13).unwrap();
    assert!(
        usb.active_port.is_none(),
        "active_port nulo deve ser aceito"
    );

    let cards = commands::cards(&b).unwrap();
    let bt_card = cards.iter().find(|c| c.index == 50).unwrap();
    assert_eq!(bt_card.active_profile_name().as_deref(), Some("a2dp-sink"));
    assert!(bt_card.profiles.contains_key("headset-head-unit"));
    let alsa_card = cards.iter().find(|c| c.index == 51).unwrap();
    assert_eq!(alsa_card.active_profile_name().as_deref(), Some("HiFi"));
}

#[test]
fn kind_and_availability_derivation() {
    let b = FakeBackend::with_fixtures();
    let sinks = commands::sinks(&b).unwrap();

    let hp = sinks.iter().find(|s| s.index == 10).unwrap();
    assert_eq!(kind_of(hp), Kind::Headphones);
    assert!(display_name(hp).contains("Headphones"));
    assert_eq!(availability_of(hp), Availability::InUse);
    assert_eq!(hp.layout_pt(), "estéreo");

    let hdmi = sinks.iter().find(|s| s.index == 11).unwrap();
    assert_eq!(kind_of(hdmi), Kind::Hdmi);
    assert_eq!(availability_of(hdmi), Availability::Unavailable);

    let bt = sinks.iter().find(|s| s.index == 12).unwrap();
    assert_eq!(kind_of(bt), Kind::Bluetooth);

    let usb = sinks.iter().find(|s| s.index == 13).unwrap();
    assert_eq!(kind_of(usb), Kind::Usb);
    assert_eq!(usb.layout_pt(), "estéreo");

    let own = sinks.iter().find(|s| s.index == 14).unwrap();
    assert_eq!(kind_of(own), Kind::Own);

    // A saída virtual externa não é confundida com um grupo próprio.
    let external = sinks.iter().find(|s| s.index == 15).unwrap();
    assert_eq!(kind_of(external), Kind::Virtual);
}

// ---------------------------------------------------------------------------
// Posse
// ---------------------------------------------------------------------------

#[test]
fn ownership_detects_only_our_modules() {
    let b = FakeBackend::with_fixtures();
    let sinks = commands::sinks(&b).unwrap();
    let owned = commands::owned_groups(&b, &sinks).unwrap();

    let slugs: Vec<&str> = owned.iter().map(|g| g.slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["sala"],
        "apenas o módulo com marcador e prefixo válidos é próprio"
    );
    assert_eq!(owned[0].module_id, Some(536870913));
}

// ---------------------------------------------------------------------------
// Validação de combinação
// ---------------------------------------------------------------------------

#[test]
fn combine_rejects_duplicate_outputs() {
    let b = FakeBackend::with_fixtures();
    let err = commands::combine(
        &b,
        "novo",
        &[
            "alsa_output.pci-analog".into(),
            "alsa_output.pci-analog".into(),
        ],
        false,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("duplicada"));
    assert!(b.calls_matching("load-module").is_empty());
}

#[test]
fn combine_rejects_fewer_than_two() {
    let b = FakeBackend::with_fixtures();
    let err = commands::combine(&b, "novo", &["alsa_output.pci-analog".into()], false, false)
        .unwrap_err();
    assert!(err.to_string().contains("2 saídas"));
    assert!(b.calls_matching("load-module").is_empty());
}

#[test]
fn combine_rejects_invalid_slug() {
    let b = FakeBackend::with_fixtures();
    let err = commands::combine(
        &b,
        "Sala Nova",
        &[
            "alsa_output.pci-analog".into(),
            "bluez_output.AA_BB_CC.1".into(),
        ],
        false,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("inválido"));
}

#[test]
fn combine_never_combines_own_group() {
    let b = FakeBackend::with_fixtures();
    let err = commands::combine(
        &b,
        "novo",
        &["audiomix_sala".into(), "alsa_output.pci-analog".into()],
        false,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("grupo próprio"));
}

// ---------------------------------------------------------------------------
// Criação, rollback
// ---------------------------------------------------------------------------

#[test]
fn combine_creates_owned_group() {
    let b = FakeBackend::with_fixtures();
    commands::combine(
        &b,
        "novo",
        &[
            "alsa_output.pci-analog".into(),
            "bluez_output.AA_BB_CC.1".into(),
        ],
        false,
        false,
    )
    .unwrap();

    let loads = b.calls_matching("load-module");
    assert_eq!(loads.len(), 1);
    assert!(loads[0].contains("sink_name=audiomix_novo"));
    assert!(loads[0].contains("audiomix.managed=true"));
    assert!(loads[0].contains("slaves="));
    assert!(b.calls_matching("unload-module").is_empty());

    let sinks = commands::sinks(&b).unwrap();
    let owned = commands::owned_groups(&b, &sinks).unwrap();
    assert!(owned
        .iter()
        .any(|g| g.slug == "novo" && g.module_id.is_some()));
}

#[test]
fn combine_rolls_back_module_when_default_fails() {
    let b = FakeBackend::with_fixtures();
    b.fail_on("set-default-sink");

    let err = commands::combine(
        &b,
        "novo",
        &[
            "alsa_output.pci-analog".into(),
            "bluez_output.AA_BB_CC.1".into(),
        ],
        true,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("padrão"));

    // O módulo recém-criado deve ser desmontado para não ficar órfão.
    let unloads = b.calls_matching("unload-module");
    assert_eq!(unloads.len(), 1);
    assert!(unloads[0].contains("536870900"));
}

#[test]
fn combine_rolls_back_when_post_load_discovery_fails() {
    for failure in [
        "-f json list sinks",
        "-f json list modules",
        "list short modules",
    ] {
        let b = FakeBackend::with_fixtures();
        b.fail_on(&format!("after-load:{failure}"));
        let error =
            commands::combine(&b, "novo", &["10".into(), "12".into()], false, false).unwrap_err();
        assert!(error.to_string().contains("redescoberta"));
        assert_eq!(b.calls_matching("unload-module 536870900").len(), 1);
        assert!(b.calls_matching("set-default-sink").is_empty());
    }
}

#[test]
fn rollback_failure_is_not_reported_as_successful_cleanup() {
    let b = FakeBackend::with_fixtures();
    b.fail_on("set-default-sink");
    b.fail_on("unload-module");
    let error =
        commands::combine(&b, "novo", &["10".into(), "12".into()], true, false).unwrap_err();
    assert!(error.to_string().contains("desmontagem falhou"));
    assert!(!error.to_string().contains("nenhuma alteração permanece"));
}

#[test]
fn partial_stream_moves_return_error_and_preserve_combination() {
    let b = FakeBackend::with_fixtures();
    b.fail_on("move-sink-input 100");
    assert!(commands::combine(&b, "novo", &["10".into(), "12".into()], true, true).is_err());
    assert!(b.calls_matching("unload-module").is_empty());
    assert_eq!(*b.default.borrow(), "audiomix_novo");
    assert!(b.calls_matching("move-sink-input 101").is_empty());
}

#[test]
fn partial_stream_moves_return_error_after_default_change() {
    let b = FakeBackend::with_fixtures();
    b.fail_on("move-sink-input 100");
    assert!(commands::set_default(&b, "10", true).is_err());
    assert_eq!(*b.default.borrow(), "alsa_output.pci-analog");
    assert!(b.calls_matching("move-sink-input 101").is_empty());
}

// ---------------------------------------------------------------------------
// Remoção
// ---------------------------------------------------------------------------

#[test]
fn remove_as_default_uses_fallback_then_unloads() {
    let b = FakeBackend::with_fixtures();
    commands::remove(&b, "sala", Some("alsa_output.pci-analog")).unwrap();

    let calls = b.calls.borrow();
    let pos = |needle: &str| calls.iter().position(|c| c.contains(needle));

    let set_default = pos("set-default-sink alsa_output.pci-analog").expect("define fallback");
    let unload = pos("unload-module 536870913").expect("desmonta módulo");
    assert!(
        set_default < unload,
        "padrão deve mudar antes da desmontagem"
    );
    assert!(calls.iter().any(|c| c.contains("move-sink-input 100")));
    // Transmissão interna (client null / pulse.module.id) não é movida.
    assert!(!calls.iter().any(|c| c.contains("move-sink-input 101")));
}

#[test]
fn remove_stops_unload_when_stream_move_fails() {
    let b = FakeBackend::with_fixtures();
    b.fail_on("move-sink-input");
    let err = commands::remove(&b, "sala", Some("alsa_output.pci-analog")).unwrap_err();
    assert!(err.to_string().contains("desmontagem cancelada"));
    assert!(b.calls_matching("unload-module").is_empty());
}

#[test]
fn remove_refuses_foreign_module() {
    let b = FakeBackend::with_fixtures();
    let err = commands::remove(&b, "legado", None).unwrap_err();
    assert!(err.to_string().contains("nenhum grupo próprio"));
    assert!(b.calls_matching("unload-module").is_empty());
}

#[test]
fn remove_rejects_fallback_equal_to_target() {
    let b = FakeBackend::with_fixtures();
    let err = commands::remove(&b, "sala", Some("audiomix_sala")).unwrap_err();
    assert!(err.to_string().contains("próprio grupo"));
    assert!(b.calls_matching("unload-module").is_empty());
}

// ---------------------------------------------------------------------------
// Bluetooth / A2DP (comando `stereo` e fluxo interativo)
// ---------------------------------------------------------------------------

/// Backend só com saídas e placas customizadas (módulos/streams vazios).
fn custom_backend(sinks_json: Value, cards_json: Value) -> FakeBackend {
    let b = FakeBackend::new();
    *b.sinks.borrow_mut() = sinks_json;
    *b.cards.borrow_mut() = cards_json;
    b
}

fn bt_sink(index: u32, name: &str, card: &str) -> Value {
    json!({
        "index": index,
        "name": name,
        "state": "RUNNING",
        "properties": { "device.api": "bluez5", "device.name": card }
    })
}

fn bt_card(name: &str, active: &str, a2dp_available: bool) -> Value {
    json!({
        "index": 50,
        "name": name,
        "active_profile": active,
        "properties": { "device.api": "bluez5" },
        "profiles": {
            "off": { "available": true },
            "a2dp-sink": { "available": a2dp_available },
            "headset-head-unit": { "available": true }
        }
    })
}

#[test]
fn stereo_activates_a2dp_for_switches_profile() {
    let sinks = json!([bt_sink(12, "bluez_output.AA_BB_CC.1", "bluez_card.AA_BB")]);
    let cards = json!([bt_card("bluez_card.AA_BB", "off", true)]);
    let b = custom_backend(sinks, cards);

    commands::stereo(&b, "bluez_output.AA_BB_CC.1").unwrap();

    let calls = b.calls_matching("set-card-profile");
    assert_eq!(calls.len(), 1, "deve ativar o perfil A2DP");
    assert!(calls[0].contains("bluez_card.AA_BB"));
    assert!(calls[0].contains("a2dp-sink"));
}

#[test]
fn stereo_selects_the_right_card_among_two() {
    let sinks = json!([
        bt_sink(12, "bluez_output.AA.1", "bluez_card.AA"),
        bt_sink(13, "bluez_output.BB.1", "bluez_card.BB")
    ]);
    let cards = json!([
        bt_card("bluez_card.AA", "off", true),
        bt_card("bluez_card.BB", "off", true)
    ]);
    let b = custom_backend(sinks, cards);

    commands::stereo(&b, "bluez_output.BB.1").unwrap();

    let calls = b.calls_matching("set-card-profile");
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains("bluez_card.BB"));
    assert!(
        !calls[0].contains("bluez_card.AA "),
        "não deve tocar a outra placa"
    );
}

#[test]
fn stereo_existing_a2dp_is_noop() {
    let sinks = json!([bt_sink(12, "bluez_output.AA_BB_CC.1", "bluez_card.AA_BB")]);
    let cards = json!([bt_card("bluez_card.AA_BB", "a2dp-sink", true)]);
    let b = custom_backend(sinks, cards);

    commands::stereo(&b, "bluez_output.AA_BB_CC.1").unwrap();
    assert!(
        b.calls_matching("set-card-profile").is_empty(),
        "já em A2DP não deve alterar nada"
    );
}

#[test]
fn stereo_errors_when_no_a2dp_available() {
    let sinks = json!([bt_sink(12, "bluez_output.AA_BB_CC.1", "bluez_card.AA_BB")]);
    let cards = json!([bt_card("bluez_card.AA_BB", "off", false)]);
    let b = custom_backend(sinks, cards);

    let err = commands::stereo(&b, "bluez_output.AA_BB_CC.1").unwrap_err();
    assert!(err.to_string().contains("A2DP"));
    assert!(b.calls_matching("set-card-profile").is_empty());
}

#[test]
fn stereo_errors_on_non_bluetooth_output() {
    let b = FakeBackend::with_fixtures();
    let err = commands::stereo(&b, "alsa_output.pci-analog").unwrap_err();
    assert!(err.to_string().contains("não é Bluetooth"));
}

#[test]
fn stereo_preserves_existing_high_quality_a2dp_codec() {
    let b = custom_backend(
        json!([bt_sink(12, "bluez_output.AA_BB_CC.1", "bluez_card.AA_BB")]),
        json!([bt_card("bluez_card.AA_BB", "a2dp-sink-sbc_xq", true)]),
    );
    commands::stereo(&b, "12").unwrap();
    assert!(b.calls_matching("set-card-profile").is_empty());
}

#[test]
fn stereo_never_guesses_an_unrelated_single_bluetooth_card() {
    let b = custom_backend(
        json!([bt_sink(12, "bluez_output.AA_BB_CC.1", "bluez_card.AA_BB")]),
        json!([bt_card("bluez_card.DD_EE", "off", true)]),
    );
    assert!(commands::stereo(&b, "12").is_err());
    assert!(b.calls_matching("set-card-profile").is_empty());
}

#[test]
fn default_rejects_known_disconnected_output_before_mutation() {
    let b = FakeBackend::with_fixtures();
    assert!(commands::set_default(&b, "11", true).is_err());
    assert!(b.calls_matching("set-default-sink").is_empty());
    assert!(b.calls_matching("move-sink-input").is_empty());
}

// ---------------------------------------------------------------------------
// Remoção: escopo de transmissões
// ---------------------------------------------------------------------------

#[test]
fn remove_inactive_group_moves_zero_unrelated_streams() {
    let b = FakeBackend::with_fixtures();
    *b.default.borrow_mut() = "alsa_output.pci-analog".to_string();
    *b.sink_inputs.borrow_mut() = json!([
        { "index": 102, "client": "236", "sink": 10, "properties": { "application.name": "MPV" } }
    ]);

    commands::remove(&b, "sala", None).unwrap();

    assert!(
        b.calls_matching("move-sink-input").is_empty(),
        "nenhuma transmissão de outra saída deve ser movida"
    );
    assert_eq!(b.calls_matching("unload-module").len(), 1);
}

#[test]
fn remove_mixed_streams_moves_only_group_target() {
    let b = FakeBackend::with_fixtures();
    *b.default.borrow_mut() = "alsa_output.pci-analog".to_string();

    commands::remove(&b, "sala", None).unwrap();

    let calls = b.calls.borrow();
    assert!(
        calls.iter().any(|c| c.contains("move-sink-input 100")),
        "transmissão presa ao grupo deve ser movida"
    );
    assert!(
        !calls.iter().any(|c| c.contains("move-sink-input 101")),
        "transmissão interna não deve ser movida"
    );
    assert!(
        !calls.iter().any(|c| c.contains("move-sink-input 102")),
        "transmissão de outra saída não deve ser movida"
    );
}

#[test]
fn remove_propagates_default_lookup_failure_without_mutation() {
    let b = FakeBackend::with_fixtures();
    b.fail_on("get-default-sink");

    let err = commands::remove(&b, "sala", Some("alsa_output.pci-analog")).unwrap_err();
    assert!(err.to_string().contains("get-default-sink"));
    assert!(b.calls_matching("set-default-sink").is_empty());
    assert!(b.calls_matching("unload-module").is_empty());
    assert!(b.calls_matching("move-sink-input").is_empty());
}

#[test]
fn remove_active_default_requires_explicit_fallback() {
    let b = FakeBackend::with_fixtures(); // padrão = audiomix_sala
    let err = commands::remove(&b, "sala", None).unwrap_err();
    assert!(err.to_string().contains("fallback"));
    assert!(b.calls_matching("set-default-sink").is_empty());
    assert!(b.calls_matching("unload-module").is_empty());
}

#[test]
fn remove_rejects_unavailable_fallback() {
    let b = FakeBackend::with_fixtures(); // padrão = audiomix_sala
    let err = commands::remove(&b, "sala", Some("alsa_output.pci-hdmi")).unwrap_err();
    assert!(err.to_string().contains("indisponível"));
    assert!(b.calls_matching("set-default-sink").is_empty());
    assert!(b.calls_matching("unload-module").is_empty());
}

// ---------------------------------------------------------------------------
// Combinação: rejeições antes de mutar
// ---------------------------------------------------------------------------

#[test]
fn combine_rejects_virtual_output() {
    let b = FakeBackend::with_fixtures();
    let err = commands::combine(
        &b,
        "novo",
        &[
            "fones_fio_bluetooth".into(),
            "alsa_output.pci-analog".into(),
        ],
        false,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("virtual"));
    assert!(b.calls_matching("load-module").is_empty());
}

#[test]
fn combine_rejects_unavailable_output() {
    let b = FakeBackend::with_fixtures();
    let err = commands::combine(
        &b,
        "novo",
        &[
            "alsa_output.pci-hdmi".into(),
            "alsa_output.pci-analog".into(),
        ],
        false,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("indisponível"));
    assert!(b.calls_matching("load-module").is_empty());
}

#[test]
fn combine_rejects_native_pulseaudio_filter_and_simultaneous_outputs() {
    for properties in [
        json!({"device.class": "filter", "device.description": "Filtro"}),
        json!({"device.description": "Simultaneous output to Fones"}),
    ] {
        let sinks = json!([
            { "index": 20, "name": "alsa_output.pci-ok", "state": "RUNNING", "properties": { "device.description": "A" } },
            { "index": 21, "name": "combined_output", "state": "RUNNING", "properties": properties }
        ]);
        let b = custom_backend(sinks, json!([]));
        let err = commands::combine(
            &b,
            "novo",
            &["alsa_output.pci-ok".into(), "combined_output".into()],
            false,
            false,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("virtual"),
            "filter/combine sink must be rejected as virtual: {err}"
        );
        assert!(b.calls_matching("load-module").is_empty());
    }
}

#[test]
fn combine_rejects_unsafe_sink_name() {
    let sinks = json!([
        { "index": 20, "name": "alsa_output.pci-ok", "state": "RUNNING", "properties": { "device.description": "A" } },
        { "index": 21, "name": "bad name,with space", "state": "RUNNING", "properties": { "device.description": "B" } }
    ]);
    let b = custom_backend(sinks, json!([]));

    let err = commands::combine(
        &b,
        "novo",
        &["alsa_output.pci-ok".into(), "bad name,with space".into()],
        false,
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("inseguros"));
    assert!(b.calls_matching("load-module").is_empty());
}

// ---------------------------------------------------------------------------
// Subprocesso com pactl falso no PATH
// ---------------------------------------------------------------------------

#[test]
fn subprocess_uses_fake_pactl_from_path() {
    let dir = std::env::temp_dir().join(format!("audiomix-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let sinks_path: PathBuf = dir.join("sinks.json");
    std::fs::write(&sinks_path, SINKS).unwrap();

    let script_path = dir.join("pactl");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n  \"-f json list sinks\") cat \"{}\" ;;\n  \"-f json list modules\") printf '[]' ;;\n  \"list short modules\") printf '' ;;\n  \"-f json list sink-inputs\") printf '[]' ;;\n  \"-f json list cards\") printf '[]' ;;\n  \"get-default-sink\") printf 'alsa_output.pci-analog' ;;\n  *) printf '' ;;\nesac\n",
        sinks_path.display()
    );
    std::fs::write(&script_path, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();
    }

    let old_path = std::env::var("PATH").unwrap_or_default();
    let output = StdCommand::new(env!("CARGO_BIN_EXE_audiomix"))
        .args(["list", "--json"])
        .env("PATH", format!("{}:{}", dir.display(), old_path))
        .env_remove("AUDIOMIX_PACTL")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(&stdout).expect("saída JSON válida");
    assert!(parsed.is_array());
    assert!(stdout.contains("Fones com fio"));
}

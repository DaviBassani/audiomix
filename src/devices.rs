//! Derivação do tipo de dispositivo, disponibilidade e nome amigável.

use crate::model::Sink;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bluetooth,
    Headphones,
    Speakers,
    Usb,
    Hdmi,
    Virtual,
    Own,
    Unknown,
}

impl Kind {
    pub fn label_pt(self) -> &'static str {
        match self {
            Kind::Bluetooth => "Bluetooth",
            Kind::Headphones => "Fones (analógico)",
            Kind::Speakers => "Alto-falantes",
            Kind::Usb => "USB",
            Kind::Hdmi => "HDMI",
            Kind::Virtual => "Virtual",
            Kind::Own => "Grupo audiomix",
            Kind::Unknown => "Desconhecido",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    InUse,
    Unavailable,
    Unknown,
}

impl Availability {
    pub fn label_pt(self) -> &'static str {
        match self {
            Availability::Available => "disponível",
            Availability::InUse => "em uso",
            Availability::Unavailable => "indisponível",
            Availability::Unknown => "desconhecida",
        }
    }
}

/// Native PulseAudio exposes filter/combine sinks without PipeWire's
/// `node.virtual`; `device.class=filter` and the standard simultaneous-output
/// description are the stable signals used there.
fn is_filter_or_simultaneous(sink: &Sink) -> bool {
    if sink
        .prop("device.class")
        .is_some_and(|class| class.eq_ignore_ascii_case("filter"))
    {
        return true;
    }
    [
        sink.prop("device.description"),
        sink.prop("node.description"),
        sink.description.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|description| {
        description
            .to_ascii_lowercase()
            .contains("simultaneous output")
    })
}

fn port_type_is(sink: &Sink, needle: &str) -> bool {
    sink.ports.iter().any(|p| {
        p.port_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case(needle))
    })
}

/// Classifica a saída. A ordem das verificações importa:
/// Grupo proprio/virtual > Bluetooth > fones analogicos > HDMI > USB > alto-falantes.
pub fn kind_of(s: &Sink) -> Kind {
    let lname = s.name.to_ascii_lowercase();

    if lname.starts_with(crate::combine::OWN_PREFIX)
        && s.prop(crate::combine::MARKER_KEY) == Some(crate::combine::MARKER_VALUE)
    {
        return Kind::Own;
    }

    if s.prop("node.virtual") == Some("true")
        || s.properties.get("node.virtual").and_then(|v| v.as_bool()) == Some(true)
        || is_filter_or_simultaneous(s)
        || lname.contains("virtual")
        || lname.contains("null")
    {
        return Kind::Virtual;
    }

    if s.prop("device.api").is_some_and(|v| v.starts_with("bluez"))
        || lname.starts_with("bluez_output.")
    {
        return Kind::Bluetooth;
    }

    if let Some(port) = s.active_port_obj() {
        if port
            .port_type
            .as_deref()
            .is_some_and(|t| t.to_ascii_lowercase().contains("headphone"))
        {
            return Kind::Headphones;
        }
    }

    if lname.contains("hdmi") || port_type_is(s, "HDMI") {
        return Kind::Hdmi;
    }

    if lname.contains("usb") || s.prop("device.bus") == Some("usb") {
        return Kind::Usb;
    }

    if lname.contains("analog")
        || lname.contains("speaker")
        || lname.contains("pci")
        || port_type_is(s, "Speaker")
    {
        return Kind::Speakers;
    }

    Kind::Unknown
}

/// Disponibilidade combinando o estado da saída e a disponibilidade da porta ativa.
pub fn availability_of(s: &Sink) -> Availability {
    let running = s.state.as_deref() == Some("RUNNING");
    match s.active_port_obj().and_then(|p| p.availability.as_deref()) {
        Some("available") => {
            if running {
                Availability::InUse
            } else {
                Availability::Available
            }
        }
        Some("not available") => Availability::Unavailable,
        Some(_) => Availability::Unknown,
        None => {
            if running {
                Availability::InUse
            } else {
                Availability::Unknown
            }
        }
    }
}

/// Elegível para combinação: não é grupo próprio e a porta ativa não é
/// explicitamente indisponível.
pub fn selectable(s: &Sink) -> bool {
    !matches!(kind_of(s), Kind::Own | Kind::Virtual)
        && availability_of(s) != Availability::Unavailable
}

/// Nome amigável. Garante que fones analógicos apareçam como "Headphones".
pub fn display_name(s: &Sink) -> String {
    if kind_of(s) == Kind::Own {
        if let Some(slug) = s.name.strip_prefix(crate::combine::OWN_PREFIX) {
            if crate::combine::valid_slug(slug) {
                return format!("Grupo {slug}");
            }
        }
    }
    let base = s
        .prop("device.description")
        .or_else(|| s.prop("node.description"))
        .or(s.description.as_deref())
        .or_else(|| s.prop("node.nick"))
        .unwrap_or(s.name.as_str())
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();

    if let Some(port) = s.active_port_obj() {
        if let Some(t) = port.port_type.as_deref() {
            let tl = t.to_ascii_lowercase();
            if tl.contains("headphone") && !base.to_ascii_lowercase().contains("headphone") {
                return format!("{base} (Headphones)");
            }
            if let Some(d) = port.description.as_deref() {
                if !d.is_empty()
                    && !base.contains(d)
                    && (tl.contains("hdmi") || tl.contains("headset") || tl.contains("speaker"))
                {
                    let safe: String = d.chars().filter(|c| !c.is_control()).collect();
                    return format!("{base} ({safe})");
                }
            }
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_pulseaudio_filter_and_simultaneous_outputs_are_virtual() {
        let filter: Sink = serde_json::from_value(json!({
            "index": 1, "name": "filter_output",
            "properties": {"device.class": "filter", "device.description": "Filtro"}
        }))
        .unwrap();
        assert_eq!(kind_of(&filter), Kind::Virtual);
        assert!(!selectable(&filter));

        let simultaneous: Sink = serde_json::from_value(json!({
            "index": 2, "name": "combined_output",
            "properties": {"device.description": "Simultaneous output to Fones"}
        }))
        .unwrap();
        assert_eq!(kind_of(&simultaneous), Kind::Virtual);
        assert!(!selectable(&simultaneous));
    }

    #[test]
    fn own_marker_wins_over_filter_classification() {
        let own: Sink = serde_json::from_value(json!({
            "index": 3, "name": "audiomix_sala",
            "properties": {
                "device.class": "filter",
                "device.description": "Simultaneous output to Fones",
                "audiomix.managed": "true"
            }
        }))
        .unwrap();
        assert_eq!(kind_of(&own), Kind::Own);
        assert!(!selectable(&own));
    }

    #[test]
    fn virtual_outputs_are_never_physical_even_with_bluetooth_names() {
        for virtual_flag in [json!(true), json!("true")] {
            let sink: Sink = serde_json::from_value(json!({
                "index": 1, "name": "bluez_output.test",
                "properties": {"node.virtual": virtual_flag, "device.api": "bluez5"}
            }))
            .unwrap();
            assert_eq!(kind_of(&sink), Kind::Virtual);
            assert!(!selectable(&sink));
        }
    }

    #[test]
    fn friendly_labels_do_not_emit_terminal_control_characters() {
        let sink: Sink = serde_json::from_value(json!({
            "index": 1, "name": "test", "description": "Fone\u{1b}[31m\n"
        }))
        .unwrap();
        assert!(!display_name(&sink).chars().any(char::is_control));
    }

    #[test]
    fn own_group_uses_the_chosen_name_as_its_friendly_label() {
        let sink: Sink = serde_json::from_value(json!({
            "index": 1, "name": "audiomix_sala",
            "description": "Simultaneous output",
            "properties": {"audiomix.managed": "true"}
        }))
        .unwrap();
        assert_eq!(display_name(&sink), "Grupo sala");
    }
}

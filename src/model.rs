//! Estruturas desserializadas a partir das respostas `pactl -f json list ...`.
//!
//! Os tipos são deliberadamente tolerantes: campos ausentes ou com tipos
//! variáveis entre versões (PulseAudio vs. PipeWire) são modelados com
//! `Option`/`serde_json::Value` para nunca causar panic.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
pub struct Port {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "type")]
    pub port_type: Option<String>,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sink {
    pub index: u32,
    pub name: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub sample_specification: Option<String>,
    #[serde(default)]
    pub channel_map: Option<Value>,
    #[serde(default)]
    pub active_port: Option<String>,
    #[serde(default)]
    pub owner_module: Option<Value>,
    #[serde(default)]
    pub ports: Vec<Port>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CardProfile {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub available: Option<bool>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub sinks: Option<i64>,
    #[serde(default)]
    pub sources: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Card {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub name: String,
    /// Pode ser uma string ("HiFi") ou um objeto { "name": ... }.
    #[serde(default)]
    pub active_profile: Option<Value>,
    #[serde(default)]
    pub profiles: BTreeMap<String, CardProfile>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Module {
    /// Presente no PulseAudio; ausente no PipeWire (usar `list short modules`).
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub name: String,
    /// Pode ser `null` em alguns módulos.
    #[serde(default)]
    pub argument: Option<String>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SinkInput {
    pub index: u32,
    #[serde(default)]
    pub sink: Option<Value>,
    /// `null` para transmissões internas de combinação; string/número para apps.
    #[serde(default)]
    pub client: Option<Value>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
}

/// Lê uma propriedade textual de um mapa de propriedades do pactl.
pub fn prop<'a>(map: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(|v| v.as_str())
}

/// Desserializa uma resposta JSON do pactl.
pub fn parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        serde_json::from_str("[]").context("resposta JSON vazia do pactl")
    } else {
        serde_json::from_str(trimmed).context("resposta JSON inválida do pactl")
    }
}

fn channels_from_spec(spec: &str) -> Option<usize> {
    for tok in spec.split_whitespace() {
        if let Some(num) = tok.strip_suffix("ch") {
            if let Ok(v) = num.parse::<usize>() {
                return Some(v);
            }
        }
    }
    None
}

impl Sink {
    pub fn prop(&self, key: &str) -> Option<&str> {
        prop(&self.properties, key)
    }

    pub fn active_port_obj(&self) -> Option<&Port> {
        let active = self.active_port.as_deref()?;
        self.ports.iter().find(|p| p.name == active)
    }

    /// Número de canais, se conhecido (channel_map ou sample_specification).
    pub fn channels(&self) -> Option<usize> {
        match &self.channel_map {
            Some(Value::String(s)) if !s.is_empty() => Some(s.split(',').count()),
            Some(Value::Array(a)) if !a.is_empty() => Some(a.len()),
            _ => self
                .sample_specification
                .as_deref()
                .and_then(channels_from_spec),
        }
    }

    pub fn layout_pt(&self) -> &'static str {
        match self.channels() {
            Some(1) => "mono",
            Some(2) => "estéreo",
            Some(n) if n > 2 => "multicanal",
            Some(_) => "desconhecido",
            None => "desconhecido",
        }
    }
}

impl Card {
    pub fn active_profile_name(&self) -> Option<String> {
        match &self.active_profile {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Object(m)) => m.get("name").and_then(|v| v.as_str()).map(String::from),
            _ => None,
        }
    }

    pub fn is_bluetooth(&self) -> bool {
        prop(&self.properties, "device.api").is_some_and(|v| v.starts_with("bluez"))
            || self.name.to_ascii_lowercase().contains("bluez")
            || prop(&self.properties, "device.string").is_some_and(|v| v.starts_with("bluez"))
    }
}

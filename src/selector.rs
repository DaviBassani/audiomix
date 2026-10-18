//! Resolução de seletores: nome exato, índice ou nome amigável inequívoco.

use anyhow::{bail, Result};

use crate::devices::display_name;
use crate::model::Sink;

/// Resolve um seletor para exatamente uma saída, ou falha.
pub fn resolve<'a>(sinks: &'a [Sink], selector: &str) -> Result<&'a Sink> {
    let sel = selector.trim();
    if sel.is_empty() {
        bail!("seletor vazio");
    }

    // 1. Nome exato da saída (identificador técnico completo).
    if let Some(s) = sinks.iter().find(|s| s.name == sel) {
        return Ok(s);
    }

    // 2. Índice numérico.
    if let Ok(idx) = sel.parse::<u32>() {
        if let Some(s) = sinks.iter().find(|s| s.index == idx) {
            return Ok(s);
        }
    }

    let low = sel.to_ascii_lowercase();

    // 3. Nome amigável exato (sem distinção de maiúsculas).
    let exact: Vec<&Sink> = sinks
        .iter()
        .filter(|s| display_name(s).to_ascii_lowercase() == low)
        .collect();
    match exact.len() {
        1 => return Ok(exact[0]),
        n if n > 1 => bail!("seletor ambíguo `{sel}`: corresponde a {n} saídas"),
        _ => {}
    }

    // 4. Correspondência parcial única (nome amigável ou técnico).
    let partial: Vec<&Sink> = sinks
        .iter()
        .filter(|s| {
            display_name(s).to_ascii_lowercase().contains(&low)
                || s.name.to_ascii_lowercase().contains(&low)
        })
        .collect();
    match partial.len() {
        1 => Ok(partial[0]),
        n if n > 1 => bail!("seletor ambíguo `{sel}`: corresponde a {n} saídas"),
        _ => bail!("nenhuma saída corresponde a `{sel}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Sink;

    fn sink(index: u32, name: &str, desc: &str) -> Sink {
        let json = serde_json::json!({
            "index": index,
            "name": name,
            "properties": { "device.description": desc }
        });
        serde_json::from_value(json).unwrap()
    }

    fn sample() -> Vec<Sink> {
        vec![
            sink(10, "alsa_output.pci-analog", "Fones com fio"),
            sink(11, "alsa_output.usb-headset", "Headset USB"),
            sink(12, "bluez_output.AA_BB.1", "Fone BT"),
        ]
    }

    #[test]
    fn resolves_by_exact_name() {
        let s = sample();
        assert_eq!(resolve(&s, "alsa_output.usb-headset").unwrap().index, 11);
    }

    #[test]
    fn resolves_by_index() {
        let s = sample();
        assert_eq!(resolve(&s, "12").unwrap().name, "bluez_output.AA_BB.1");
    }

    #[test]
    fn resolves_by_friendly_name() {
        let s = sample();
        assert_eq!(resolve(&s, "Fone BT").unwrap().index, 12);
    }

    #[test]
    fn resolves_by_unique_substring() {
        let s = sample();
        assert_eq!(resolve(&s, "headset").unwrap().index, 11);
    }

    #[test]
    fn rejects_missing() {
        let s = sample();
        assert!(resolve(&s, "inexistente").is_err());
    }

    #[test]
    fn rejects_ambiguous() {
        let mut s = sample();
        s.push(sink(13, "alsa_output.pci-analog-2", "Fones com fio"));
        assert!(resolve(&s, "Fones com fio").is_err());
    }
}

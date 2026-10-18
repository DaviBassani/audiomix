//! Posse e identificação dos grupos próprios do audiomix.
//!
//! Um módulo só é considerado nosso quando, cumulativamente:
//!   1. o nome do módulo é exatamente `module-combine-sink`;
//!   2. o argumento contém um token `sink_name=audiomix_<slug>` com slug ASCII válido;
//!   3. o argumento contém o marcador exato `audiomix.managed=true` dentro do token
//!      `sink_properties=` (comparação por token, nunca substring solta);
//!   4. se a saída correspondente existir, ela carrega a propriedade
//!      `audiomix.managed=true`.
//!
//! IDs de módulo nunca são aceitos do usuário: são redescobertos a partir da
//! listagem (`index` no JSON do PulseAudio, ou `list short modules` no PipeWire).

use crate::model::{Module, Sink};

pub const OWN_PREFIX: &str = "audiomix_";
pub const MARKER_KEY: &str = "audiomix.managed";
pub const MARKER_VALUE: &str = "true";

/// Slug ASCII estrito: não vazio, <= 40 caracteres, [a-z0-9_-], sem começar/terminar
/// com separador.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 40
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        && slug
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && slug
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric())
}

#[derive(Debug, Clone)]
pub struct OwnedGroup {
    pub slug: String,
    pub sink_name: String,
    pub module_id: Option<u32>,
    pub slaves: Vec<String>,
}

/// Extrai a posse de um módulo; `None` quando o módulo não é claramente nosso.
pub fn parse_owner(module: &Module, sinks: &[Sink]) -> Option<OwnedGroup> {
    if module.name != "module-combine-sink" {
        return None;
    }
    let arg = module.argument.as_deref()?;
    let marker_token = format!("{MARKER_KEY}={MARKER_VALUE}");

    let mut sink_name: Option<&str> = None;
    let mut slaves: Option<&str> = None;
    let mut has_marker = false;
    let mut properties_seen = false;

    for token in arg.split_whitespace() {
        if let Some(v) = token.strip_prefix("sink_name=") {
            if sink_name.replace(v).is_some() {
                return None;
            }
        } else if let Some(v) = token.strip_prefix("slaves=") {
            slaves = Some(v);
        } else if let Some(v) = token.strip_prefix("sinks=") {
            slaves = Some(v);
        } else if let Some(v) = token.strip_prefix("sink_properties=") {
            if properties_seen {
                return None;
            }
            properties_seen = true;
            has_marker = v == marker_token;
        }
    }

    let sink_name = sink_name?;
    let slug = sink_name.strip_prefix(OWN_PREFIX)?;
    if !valid_slug(slug) {
        return None;
    }
    if !has_marker {
        return None;
    }

    // Confirmação cruzada na própria saída, quando ela estiver listada.
    if let Some(sink) = sinks.iter().find(|s| s.name == sink_name) {
        if sink.prop(MARKER_KEY) != Some(MARKER_VALUE) {
            return None;
        }
    }

    Some(OwnedGroup {
        slug: slug.to_string(),
        sink_name: sink_name.to_string(),
        module_id: module.index,
        slaves: slaves
            .map(|s| {
                s.split(',')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(name: &str, argument: &str) -> Module {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "argument": argument
        }))
        .unwrap()
    }

    fn no_sinks() -> Vec<Sink> {
        Vec::new()
    }

    #[test]
    fn accepts_our_wellformed_module() {
        let m = module(
            "module-combine-sink",
            "sink_name=audiomix_sala sink_properties=audiomix.managed=true slaves=a,b",
        );
        let g = parse_owner(&m, &no_sinks()).expect("deve reconhecer como próprio");
        assert_eq!(g.slug, "sala");
        assert_eq!(g.slaves, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn rejects_wrong_module_name() {
        let m = module(
            "module-loopback",
            "sink_name=audiomix_sala sink_properties=audiomix.managed=true",
        );
        assert!(parse_owner(&m, &no_sinks()).is_none());
    }

    #[test]
    fn rejects_missing_marker() {
        let m = module(
            "module-combine-sink",
            "sink_name=audiomix_legado sink_properties=device.description=Legado slaves=a,b",
        );
        assert!(parse_owner(&m, &no_sinks()).is_none());
    }

    #[test]
    fn rejects_marker_as_loose_substring() {
        let m = module(
            "module-combine-sink",
            "sink_name=audiomix_evil sink_properties=notaudiomix.managed=true slaves=a,b",
        );
        assert!(parse_owner(&m, &no_sinks()).is_none());
    }

    #[test]
    fn rejects_foreign_sink_name_prefix() {
        let m = module(
            "module-combine-sink",
            "sink_name=fones_fio_buetooh sink_properties=audiomix.managed=true slaves=a,b",
        );
        assert!(parse_owner(&m, &no_sinks()).is_none());
    }

    #[test]
    fn rejects_when_sink_lacks_marker_property() {
        let m = module(
            "module-combine-sink",
            "sink_name=audiomix_sala sink_properties=audiomix.managed=true slaves=a,b",
        );
        let sinks: Vec<Sink> = vec![serde_json::from_value(serde_json::json!({
            "index": 40,
            "name": "audiomix_sala",
            "properties": {}
        }))
        .unwrap()];
        assert!(parse_owner(&m, &sinks).is_none());
    }

    #[test]
    fn accepts_when_sink_has_marker_property() {
        let m = module(
            "module-combine-sink",
            "sink_name=audiomix_sala sink_properties=audiomix.managed=true slaves=a,b",
        );
        let sinks: Vec<Sink> = vec![serde_json::from_value(serde_json::json!({
            "index": 40,
            "name": "audiomix_sala",
            "properties": { "audiomix.managed": "true" }
        }))
        .unwrap()];
        assert!(parse_owner(&m, &sinks).is_some());
    }

    #[test]
    fn slug_validation() {
        assert!(valid_slug("sala"));
        assert!(valid_slug("fone-2_sala"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("Sala"));
        assert!(!valid_slug("-sala"));
        assert!(!valid_slug("sala-"));
        assert!(!valid_slug("sa la"));
        assert!(!valid_slug("sala.x"));
    }

    #[test]
    fn rejects_duplicate_identity_and_marker_properties() {
        for args in [
            "sink_name=foreign sink_name=audiomix_sala sink_properties=audiomix.managed=true",
            "sink_name=audiomix_sala sink_properties=foreign=true sink_properties=audiomix.managed=true",
            "sink_name=audiomix_sala sink_properties=audiomix.managed=true,other=value",
        ] {
            assert!(parse_owner(&module("module-combine-sink", args), &[]).is_none());
        }
    }
}

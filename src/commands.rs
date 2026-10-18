//! Implementação dos comandos não interativos e auxiliares compartilhados.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::backend::Backend;
use crate::cli::Command;
use crate::combine::{self, OwnedGroup, MARKER_KEY, MARKER_VALUE, OWN_PREFIX};
use crate::devices::{availability_of, display_name, kind_of, selectable, Availability, Kind};
use crate::model::{parse_json, Card, Module, Sink, SinkInput};
use crate::selector::resolve;

/// Entrada de `pactl list short modules`.
#[derive(Debug, Clone)]
pub struct ShortModule {
    pub id: u32,
    pub name: String,
    pub argument: Option<String>,
}

// ---------------------------------------------------------------------------
// Leituras do backend
// ---------------------------------------------------------------------------

pub fn sinks(backend: &dyn Backend) -> Result<Vec<Sink>> {
    parse_json(&backend.run(&["-f", "json", "list", "sinks"])?)
}

pub fn cards(backend: &dyn Backend) -> Result<Vec<Card>> {
    parse_json(&backend.run(&["-f", "json", "list", "cards"])?)
}

pub fn modules(backend: &dyn Backend) -> Result<Vec<Module>> {
    parse_json(&backend.run(&["-f", "json", "list", "modules"])?)
}

pub fn sink_inputs(backend: &dyn Backend) -> Result<Vec<SinkInput>> {
    parse_json(&backend.run(&["-f", "json", "list", "sink-inputs"])?)
}

/// Saída padrão atual; `None` quando não há ou o pactl não a informa.
pub fn default_sink(backend: &dyn Backend) -> Result<Option<String>> {
    let raw = backend.run(&["get-default-sink"])?;
    let name = raw.trim();
    Ok(if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    })
}

pub fn short_modules(backend: &dyn Backend) -> Result<Vec<ShortModule>> {
    let raw = backend.run(&["list", "short", "modules"])?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut it = line.split('\t');
        let id = match it.next().map(str::trim).and_then(|s| s.parse::<u32>().ok()) {
            Some(id) => id,
            None => continue,
        };
        let name = it.next().unwrap_or("").trim().to_string();
        let argument = it.next().map(|s| s.trim_end().to_string());
        out.push(ShortModule { id, name, argument });
    }
    Ok(out)
}

/// Descobre todos os grupos próprios, associando o ID do módulo com segurança.
pub fn owned_groups(backend: &dyn Backend, sinks: &[Sink]) -> Result<Vec<OwnedGroup>> {
    let mods = modules(backend)?;
    let shorts = short_modules(backend)?;
    let mut out = Vec::new();
    for module in &mods {
        if let Some(mut group) = combine::parse_owner(module, sinks) {
            let id = module.index.or_else(|| {
                shorts
                    .iter()
                    .find(|s| {
                        s.name == module.name && s.argument.as_deref() == module.argument.as_deref()
                    })
                    .map(|s| s.id)
            });
            group.module_id = id;
            out.push(group);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

pub fn list(backend: &dyn Backend, as_json: bool) -> Result<()> {
    let sinks = sinks(backend)?;
    let default = default_sink(backend)?;

    if as_json {
        let items: Vec<Value> = sinks
            .iter()
            .map(|s| {
                json!({
                    "index": s.index,
                    "name": s.name,
                    "friendly": display_name(s),
                    "kind": kind_of(s).label_pt(),
                    "availability": availability_of(s).label_pt(),
                    "layout": s.layout_pt(),
                    "default": Some(s.name.as_str()) == default.as_deref(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    match &default {
        Some(name) => println!("Saída padrão atual: {name}"),
        None => println!("Saída padrão atual: (nenhuma)"),
    }
    println!();

    if sinks.is_empty() {
        println!("Nenhuma saída de áudio encontrada.");
        return Ok(());
    }

    println!("Saídas disponíveis:");
    for s in &sinks {
        let mark = if Some(s.name.as_str()) == default.as_deref() {
            " *"
        } else {
            ""
        };
        println!(
            "  [{}] {}{} | {} ({}, {})",
            s.index,
            display_name(s),
            mark,
            kind_of(s).label_pt(),
            availability_of(s).label_pt(),
            s.layout_pt()
        );
        println!("       seletor: nome={}  |  índice={}", s.name, s.index);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// groups
// ---------------------------------------------------------------------------

pub fn groups(backend: &dyn Backend) -> Result<()> {
    let sinks = sinks(backend)?;
    let default = default_sink(backend)?;
    let owned = owned_groups(backend, &sinks)?;

    if owned.is_empty() {
        println!("Nenhum grupo próprio do audiomix foi encontrado.");
        return Ok(());
    }

    println!("Grupos próprios do audiomix:");
    for g in &owned {
        let mark = if default.as_deref() == Some(g.sink_name.as_str()) {
            " *padrão"
        } else {
            ""
        };
        let id = g
            .module_id
            .map(|i| i.to_string())
            .unwrap_or_else(|| "?".to_string());
        println!("  audiomix_{} -> {}{}", g.slug, g.sink_name, mark);
        println!(
            "       módulo: {} (module-combine-sink)  |  saídas: {}",
            id,
            if g.slaves.is_empty() {
                "(desconhecidas)".to_string()
            } else {
                g.slaves.join(", ")
            }
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// stereo
// ---------------------------------------------------------------------------

pub fn stereo(backend: &dyn Backend, selector: &str) -> Result<Vec<String>> {
    activate_bluetooth_for(backend, selector)
}

// ---------------------------------------------------------------------------
// default
// ---------------------------------------------------------------------------

pub fn set_default(
    backend: &dyn Backend,
    selector: &str,
    move_streams: bool,
) -> Result<Vec<String>> {
    let all_sinks = sinks(backend)?;
    let (name, disp) = {
        let target = resolve(&all_sinks, selector)?;
        if availability_of(target) == Availability::Unavailable {
            bail!("a saída selecionada está indisponível");
        }
        (target.name.clone(), display_name(target))
    };
    backend.run(&["set-default-sink", &name])?;
    let mut messages = vec![format!("Saída padrão definida: {disp} ({name})")];

    if move_streams {
        let report = move_streams_to(backend, &name)?;
        messages.extend(move_messages(&report));
        if !report.failed.is_empty() {
            bail!(
                "a saída padrão foi alterada, mas {} transmissão(ões) não puderam ser movidas: {}",
                report.failed.len(),
                report.failed.join("; ")
            );
        }
    }
    Ok(messages)
}

// ---------------------------------------------------------------------------
// combine
// ---------------------------------------------------------------------------

pub fn combine(
    backend: &dyn Backend,
    name: &str,
    outputs: &[String],
    make_default: bool,
    do_move: bool,
) -> Result<Vec<String>> {
    if !combine::valid_slug(name) {
        bail!("nome de grupo inválido `{name}`: use [a-z0-9_-], até 40 caracteres, sem começar/terminar com separador");
    }

    let all_sinks = sinks(backend)?;

    // Resolve e valida todos os seletores ANTES de qualquer mutação.
    let mut chosen: Vec<&Sink> = Vec::new();
    for sel in outputs {
        let s = resolve(&all_sinks, sel)?;
        if chosen.iter().any(|c| c.index == s.index) {
            bail!("saída duplicada: {} (índice {})", display_name(s), s.index);
        }
        match kind_of(s) {
            Kind::Own => bail!(
                "não é possível combinar o grupo próprio `{}` (evita aninhamento)",
                display_name(s)
            ),
            Kind::Virtual => bail!(
                "não é possível combinar a saída virtual `{}` (evita aninhamento de combinações)",
                display_name(s)
            ),
            _ => {}
        }
        if availability_of(s) == Availability::Unavailable {
            bail!(
                "a saída `{}` está indisponível e não pode ser combinada",
                display_name(s)
            );
        }
        if !safe_sink_name(&s.name) {
            bail!(
                "o nome técnico `{}` contém caracteres inseguros para os argumentos do módulo",
                s.name
            );
        }
        chosen.push(s);
    }
    if chosen.len() < 2 {
        bail!("são necessárias pelo menos 2 saídas distintas para combinar");
    }

    let sink_name = format!("{OWN_PREFIX}{name}");
    if all_sinks.iter().any(|s| s.name == sink_name) {
        bail!("já existe uma saída chamada `{sink_name}`");
    }
    let existing = owned_groups(backend, &all_sinks)?;
    if existing.iter().any(|g| g.slug == name) {
        bail!("já existe um grupo próprio chamado `audiomix_{name}`");
    }

    let slaves = chosen
        .iter()
        .map(|s| s.name.clone())
        .collect::<Vec<_>>()
        .join(",");
    let argument = format!(
        "sink_name={sink_name} sink_properties={MARKER_KEY}={MARKER_VALUE} slaves={slaves}"
    );

    let load_out = backend.run(&["load-module", "module-combine-sink", &argument])?;
    let returned_id = parse_returned_id(&load_out);

    // Redescobre o módulo recém-criado; nunca confia no ID devolvido.
    let discovery = sinks(backend).and_then(|after| owned_groups(backend, &after));
    let owned = match discovery {
        Ok(o) => o,
        Err(e) => {
            return match rollback_unload(backend, returned_id) {
                Ok(()) => Err(anyhow!(
                    "módulo criado, mas a redescoberta falhou ({e}); o módulo foi desmontado por segurança"
                )),
                Err(ce) => Err(anyhow!(
                    "módulo criado, mas a redescoberta falhou ({e}); a desmontagem de rollback TAMBÉM falhou: {ce}"
                )),
            };
        }
    };
    let group = match owned
        .iter()
        .find(|g| g.slug == name && g.module_id.is_some())
    {
        Some(g) => g.clone(),
        None => {
            let id = returned_id;
            return match rollback_unload(backend, id) {
                Ok(()) => Err(anyhow!(
                    "o grupo foi carregado mas não pôde ser confirmado como próprio; módulo desmontado por segurança"
                )),
                Err(ce) => Err(anyhow!(
                    "o grupo foi carregado mas não pôde ser confirmado como próprio; a desmontagem de rollback falhou: {ce}"
                )),
            };
        }
    };

    if make_default {
        if let Err(e) = backend.run(&["set-default-sink", &sink_name]) {
            return match rollback_unload(backend, group.module_id.or(returned_id)) {
                Ok(()) => Err(anyhow!(
                    "falha ao definir o grupo como padrão ({e}); o módulo criado foi desmontado e nenhuma alteração permanece"
                )),
                Err(ce) => Err(anyhow!(
                    "falha ao definir o grupo como padrão ({e}); a saída combinada PODE ter ficado órfã (desmontagem falhou: {ce})"
                )),
            };
        }
    }

    let mut messages = vec![format!(
        "Grupo criado: audiomix_{name} ({sink_name}) combinando {}",
        chosen
            .iter()
            .map(|s| display_name(s))
            .collect::<Vec<_>>()
            .join(", ")
    )];

    if do_move {
        let report = move_streams_to(backend, &sink_name)?;
        messages.extend(move_messages(&report));
        if !report.failed.is_empty() {
            bail!(
                "o grupo foi criado, mas {} transmissão(ões) não puderam ser movidas: {}",
                report.failed.len(),
                report.failed.join("; ")
            );
        }
    } else {
        messages.push(
            "Dica: use --move-streams para mover as transmissões dos aplicativos para o grupo."
                .into(),
        );
    }
    Ok(messages)
}

/// Extrai o ID devolvido por `load-module` (usado apenas para rollback local;
/// não é aceito de entrada do usuário).
fn parse_returned_id(out: &str) -> Option<u32> {
    out.trim().parse().ok()
}

/// Nome técnico seguro para compor o argumento `slaves=` do módulo.
/// Permitido: ASCII alfanumérico mais `. _ - :`.
fn safe_sink_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
}

/// Desmonta um módulo no rollback, reportando falha (não mascara o erro).
fn rollback_unload(backend: &dyn Backend, id: Option<u32>) -> Result<()> {
    match id {
        Some(id) => backend
            .run(&["unload-module", &id.to_string()])
            .map(|_| ())
            .map_err(|e| anyhow!("unload-module {id}: {e}")),
        None => bail!("sem ID de módulo conhecido para rollback"),
    }
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

pub fn remove(backend: &dyn Backend, slug: &str, fallback: Option<&str>) -> Result<Vec<String>> {
    if !combine::valid_slug(slug) {
        bail!("identificador de grupo inválido `{slug}`: use [a-z0-9_-]");
    }

    let all_sinks = sinks(backend)?;
    // Erros do pactl ao consultar o padrão NÃO são engolidos aqui.
    let default = default_sink(backend)?;
    let owned = owned_groups(backend, &all_sinks)?;

    let group = owned
        .iter()
        .find(|g| g.slug == slug)
        .ok_or_else(|| anyhow!("nenhum grupo próprio `audiomix_{slug}` encontrado"))?
        .clone();

    let target = group.sink_name.clone();
    let module_id = group
        .module_id
        .ok_or_else(|| anyhow!("não foi possível determinar o ID do módulo do grupo; desmontagem cancelada por segurança"))?;

    // Índice da saída do grupo (None se ela não existir na listagem).
    let group_sink_index = all_sinks.iter().find(|s| s.name == target).map(|s| s.index);

    let is_default = default.as_deref() == Some(target.as_str());

    // Fallback informado: obrigatório quando o grupo é o padrão.
    // Precisa existir, ser diferente do alvo e estar disponível.
    let provided_fallback = match fallback {
        Some(sel) => {
            let s = resolve(&all_sinks, sel)?;
            if s.name == target {
                bail!("o fallback não pode ser o próprio grupo");
            }
            if availability_of(s) == Availability::Unavailable {
                bail!(
                    "o fallback `{}` está indisponível e não pode receber as transmissões",
                    display_name(s)
                );
            }
            Some(s.name.clone())
        }
        None => None,
    };

    // Destino das transmissões/padrão. Nunca escolhe automaticamente uma saída
    // desconhecida/desconectada para o caso padrão.
    let destination: Option<String> = if is_default {
        Some(provided_fallback.clone().ok_or_else(|| {
            anyhow!(
                "o grupo é a saída padrão; informe --fallback com uma saída existente e disponível"
            )
        })?)
    } else {
        provided_fallback.clone().or_else(|| default.clone())
    };

    if group_sink_index.is_some() && destination.is_none() {
        bail!("não há destino seguro para as transmissões do grupo; informe --fallback");
    }

    let mut messages = Vec::new();
    // 1. Se o grupo é o padrão, troca a saída padrão ANTES de qualquer desmontagem.
    if is_default {
        let dest = destination.clone().unwrap_or_default();
        backend.run(&["set-default-sink", &dest]).map_err(|e| {
            anyhow!("falha ao definir a saída padrão de fallback ({e}); desmontagem cancelada")
        })?;
        messages.push(format!("Saída padrão alterada para: {dest}"));
    }

    // 2. Move SOMENTE as transmissões presas à saída do grupo antes de desmontar.
    let report = match &destination {
        Some(dest) => move_streams_from_group(backend, dest, group_sink_index)?,
        None => MoveReport::default(),
    };
    if !report.failed.is_empty() {
        bail!(
            "não foi possível mover todas as transmissões do grupo ({} falha(s)); desmontagem cancelada para segurança: {}",
            report.failed.len(),
            report.failed.join("; ")
        );
    }
    messages.extend(move_messages(&report));

    // 3. Desmonta o módulo (ID redescoberto, nunca vindo do usuário).
    backend.run(&["unload-module", &module_id.to_string()])?;
    messages.push(format!("Grupo removido: audiomix_{slug} ({target})"));
    Ok(messages)
}

// ---------------------------------------------------------------------------
// Transmissões (streams)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct MoveReport {
    pub moved: usize,
    pub skipped_internal: usize,
    pub skipped_other: usize,
    pub failed: Vec<String>,
}

/// `true` para transmissões internas de combinação (sem cliente ou com módulo
/// associando os streams ao próprio combine-sink).
fn is_internal_stream(si: &SinkInput) -> bool {
    let app_client = match &si.client {
        Some(Value::String(s)) => !s.is_empty() && s != "-",
        Some(Value::Number(_)) => true,
        _ => false,
    };
    if !app_client {
        return true;
    }
    si.properties.contains_key("pulse.module.id") || si.properties.contains_key("node.group")
}

/// Índice da saída de destino de uma transmissão, se conhecido.
fn sink_index_of(si: &SinkInput) -> Option<u32> {
    match &si.sink {
        Some(Value::Number(n)) => n.as_u64().and_then(|v| u32::try_from(v).ok()),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
}

/// Move TODAS as transmissões de aplicativo para `target` (comportamento de
/// `default`/`combine`). Preserva transmissões internas de combinação.
pub fn move_streams_to(backend: &dyn Backend, target: &str) -> Result<MoveReport> {
    let inputs = sink_inputs(backend)?;
    let mut report = MoveReport::default();

    for si in inputs {
        if is_internal_stream(&si) {
            report.skipped_internal += 1;
            continue;
        }
        let idx = si.index.to_string();
        match backend.run(&["move-sink-input", &idx, target]) {
            Ok(_) => report.moved += 1,
            Err(e) => {
                let msg = e.to_string();
                let msg = msg.lines().next().unwrap_or(&msg).to_string();
                report.failed.push(format!("#{}: {msg}", si.index));
            }
        }
    }
    Ok(report)
}

/// Move SOMENTE as transmissões cuja saída atual é a do grupo removido.
///
/// Se `group_sink_index` for `None` (a saída do grupo já não existe), nenhuma
/// transmissão é tocada. Transmissões de outras saídas são ignoradas e
/// contabilizadas em `skipped_other`.
pub fn move_streams_from_group(
    backend: &dyn Backend,
    target: &str,
    group_sink_index: Option<u32>,
) -> Result<MoveReport> {
    let inputs = sink_inputs(backend)?;
    let mut report = MoveReport::default();

    for si in inputs {
        if is_internal_stream(&si) {
            report.skipped_internal += 1;
            continue;
        }
        let on_group = group_sink_index.is_some() && sink_index_of(&si) == group_sink_index;
        if !on_group {
            report.skipped_other += 1;
            continue;
        }
        let idx = si.index.to_string();
        match backend.run(&["move-sink-input", &idx, target]) {
            Ok(_) => report.moved += 1,
            Err(e) => {
                let msg = e.to_string();
                let msg = msg.lines().next().unwrap_or(&msg).to_string();
                report.failed.push(format!("#{}: {msg}", si.index));
            }
        }
    }
    Ok(report)
}

fn move_messages(report: &MoveReport) -> Vec<String> {
    let mut messages = Vec::new();
    if report.moved > 0 {
        messages.push(format!("Transmissões movidas: {}", report.moved));
    } else if report.failed.is_empty() {
        messages.push("Nenhuma transmissão de aplicativo para mover.".into());
    }
    if report.skipped_internal > 0 {
        messages.push(format!(
            "Transmissões internas de combinação preservadas: {}",
            report.skipped_internal
        ));
    }
    for f in &report.failed {
        messages.push(format!("aviso: falha ao mover transmissão {f}"));
    }
    messages
}

// ---------------------------------------------------------------------------
// Bluetooth A2DP
// ---------------------------------------------------------------------------

/// Normaliza um endereço/identificador Bluetooth para comparação.
fn normalize_addr(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Resolve a placa Bluetooth correspondente a uma saída.
fn find_bt_card<'a>(sink: &Sink, cards: &'a [Card]) -> Option<&'a Card> {
    let device_name = sink.prop("device.name");
    let address = sink.prop("api.bluez5.address");
    let bt_cards: Vec<&Card> = cards.iter().filter(|c| c.is_bluetooth()).collect();

    if let Some(dn) = device_name {
        if let Some(c) = bt_cards.iter().find(|c| c.name == dn) {
            return Some(c);
        }
    }
    if let Some(addr) = address {
        let na = normalize_addr(addr);
        if let Some(c) = bt_cards
            .iter()
            .find(|c| normalize_addr(c.name.strip_prefix("bluez_card.").unwrap_or(&c.name)) == na)
        {
            return Some(c);
        }
    }
    // Deriva o endereço a partir do nome da saída (bluez_output.<ADDR>...).
    if let Some(rest) = sink.name.strip_prefix("bluez_output.") {
        let derived = rest.split('.').next().unwrap_or(rest);
        let nd = normalize_addr(derived);
        if let Some(c) = bt_cards
            .iter()
            .find(|c| normalize_addr(c.name.strip_prefix("bluez_card.").unwrap_or(&c.name)) == nd)
        {
            return Some(c);
        }
    }
    None
}

/// Ativa um perfil A2DP disponível para a saída selecionada.
///
/// Não força reconexão do dispositivo. Se a placa já estiver em A2DP, não
/// realiza nenhuma alteração. Erro se a saída não for Bluetooth ou não houver
/// perfil A2DP disponível.
pub fn activate_bluetooth_for(backend: &dyn Backend, selector: &str) -> Result<Vec<String>> {
    let all_sinks = sinks(backend)?;
    let cards = cards(backend)?;

    let (sink_name, disp, is_bt, sink_snapshot) = {
        let s = resolve(&all_sinks, selector)?;
        (
            s.name.clone(),
            display_name(s),
            kind_of(s) == Kind::Bluetooth,
            s.clone(),
        )
    };

    if !is_bt {
        bail!("a saída `{disp}` ({sink_name}) não é Bluetooth");
    }

    let card = find_bt_card(&sink_snapshot, &cards).ok_or_else(|| {
        anyhow!(
            "não foi possível identificar a placa Bluetooth correspondente à saída `{sink_name}`"
        )
    })?;

    let card_name = card.name.clone();
    let active = card.active_profile_name();

    if let Some(profile) = active.as_deref().filter(|p| p.starts_with("a2dp-sink")) {
        return Ok(vec![format!(
            "A saída `{disp}` já está em A2DP (`{profile}`); nada a alterar."
        )]);
    }

    // Prefere explicitamente `a2dp-sink` disponível; senão qualquer A2DP disponível.
    let wanted = card
        .profiles
        .iter()
        .find(|(n, p)| n.as_str() == "a2dp-sink" && p.available.unwrap_or(true))
        .map(|(n, _)| n.clone())
        .or_else(|| {
            card.profiles
                .iter()
                .find(|(n, p)| n.starts_with("a2dp-sink") && p.available.unwrap_or(true))
                .map(|(n, _)| n.clone())
        })
        .ok_or_else(|| anyhow!("nenhum perfil A2DP disponível na placa `{card_name}`"))?;

    backend.run(&["set-card-profile", &card_name, &wanted])?;
    Ok(vec![
        format!("A2DP `{wanted}` ativado na placa {card_name} para a saída `{disp}` ({sink_name})."),
        "Aviso: no modo A2DP o microfone do fone fica DESATIVADO.".into(),
        "Aviso: mudanças de perfil podem exigir recriar grupos próprios ou combinações existentes que usem esta saída.".into(),
    ])
}

// ---------------------------------------------------------------------------
// Despacho
// ---------------------------------------------------------------------------

pub fn dispatch(backend: &dyn Backend, command: Command) -> Result<()> {
    match command {
        Command::List { json } => list(backend, json),
        Command::Groups => groups(backend),
        Command::Combine {
            name,
            default,
            move_streams,
            outputs,
        } => print_messages(combine(backend, &name, &outputs, default, move_streams)),
        Command::Remove { slug, fallback } => {
            print_messages(remove(backend, &slug, fallback.as_deref()))
        }
        Command::Stereo { selector } => print_messages(stereo(backend, &selector)),
        Command::Default {
            selector,
            move_streams,
        } => print_messages(set_default(backend, &selector, move_streams)),
    }
}

fn print_messages(result: Result<Vec<String>>) -> Result<()> {
    for message in result? {
        println!("{message}");
    }
    Ok(())
}

/// Lista de seletores elegíveis (usada pelo modo interativo).
/// Exclui grupos próprios, saídas virtuais e portas ativas indisponíveis.
pub fn selectable_sinks(backend: &dyn Backend) -> Result<Vec<Sink>> {
    Ok(sinks(backend)?
        .into_iter()
        .filter(|s| selectable(s) && kind_of(s) != Kind::Virtual)
        .collect())
}

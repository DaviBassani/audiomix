//! Definição da interface de linha de comando (clap).

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "audiomix",
    version,
    about = "Aplicativo de terminal para combinar saídas de áudio no Linux",
    long_about = "Gerencie saídas PipeWire/PulseAudio em um painel interativo. \
Sem argumentos abre o aplicativo de terminal; os subcomandos permitem automação."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Lista as saídas de áudio disponíveis (padrão, tipo, disponibilidade, estéreo/mono)
    List {
        /// Emite a listagem em JSON
        #[arg(long)]
        json: bool,
    },

    /// Combina 2 ou mais saídas físicas em um grupo próprio (audiomix_*)
    Combine {
        /// Identificador do grupo (slug ASCII: [a-z0-9_-])
        #[arg(long)]
        name: String,

        /// Define o grupo recém-criado como saída padrão
        #[arg(long)]
        default: bool,

        /// Move as transmissões de aplicativos para o novo grupo
        #[arg(long = "move-streams")]
        move_streams: bool,

        /// Seletores das saídas a combinar (nome exato, índice ou nome amigável)
        #[arg(required = true, value_name = "SELETOR")]
        outputs: Vec<String>,
    },

    /// Lista os grupos próprios criados pelo audiomix
    Groups,

    /// Remove um grupo próprio, verificando antes a posse do módulo
    Remove {
        /// Slug do grupo (sem o prefixo audiomix_)
        slug: String,

        /// Saída de destino disponível e fora do grupo; OBRIGATÓRIA quando o grupo é a saída padrão
        #[arg(long, value_name = "SELETOR")]
        fallback: Option<String>,
    },

    /// Ativa A2DP estéreo no dispositivo Bluetooth selecionado
    Stereo {
        /// Seletor da saída Bluetooth
        selector: String,
    },

    /// Define a saída padrão (e opcionalmente move as transmissões)
    Default {
        /// Seletor da saída
        selector: String,

        /// Move as transmissões de aplicativos para a nova saída padrão
        #[arg(long = "move-streams")]
        move_streams: bool,
    },
}

<div align="center">
  <h1>AudioMix</h1>
  <p>O mesmo som em mais de um fone. Um painel de controle dentro do terminal.</p>
  <p>
    <img alt="Plataforma: somente Linux" src="https://img.shields.io/badge/plataforma-Linux-2496ED">
    <img alt="Versao 0.2.0" src="https://img.shields.io/badge/versao-0.2.0-38b2ac">
    <img alt="Interface TUI e CLI" src="https://img.shields.io/badge/interface-TUI%20%2B%20CLI-eab308">
  </p>
</div>

**AudioMix** combina duas ou mais saídas físicas de áudio em uma saída virtual. Use, por exemplo, um fone Bluetooth e outro com fio para duas pessoas ouvirem o mesmo conteúdo no computador.

> [!IMPORTANT]
> **Esta versão funciona somente no Linux**, com PipeWire compatível com PulseAudio ou PulseAudio e o comando `pactl`. Ainda não há versão Windows ou macOS disponível neste projeto.

**Navegação:** [Instalar](#instalação) · [Usar o painel](#primeira-combinação) · [Atalhos](#atalhos) · [Comandos](#linha-de-comando) · [Problemas comuns](#problemas-comuns) · [Desenvolvimento](#desenvolvimento)

## O Que Faz

- Lista saídas por nome amigável, tipo, disponibilidade e canais de áudio.
- Combina fones Bluetooth, analógicos ou USB e outras saídas físicas disponíveis.
- Permite escolher se a combinação será o padrão e se os aplicativos já abertos devem mudar de destino.
- Ativa o perfil A2DP no fone Bluetooth escolhido, quando disponível.
- Mostra grupos próprios e remove-os com verificação de posse e saída de fallback.
- Oferece uma TUI persistente e subcomandos úteis para automação.

O AudioMix **duplica a reprodução** nos destinos escolhidos. Ele não mistura microfones, não cria dispositivos físicos e não elimina o atraso de transmissão Bluetooth.

## Requisitos

| Requisito | Observação |
|---|---|
| Linux | Plataforma suportada nesta versão |
| PipeWire com compatibilidade PulseAudio, ou PulseAudio | O servidor deve estar acessível na sessão do usuário |
| `pactl` com suporte a JSON | Confira com `pactl -f json list sinks` |
| Rust/Cargo estável atual | Necessário para compilar a partir do código |
| Terminal interativo | Mínimo de 55 colunas por 18 linhas; recomendado 100 × 30 ou maior |
| `desktop-file-install` | Opcional, para criar o atalho no menu de aplicativos |

Em Debian/Ubuntu, `pactl` costuma ser fornecido por `pulseaudio-utils` e `desktop-file-install` por `desktop-file-utils`. Em outras distribuições, consulte os pacotes equivalentes. Instale Rust pelas [instruções oficiais](https://www.rust-lang.org/tools/install).

Conecte e pareie os dispositivos antes de abrir o programa. O AudioMix não realiza pareamento Bluetooth.

## Instalação

```bash
git clone https://github.com/DaviBassani/audiomix.git
cd audiomix
./install.sh
```

O instalador compila a versão otimizada usando `Cargo.lock` e instala **somente para o usuário atual**, sem executar `sudo`:

| Item | Local |
|---|---|
| Executável | `~/.local/bin/audiomix` |
| Atalho, se a ferramenta opcional estiver disponível | `~/.local/share/applications/audiomix.desktop` |
| Código-fonte | A pasta em que você clonou o projeto |

Abra **AudioMix** no menu de aplicativos ou execute:

```bash
"$HOME/.local/bin/audiomix"
```

Se `~/.local/bin` já estiver no seu `PATH`, basta `audiomix`. O instalador informa quando a pasta não está presente; ele não edita a configuração do shell automaticamente.

<details>
<summary><strong>Atualizar ou remover a instalação</strong></summary>

Depois de obter uma revisão nova do código, execute `./install.sh` novamente. Não é necessário mover a instalação junto com o código-fonte.

Para remover uma instalação feita por este instalador:

```bash
cargo uninstall --root "$HOME/.local" audiomix
rm -f "$HOME/.local/share/applications/audiomix.desktop"
```

Remover o executável não desmonta um grupo que já esteja ativo no servidor de áudio. Desfaça os grupos pelo aplicativo antes, se desejar.

</details>

## Primeira Combinação

1. Abra o AudioMix. A aba **Dispositivos** mostra as saídas e o painel lateral explica a saída destacada.
2. Navegue com as **setas** e marque cada saída com **Espaço**. Escolha pelo menos duas saídas físicas disponíveis.
3. Pressione **`c`** e digite um nome, como `dupla`. Use letras minúsculas sem acentos, números, `_` ou `-`, com até 40 caracteres e sem separador no início ou no fim.
4. Pressione **Enter**. Escolha se o grupo deve ser o padrão e se as transmissões dos aplicativos devem ser movidas agora. **Tab** muda a opção e **Espaço** alterna sua marcação.
5. Pressione **Enter** para revisar as saídas e opções. Só **`y`** confirma a execução; **Esc** ou **`n`** cancelam a revisão.
6. Acompanhe o resultado no rodapé. O aplicativo continua aberto e atualiza os dispositivos depois da operação.

> [!NOTE]
> Tornar um grupo padrão e mover transmissões são decisões diferentes. O padrão orienta as próximas reproduções; mover transmissões redireciona os aplicativos já abertos. Aplicativos podem ter suas próprias preferências de saída.

### Estéreo Bluetooth

Na aba **Dispositivos**, destaque o fone Bluetooth e pressione **`s`**. Revise o aviso e confirme com **`y`**. Se o dispositivo já usa A2DP, o perfil existente é preservado.

> [!WARNING]
> A2DP normalmente desativa o microfone do próprio fone. Uma troca de perfil pode recriar a saída no servidor: grupos que a utilizavam podem precisar ser recriados. Faça esse ajuste antes de combinar os dispositivos, quando possível.

### Desfazer Uma Combinação

Use **Tab** para ir a **Grupos**, destaque um grupo próprio e pressione **`x`**. Se ele for a saída padrão, escolha primeiro uma saída disponível fora do grupo para continuar a reprodução. Revise e confirme com **`y`**.

Apenas transmissões associadas ao grupo removido são migradas nessa operação. Uma falha na migração impede a desmontagem. Combinações criadas por outras ferramentas aparecem como leitura e não são removidas pelo AudioMix.

## Atalhos

| Tecla | Ação |
|---|---|
| Setas ou `j` / `k` | Navegar na lista em foco |
| Espaço | Marcar uma saída física ou alternar uma opção do formulário |
| Tab | Alternar abas ou opções, conforme a tela |
| `c` | Começar a combinação das saídas marcadas |
| `d` | Revisar a mudança de saída padrão para o dispositivo ou grupo destacado |
| `s` | Revisar a ativação A2DP no Bluetooth destacado |
| `x` | Revisar a remoção de um grupo próprio na aba Grupos |
| `r` | Atualizar a leitura do servidor |
| `?` | Ajuda e histórico de mensagens |
| PgUp / PgDn | Rolar detalhes, revisões e histórico |
| Esc | Cancelar ou fechar a tela atual, antes da execução |
| `y` | Executar uma ação na tela de confirmação |
| `q` ou Ctrl-C | Sair; se houver operação em curso, aguardar sua conclusão |

Durante a digitação do nome, letras como `q`, `j` e `k` são texto normal. Se a janela ficar pequena demais, confirmações ficam bloqueadas até que os controles possam ser exibidos novamente.

## Linha de Comando

```bash
audiomix list
audiomix list --json
audiomix groups
audiomix --help
```

Os seletores aceitam nome técnico, índice atual ou nome amigável. Uma parte única do nome também pode ser usada. Seleções ambíguas são rejeitadas.

```bash
# Exemplo: substitua pelos nomes mostrados em `audiomix list`.
audiomix stereo "Meu fone Bluetooth"
audiomix combine --name dupla "Fone com fio" "Meu fone Bluetooth" --default --move-streams
audiomix default "Meu fone Bluetooth" --move-streams
audiomix remove dupla --fallback "Meu fone Bluetooth"
```

`combine` sem `--default` e sem `--move-streams` cria o grupo sem redirecionar os aplicativos. `remove` exige `--fallback` quando o grupo é o padrão. Os índices podem mudar após reconexões: para scripts, prefira nomes técnicos conhecidos.

Os subcomandos não exigem confirmação interativa; as opções da linha de comando autorizam a operação solicitada. Código de saída diferente de zero indica erro. Em falhas parciais, confira o estado antes de repetir: uma mudança de padrão ou criação de grupo pode ter ocorrido antes da falha de migração.

## Limites da Versão

- **Somente Linux.** Nenhum executável Windows é oferecido por esta versão.
- Os grupos vivem no servidor de áudio e **não são restaurados automaticamente após reiniciar**.
- Desconectar dispositivos ou trocar perfis pode exigir recriar o grupo.
- Bluetooth e saídas com fio podem ter atrasos diferentes. Sincronização perfeita não é garantida.
- Saídas virtuais não podem ser combinadas de novo: não há grupos aninhados.
- O programa não ajusta volumes automaticamente. Confira o volume de cada destino antes de reproduzir.

## Problemas Comuns

| Sintoma | O que verificar |
|---|---|
| `audiomix: comando não encontrado` | Use `"$HOME/.local/bin/audiomix"` ou o atalho; confira o `PATH` no terminal em que ocorreu o erro |
| `pactl` ausente ou servidor inacessível | Execute `pactl info` na mesma sessão de desktop; não rode AudioMix como root |
| Fone não aparece | Conecte-o pelas configurações do sistema e pressione `r` |
| Som Bluetooth mono ou de chamada | Confira o perfil A2DP com `s`; se não estiver disponível, reveja a conexão pelo sistema |
| Aplicativo continua no destino antigo | Use a opção de mover transmissões ou confira a preferência de áudio do aplicativo |
| Mensagem longa cortada no rodapé | Abra `?` e role o histórico com PgUp/PgDn |
| Terminal pequeno | Amplie a janela; mínimo de 55 × 18 e recomendado 100 × 30 |

Ao relatar um problema, remova nomes pessoais de dispositivos, endereços Bluetooth, IPs, caminhos do usuário e outros identificadores dos logs. O nome técnico de uma saída pode conter o endereço Bluetooth do fone.

## Desenvolvimento

```bash
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

Os testes usam fixtures e backends simulados para operações de áudio. Os testes de renderização usam um terminal em memória; eles não substituem testes com hardware real. Consulte [Arquitetura e segurança](docs/ARCHITECTURE.md) para as fronteiras e garantias do projeto.

A preparação da versão Linux inclui um [relatório de revisão de publicação](docs/AUDIT.md), com achados, correções, evidências e limitações.

**Autoria:** [Davi Lopes Bassani](https://github.com/DaviBassani).

**Licença:** [MIT](LICENSE). Você pode usar, modificar e redistribuir o projeto, mantendo os avisos de autoria e os termos da licença.

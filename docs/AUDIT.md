# Revisão de Publicação Linux

**Data:** 18 de outubro de 2026. **Versão:** 0.2.0.

Este relatório registra uma revisão local de preparação para publicação. Não representa certificação nem garantia de ausência de vulnerabilidades.

## Escopo

- Arquitetura, legibilidade e fronteiras entre interface, regras de áudio e execução de processos.
- Composição, posse de módulos, migração, fallback e compensação em falhas.
- Estado da TUI, erros, conteúdo exibido e ciclo de vida do terminal.
- Fontes, fixtures, assets, documentação, metadados Cargo e candidatos ao primeiro commit.
- Dependências de produção e configuração de integração contínua.

Não existia repositório Git no projeto antes desta preparação. O novo histórico começa com conteúdo revisado; o cache de compilação anterior foi descartado, não incorporado ao Git.

## Achados e Tratamento

| Área | Achado | Tratamento |
|---|---|---|
| Estado após falha interna | Pânico durante uma alteração era apresentado como falha de leitura | Preservada a semântica de alteração; nova leitura protegida por tratamento de pânico e testes de recuperação |
| PulseAudio nativo | Saídas de filtro/composição sem `node.virtual` podiam ser elegíveis | Reconhecimento de `device.class=filter` e descrições de composição; testes de rejeição no comando e na TUI |
| Subprocessos | Descendentes de um wrapper podiam manter os pipes abertos após timeout ou saída do processo principal | Grupo de processos dedicado, encerramento dos descendentes do grupo e testes de duração limitada |
| Texto do terminal | Identificadores e diagnósticos nem sempre passavam pela mesma sanitização | Sanitização na apresentação, preservando os nomes originais usados nas operações |
| Dependências | Ratatui antigo incluía `lru` com avisos de unsoundness e `paste` sem manutenção | Atualização de Ratatui/Crossterm e do lockfile; `lru` atualizado e `paste` removido |
| Mudança de diretório | Cache de testes continha caminhos da localização anterior | Cache removido e compilação validada novamente no diretório novo |

Os avisos eliminados da árvore antiga incluíam [RUSTSEC-2026-0002](https://rustsec.org/advisories/RUSTSEC-2026-0002), [RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253) e [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436). Nenhum foi suprimido por uma regra de ignore.

## Evidências Locais

| Verificação | Resultado |
|---|---|
| `cargo fmt -- --check` | Aprovado |
| `cargo clippy --locked --all-targets -- -D warnings` | Aprovado |
| `cargo test --locked --all-targets` | 102 testes aprovados |
| `cargo build --locked --release` | Aprovado |
| `cargo audit --deny warnings` com Cargo Audit | Base RustSec atualizada; sem alertas de segurança para as 203 dependências do lockfile |
| GitLeaks 8.30.1 sobre o conteúdo indexado | Sem segredos detectados |
| `sh -n` e validação das entradas desktop | Aprovados |

A ferramenta Cargo Audit emitiu um aviso operacional de atualização do índice de pacotes. A base de advisories RustSec foi atualizada e a verificação do lockfile terminou com sucesso; esse resultado não é uma afirmação sobre disponibilidade ou retirada de versões no registro de pacotes.

Os testes de mutação usam backends simulados. A validação de interface usa renderização em memória e terminal com pseudo-TTY. A leitura do servidor real não substitui testes de reprodução, microfone, codecs e latência em cada equipamento.

O workflow Linux foi preparado para executar formatação, lint, testes e build. A validação remota do GitHub Actions só poderá ocorrer depois da publicação; não é apresentada aqui como já executada.

## Privacidade e Histórico

- Não foram encontrados segredos, caminhos pessoais literais, endereços Bluetooth completos reais ou dumps reais de dispositivos nos candidatos revisados.
- Os identificadores de dispositivos das fixtures são sintéticos.
- `target/`, configurações locais de ambiente e logs não fazem parte dos candidatos à publicação.
- Nome do mantenedor e perfil público do GitHub são dados de autoria intencionais, não resíduos da máquina de desenvolvimento.
- Os commits desta preparação usam autoria e committer do mantenedor e e-mail `noreply` do GitHub, sem trailers de coautoria.
- A verificação de segredos precisa ser repetida após novas alterações; uma varredura limpa não prova que qualquer arquivo futuro será seguro.

## Riscos e Decisões Remanescentes

- O módulo `tui` é grande, embora mantenha seções e contratos testáveis de estado, execução e desenho. Sua extração em submódulos é uma melhoria de manutenção, não foi tratada como condição de segurança para esta revisão.
- O override `AUDIOMIX_PACTL` aceita um executável de confiança. Não há sandbox para wrappers arbitrários ou processos que deliberadamente se desassociem do grupo.
- O servidor pode receber uma alteração antes de um timeout ou erro de comunicação. A interface informa falhas e permite reler o estado; não há transação distribuída com rollback garantido.
- A versão continua exclusiva para Linux e não restaura grupos automaticamente após reiniciar.
- Na revisão de 18 de outubro, a licença de distribuição ainda estava pendente. Após a autorização para publicação open source em 20 de outubro de 2026, o projeto passou a adotar a [licença MIT](../LICENSE), com autoria do mantenedor.

Para entender os controles e seus limites, consulte [Arquitetura e segurança](ARCHITECTURE.md).

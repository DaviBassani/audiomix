# Arquitetura e Segurança

> A implementação atual é exclusiva para Linux. A interface organiza operações de PulseAudio/PipeWire; ela não processa amostras de áudio diretamente.

## Fluxo

```text
TUI / subcomandos
        |
        v
commands: validacao, seletores e regras de alteracao
        |
        v
Backend: argumentos explicitos para pactl
        |
        v
servidor de audio da sessao do usuario
```

| Módulo | Responsabilidade |
|---|---|
| `main` / `cli` | Entrada, ajuda, subcomandos e exigência de terminal para a TUI |
| `model` | Tipos e interpretação das respostas JSON |
| `devices` / `selector` | Nomes amigáveis, classificação, disponibilidade e resolução sem ambiguidade |
| `combine` | Identificação de grupos e verificação dos marcadores de posse |
| `commands` | Validação anterior à escrita, composição, fallback, migração e mensagens de resultado |
| `backend` | Execução limitada de `pactl`, captura de saída e tratamento de erros |
| `tui` | Estado, navegação, formulários, trabalhos em background e renderização |

## Estado e Concorrência

A TUI mantém um snapshot para exibição. Seleções usam nomes técnicos estáveis em vez de posições da lista; uma atualização remove seleções desconectadas ou inelegíveis. Antes de alterar áudio, os comandos consultam e validam o estado novamente.

O redutor de teclado não altera o áudio: ele produz uma ação após confirmação. Um trabalho de cada vez executa no background e devolve mensagens e uma leitura atualizada. Novas alterações são bloqueadas enquanto ele termina. Sair durante uma operação aguarda o resultado em vez de abandoná-la silenciosamente.

As mensagens das operações são valores retornados, não impressões dentro da TUI. O modo CLI imprime esses valores fora da tela interativa. Erros permanecem no histórico consultável.

## Fronteiras de Segurança

- `pactl` é executado com uma lista de argumentos, sem interpolação em shell.
- Nomes de grupos e nomes técnicos usados nos argumentos de composição são validados.
- Uma combinação exige pelo menos duas saídas físicas distintas e não aceita portas conhecidas como indisponíveis ou saídas virtuais.
- A remoção exige nome de módulo, prefixo, marcador e identidade compatíveis com grupos próprios. Um nome parecido, sozinho, não autoriza remover um módulo.
- Os IDs de módulos são descobertos no servidor. A versão de JSON do servidor pode omitir esses IDs; nesse caso, são cruzados com a listagem curta por identidade.
- Se o grupo for a saída padrão, um fallback fora dele é exigido antes de desmontar. A migração é limitada às transmissões do grupo e não move transmissões internas de composição.
- Falhas de migração mantêm o grupo. Falhas posteriores à criação podem exigir compensação; erros na própria compensação são informados, não ocultados.
- Os comandos não alteram arquivos permanentes de configuração de áudio nem volumes.
- O backend limita a espera por cada chamada e captura stdout/stderr. Uma falha ou timeout pode ocorrer depois de uma alteração no servidor: atualize o estado antes de repetir uma ação.
- Cada chamada possui um grupo de processos próprio; descendentes nesse grupo são encerrados antes de aguardar as leituras dos pipes. O override `AUDIOMIX_PACTL` é destinado a testes e wrappers de confiança, não é um sandbox para programas maliciosos que se desassociem do grupo.
- Metadados e diagnósticos passam por sanitização antes de aparecer na TUI. Os identificadores originais são preservados para as chamadas ao backend.
- O terminal é restaurado ao sair normalmente, em Ctrl-C e nos caminhos de erro tratados. Interrupções forçadas do processo, como SIGKILL, não oferecem garantia de limpeza.

Essas verificações evitam alterações acidentais; os marcadores não são uma fronteira contra programas maliciosos executados com os mesmos privilégios do usuário.

## Privacidade

O aplicativo não implementa telemetria nem serviço remoto. O fluxo de áudio permanece a cargo do servidor local. A instalação e compilação podem acessar registros de dependências; esse acesso não é uma transmissão de áudio pelo aplicativo.

A listagem JSON e os detalhes técnicos podem conter endereços Bluetooth, nomes pessoais e identificadores definidos pelo sistema. O AudioMix não grava esse snapshot em um arquivo de configuração próprio. Revise a saída antes de compartilhá-la em issues ou capturas.

Fixtures devem ser sintéticas. Não adicione dumps de dispositivos reais, caminhos absolutos do computador, credenciais ou logs pessoais ao repositório. `target/` e arquivos de ambiente locais não devem ser versionados.

## Estratégia de Testes

- Parsing e seletores: formas de JSON, ambiguidade e disponibilidade.
- Operações: posse, rollback, falhas parciais, A2DP escolhido e escopo de migração.
- Processos: erros de execução, timeout e captura de mensagens.
- Estado TUI: confirmação explícita, cancelamento, contexto da aba e bloqueio durante trabalhos.
- Renderização: tamanhos de terminal, alvos longos, caracteres largos, contraste e rolagem.

Testes com mocks não demonstram qualidade acústica, latência, suporte de codecs ou compatibilidade com cada controlador Bluetooth. Uma revisão de segurança também não é garantia de ausência de vulnerabilidades.

## Evolução

O maior módulo é a TUI. A separação interna entre estado, execução e desenho permite extrair submódulos sem alterar o contrato de comandos quando isso trouxer benefício concreto. Não há um framework genérico de plugins nem uma camada de compatibilidade Windows nesta versão.

Uma futura implementação Windows precisará de um backend próprio e de testes da plataforma; trocar apenas a extensão do executável ou procurar `pactl` no Windows não fornece essa funcionalidade.

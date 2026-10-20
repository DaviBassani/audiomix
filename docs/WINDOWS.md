# Windows: Previa Experimental

> Esta branch implementa espelhamento nativo com WASAPI. O comportamento e as capacidades sao diferentes dos grupos PulseAudio/PipeWire da versao Linux. Ainda e necessaria validacao em um computador Windows real com os dispositivos desejados.

## Modelo de Audio

```text
Aplicativos do Windows
         |
         v
Saida padrao atual ----> reproducao original, mantida pelo Windows
         |
         +-- WASAPI loopback --> AudioMix --> saidas adicionais escolhidas
```

A origem e a saida padrao atual. O AudioMix captura o que essa saida reproduz e envia copias apenas aos destinos adicionais, nunca de volta a origem. Escolher a propria origem como destino e rejeitado para evitar realimentacao.

Isso evita realimentacao criada pelo proprio aplicativo. Roteamentos externos, cabos virtuais e dispositivos que internamente devolvam um destino a origem nao podem ser deduzidos apenas pelos IDs dos endpoints; nao use esse tipo de ligacao sem revisar seu fluxo de audio.

O programa nao troca a saida padrao automaticamente, nao cria um dispositivo de audio virtual permanente e nao muda volumes. Se precisar de outra origem, escolha-a primeiro nas configuracoes de som do Windows e atualize a lista no aplicativo.

## Requisitos

- Windows 10 ou Windows 11, 64 bits, arquitetura x86-64.
- Pelo menos duas saidas de reproducao ativas: a origem padrao e um destino adicional.
- Dispositivos disponiveis em modo compartilhado WASAPI e formatos aceitos pelo motor de espelhamento.
- Terminal com pelo menos 55 colunas por 18 linhas; recomendado 100 por 30 ou maior.

Nao e necessario instalar `pactl`, PipeWire ou um servidor PulseAudio. Esta implementacao usa as APIs nativas do Windows.

## Usar a Previa

1. Extraia o pacote completo para uma pasta de sua escolha.
2. Abra `audiomix.exe`. O aplicativo mostra a origem padrao e as saidas nativas encontradas no Windows.
3. Marque os destinos adicionais com Espaco; a origem nao pode ser marcada como destino.
4. Pressione `c`, revise os nomes e confirme com `y` para iniciar o espelhamento.
5. Use `x` para parar a copia. A reproducao original permanece sob controle do Windows.
6. Use `q` ou Ctrl-C para parar a sessao e sair.

O AudioMix precisa permanecer aberto enquanto estiver espelhando. A sessao nao e persistida depois de fechar o programa ou reiniciar o computador.

### Linha de Comando

```powershell
.\audiomix.exe list
.\audiomix.exe list --json
.\audiomix.exe mirror --source "Speakers" "Headphones"
.\audiomix.exe --help
```

Substitua os nomes pelos exibidos na listagem. Os nomes podem ser localizados pelo Windows ou definidos pelo fabricante. IDs de endpoints sao identificadores tecnicos e podem revelar informacoes do equipamento; remova-os de relatos publicos quando nao forem necessarios.

## Diferencas em Relacao ao Linux

| Capacidade | Windows nesta previa |
|---|---|
| Enumerar saidas reais | CoreAudio/WASAPI |
| Duplicar reproducao | Captura loopback da origem padrao para destinos secundarios |
| Trocar saida padrao | Use as configuracoes do Windows |
| Escolher A2DP ou perfil Bluetooth | Use as configuracoes do Windows e do dispositivo |
| Grupos PulseAudio/PipeWire | Nao se aplicam; a sessao pertence ao processo AudioMix |
| Persistir depois de fechar | Nao suportado |

Comandos sem equivalente nativo nesta previa nao devem ser tratados como se tivessem alterado o sistema.

## Seguranca e Limites

- Todo som reproduzido na origem pode ser copiado, incluindo notificacoes e outras aplicacoes. Revise os destinos antes de confirmar.
- A captura de conteudo protegido pode ser bloqueada ou resultar em silencio. O aplicativo nao contorna DRM ou restricoes do sistema.
- Aplicativos que usam outra saida, modo exclusivo ou configuracoes particulares podem nao aparecer na captura da origem.
- Bluetooth e dispositivos independentes possuem latencias e relogios diferentes. Sincronia perfeita nao e garantida.
- Nao ha mistura de microfones, alteracao automatica de volume nem transmissao de audio pela rede.
- Mudancas de origem ou desconexao de dispositivos podem interromper a sessao; revise a mensagem, reconecte e atualize antes de repetir.
- O executavel desta previa nao possui assinatura Authenticode. Confira sua origem e o checksum; nao desative protecoes do Windows para executar arquivos de procedencia desconhecida.

## Conteudo do Pacote

O pacote deve acompanhar o executavel, esta documentacao, a licenca MIT do projeto e os avisos de licencas das dependencias. O checksum SHA-256 permite comparar o arquivo recebido com o arquivo produzido na compilacao, mas nao substitui uma assinatura ou a confianca no distribuidor.

## Estado de Validacao

A compilacao cruzada e testes simulados nao comprovam reproducao acustica em Windows. Consulte o relatorio que acompanha o pacote para saber quais verificacoes foram executadas. Os downloads sao disponibilizados como [pre-release experimental no GitHub](https://github.com/DaviBassani/audiomix/releases/tag/v0.3.0-alpha.1), nao como uma versao de producao validada em hardware.

Chamadas de inicializacao e encerramento dependem das APIs e dos drivers do Windows. O prazo de espera da inicializacao nao garante que uma chamada COM bloqueada sera cancelada; um driver bloqueado pode atrasar a parada ou o encerramento.

## Compilar e Empacotar

Para compilar nativamente em Windows, instale Rust e as ferramentas C++ recomendadas pela distribuicao MSVC do Rust e execute `cargo build --locked --release`. Esse caminho de compilacao nativa nao foi executado nesta entrega.

O pacote local desta previa foi produzido no Linux com Rust para Windows e uma toolchain LLVM-MinGW portatil verificada. Para reproduzir esse caminho, instale o alvo e o gerador de avisos de licenca:

```bash
rustup target add x86_64-pc-windows-gnullvm
cargo install cargo-about --locked --features cli
export LLVM_MINGW_ARCHIVE="/caminho/para/llvm-mingw-20261006-ucrt-ubuntu-22.04-x86_64.tar.xz"
./scripts/package-windows.sh
```

O script exige a distribuicao LLVM-MinGW 20261006 acima e verifica seu SHA-256 fixado antes de extrai-la em um diretorio temporario novo. O arquivo oficial esta na [pagina do fornecedor](https://github.com/mstorsjo/llvm-mingw/releases/tag/20261006). Isso verifica o arquivo da toolchain, mas nao transforma um ambiente de compilacao comprometido em confiavel; Cargo, Rust e o gerador de licencas tambem devem vir de origens confiaveis.

O script nao instala ferramentas de sistema nem altera a configuracao do shell. Ele gera as licencas das dependencias para o alvo Windows, inclui avisos do runtime Rust e da toolchain e produz o executavel, ZIP e checksums em `dist/`. Nao sobrescreve um ZIP anterior com o mesmo nome.

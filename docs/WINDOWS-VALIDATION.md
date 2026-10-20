# Validacao da Previa Windows 0.3.0-alpha.1

**Data:** 20 de outubro de 2026. **Status:** executavel compilado; reproducao em hardware Windows ainda nao validada.

Este relatorio distingue a compilacao, os testes simulados e as verificacoes que ainda dependem de um computador Windows real. A existencia do executavel nao comprova qualidade acustica, estabilidade de drivers ou sincronizacao entre dispositivos.

## Verificacoes Executadas

| Verificacao | Resultado |
|---|---|
| Testes executados no Linux | 151 aprovados, incluindo os 102 testes Linux anteriores |
| Formatacao Rust | Aprovada |
| Clippy Linux, todos os alvos locais, avisos como erro | Aprovado |
| Clippy para Windows, incluindo testes, avisos como erro | Aprovado |
| Compilacao e link de todos os executaveis de teste Windows | Aprovados com `--no-run` na validacao local; a execucao remota e registrada separadamente no GitHub Actions |
| Compilacao otimizada do aplicativo Windows | Aprovada para `x86_64-pc-windows-gnullvm` |
| Inspecao do executavel | PE32+ x86-64, subsistema console |
| Dependencias DLL | Apenas contratos e bibliotecas do proprio Windows/UCRT na tabela de importacao |
| Caminhos do computador no executavel | Prefixos pessoais removidos por remapeamento de caminhos e verificacao de strings |
| RustSec atualizado | Sem advisories para as 215 dependencias do lockfile |

A ferramenta de auditoria emitiu o aviso operacional ja conhecido sobre atualizar o indice de pacotes. A base RustSec foi atualizada e a verificacao de advisories do lockfile terminou com sucesso.

## O Que os Testes Demonstram

- Rejeicao da propria origem como destino, de selecoes duplicadas, origem nao padrao e dispositivos ausentes.
- Validacao de tipos PCM/float, canais, alinhamento de quadros e taxas, sem reinterpretacao arbitraria de amostras.
- Filas limitadas por destino, descarte alinhado de quadros antigos em overflow e tratamento de falta de dados.
- Copia de pacotes silenciosos sem exigir um ponteiro de dados legivel; silencio PCM de 8 bits usa o ponto medio correto.
- Confirmacao explicita na TUI, cancelamento, bloqueio de confirmacao invisivel em janela pequena e limpeza das selecoes desconectadas.
- Parada e reinicio com um novo token de cancelamento, sem reusar um token ja sinalizado.
- Bloqueio de uma sessao nova enquanto a anterior ainda libera seus recursos.
- Tratamento de falhas de trabalhos e uma unica notificacao final de encerramento por sessao.

O caminho de captura nativo usa uma aquisicao de pacote que verifica os flags antes de acessar os dados e libera cada pacote adquirido. Flags exclusivos de captura nao sao enviados a `IAudioRenderClient::ReleaseBuffer`. Esses cuidados foram revisados no codigo e possuem testes da politica pura de copia; a API real ainda precisa de exercicio em Windows.

## Aceitacao de Terminal

A mesma interface Windows foi executada em um pseudo-terminal no Linux com um adaptador de audio simulado, sem abrir endpoints reais. Foram exercitados selecao, revisao, rejeicao de `y` com a revisao escondida por redimensionamento, iniciar, parar, reiniciar, saida com `q`/Ctrl-C e apresentacao de erro completo. As flags do terminal, o cursor e a tela alternativa foram restaurados.

Esse teste valida o fluxo da interface e sua limpeza no terminal usado. Ele nao executa o backend WASAPI nem comprova o comportamento do console nativo Windows.

## O Que Ainda Falta Validar em Windows

1. Conferir se a listagem corresponde aos dispositivos ativos nas Configuracoes de Som.
2. Reproduzir conteudo nao protegido na origem padrao e confirmar que ela continua audivel sem uma segunda copia nela mesma.
3. Iniciar a copia para um e depois varios destinos; testar esquerda/direita, taxas e quantidades de canais diferentes.
4. Parar e reiniciar repetidamente, verificando liberacao dos clientes e ausencia de audio residual inesperado.
5. Desconectar um destino ou mudar a origem padrao e conferir a interrupcao com mensagem util.
6. Verificar latencia, diferenca de relogios, underruns e overflows com Bluetooth, USB e dispositivos com fio reais.
7. Conferir o terminal nativo, Ctrl-C, encerramento e comportamento com o dispositivo ocupado ou em modo exclusivo.

Nenhuma dessas verificacoes acusticas ou de driver e apresentada como realizada nesta entrega.

## Pacote e Publicacao

O pacote e uma previa portatil local, nao assinada com Authenticode, com executavel, guia, este relatorio, licenca MIT e avisos de terceiros. Os checksums ficam no arquivo `SHA256SUMS` ao lado do pacote.

A implementacao fica na branch `windows-support`, separada da `main` Linux. A distribuicao publica e identificada como **pre-release experimental**, nao como uma versao validada em hardware Windows. Consulte as [notas da pre-release](https://github.com/DaviBassani/audiomix/releases/tag/v0.3.0-alpha.1) e os resultados mais recentes de [GitHub Actions](https://github.com/DaviBassani/audiomix/actions) para o estado da publicacao e dos checks remotos; eles nao substituem os testes acusticos pendentes.

## Revisao Antes da Publicacao

Uma revisao independente de seguranca nao encontrou achados bloqueantes para essa distribuicao experimental. Foram repetidas as buscas de segredos no historico e nos arquivos, a verificacao RustSec e a conferencia dos hashes do executavel e do ZIP. Os textos de disponibilidade foram atualizados para evitar confundir uma pre-release publica com uma versao de producao.

A compilacao utiliza Rust 1.99.1, Cargo About 0.9.2 e LLVM-MinGW 20261006 (Clang 23.0.0). O empacotador agora verifica o arquivo oficial LLVM-MinGW antes de extrair e usar uma copia nova; o SHA-256 fixado e `5f9c6ed95b2d4bdb2869a488c5fd5857fbdabcf288a0aa3eb1da43f6a08d8ab4`. Essa verificacao nao cobre um ambiente inteiro comprometido. O prazo de inicializacao tambem nao oferece cancelamento forcado de uma chamada COM ou driver bloqueado; essa limitacao de disponibilidade permanece documentada.

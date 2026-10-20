# AudioMix 0.3.0-alpha.1 - Windows Experimental

## Download

**[Baixar AudioMix para Windows 10/11 x64 - ZIP completo](https://github.com/DaviBassani/audiomix/releases/download/v0.3.0-alpha.1/audiomix-0.3.0-alpha.1-windows-x86_64.zip)**

[Checksums SHA-256](https://github.com/DaviBassani/audiomix/releases/download/v0.3.0-alpha.1/SHA256SUMS) · [Guia Windows](https://github.com/DaviBassani/audiomix/blob/v0.3.0-alpha.1/docs/WINDOWS.md) · [Relatório de validação](https://github.com/DaviBassani/audiomix/blob/v0.3.0-alpha.1/docs/WINDOWS-VALIDATION.md)

Baixe o ZIP do aplicativo acima, **não os arquivos automáticos “Source code”**. O pacote contém o executável, as instruções, a licença MIT e os avisos das dependências. Não é necessário instalar Rust ou um driver de áudio virtual.

> [!WARNING]
> Esta é uma **pré-release experimental**, não uma versão de produção. A reprodução em hardware Windows real ainda precisa ser validada. Compilação e testes simulados não garantem compatibilidade acústica, de drivers ou de sincronização com todos os dispositivos.

## Como Usar

1. Extraia o ZIP inteiro para uma pasta.
2. Abra `audiomix.exe`.
3. A saída padrão atual do Windows aparece como origem. Marque as saídas adicionais com **Espaço**.
4. Pressione **`c`** para revisar e **`y`** para iniciar a cópia do áudio.
5. Use **`x`** para parar; **`q`** ou **Ctrl-C** param a sessão e encerram o aplicativo.

O AudioMix precisa permanecer aberto. A reprodução original continua no dispositivo padrão; o programa não envia uma segunda cópia para essa origem e não altera o padrão, volumes ou perfis Bluetooth automaticamente.

Para escolher outra origem ou ajustar A2DP, use as configurações de som/Bluetooth do Windows antes de iniciar. Áudio protegido, modo exclusivo ou roteamentos particulares dos aplicativos podem não ser capturados.

## Verificações e Segurança

- Revisão independente de segurança antes da publicação, sem achados bloqueantes para uma pré-release.
- 151 testes automatizados executados no Linux, incluindo os testes anteriores do backend Linux e os testes puros da implementação Windows.
- Compilação e link Windows x64, análise estática e testes de terminal com backend simulado.
- Verificação RustSec das dependências e GitLeaks do histórico e dos arquivos, sem alertas de segurança detectados.
- Toolchain LLVM-MinGW fixada e conferida por SHA-256 antes da extração usada no empacotamento.
- Checks remotos Linux e Windows exigidos antes de disponibilizar esta pré-release. Os resultados ficam em [GitHub Actions](https://github.com/DaviBassani/audiomix/actions).

Os testes de CI não incluem reprodução física nem comprovam o funcionamento com seu hardware. Drivers ou chamadas COM bloqueadas ainda podem atrasar a inicialização ou o encerramento. Bluetooth e dispositivos independentes podem apresentar atrasos diferentes.

O executável **não possui assinatura Authenticode**. Verifique a origem e o checksum; não desative proteções do Windows para executar arquivos desconhecidos. No PowerShell, o hash pode ser consultado com:

```powershell
Get-FileHash .\audiomix-0.3.0-alpha.1-windows-x86_64.zip -Algorithm SHA256
```

Compare o resultado com a linha do ZIP em `SHA256SUMS`. O checksum detecta mudanças no arquivo, mas não substitui uma assinatura ou a confiança no distribuidor.

## Código e Autoria

Código Windows na branch `windows-support`, separado da implementação Linux em `main`. A licença do projeto é [MIT](https://github.com/DaviBassani/audiomix/blob/v0.3.0-alpha.1/LICENSE), com avisos próprios das dependências incluídos no pacote.

Mantido por [Davi Lopes Bassani](https://github.com/DaviBassani).

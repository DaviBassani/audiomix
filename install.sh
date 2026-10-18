#!/bin/sh
set -eu

project_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

if command -v cargo >/dev/null 2>&1; then
    cargo_bin=cargo
elif [ -x "$HOME/.cargo/bin/cargo" ]; then
    cargo_bin=$HOME/.cargo/bin/cargo
else
    printf '%s\n' 'Rust/Cargo nao encontrado. Instale a toolchain oficial em https://rustup.rs.' >&2
    exit 1
fi

"$cargo_bin" install --path "$project_dir" --root "$HOME/.local" --locked
printf '%s\n' "Instalado em $HOME/.local/bin/audiomix (somente para seu usuario)."
if command -v desktop-file-install >/dev/null 2>&1; then
    desktop-file-install --dir="$HOME/.local/share/applications" \
        --set-key=Exec --set-value="\"$HOME/.local/bin/audiomix\"" \
        "$project_dir/assets/audiomix.desktop"
    printf '%s\n' 'Atalho AudioMix instalado no menu de aplicativos.'
fi
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) printf '%s\n' 'Adicione $HOME/.local/bin ao PATH do seu shell para usar audiomix de qualquer pasta.' ;;
esac

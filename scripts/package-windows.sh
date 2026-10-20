#!/usr/bin/env bash
set -euo pipefail

project_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"

cargo_bin=${CARGO:-$(command -v cargo || true)}
rustc_bin=${RUSTC:-$(command -v rustc || true)}
[[ -n "$cargo_bin" ]] || cargo_bin="$HOME/.cargo/bin/cargo"
[[ -n "$rustc_bin" ]] || rustc_bin="$HOME/.cargo/bin/rustc"
export PATH="$(dirname -- "$cargo_bin"):$PATH"
about_bin=${CARGO_ABOUT:-cargo-about}
target=x86_64-pc-windows-gnullvm
: "${LLVM_MINGW_ROOT:?Set LLVM_MINGW_ROOT to a trusted, extracted LLVM-MinGW toolchain}"

linker="$LLVM_MINGW_ROOT/bin/x86_64-w64-mingw32-clang"
[[ -x "$linker" ]] || { printf '%s\n' 'LLVM-MinGW linker not found.' >&2; exit 1; }
for tool in "$cargo_bin" "$rustc_bin" "$about_bin" zip jq sha256sum; do
    command -v "$tool" >/dev/null || { printf 'Missing build tool: %s\n' "$tool" >&2; exit 1; }
done

version=$("$cargo_bin" metadata --no-deps --format-version 1 --locked | jq -r '.packages[] | select(.name == "audiomix") | .version')
name="audiomix-$version-windows-x86_64"
dist="$project_dir/dist"
mkdir -p "$dist"
stage=$(mktemp -d "$dist/.windows-package.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
payload="$stage/$name"
mkdir -p "$payload/licenses/rust" "$payload/licenses/toolchain"

# Remove host paths from panic locations and debug metadata in this build only.
flags=(
    "--remap-path-prefix=$project_dir=audiomix"
    "--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo"
    "--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=rustup"
    -C target-feature=+crt-static
    -C strip=symbols
    -C debuginfo=0
)
encoded_flags=$(IFS=$'\x1f'; printf '%s' "${flags[*]}")
CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER="$linker" \
CARGO_ENCODED_RUSTFLAGS="$encoded_flags" \
    "$cargo_bin" build --locked --release --target "$target"

exe="${CARGO_TARGET_DIR:-$project_dir/target}/$target/release/audiomix.exe"
cp "$exe" "$payload/audiomix.exe"
cp LICENSE "$payload/LICENSE.txt"
cp docs/WINDOWS.md "$payload/README-WINDOWS.md"
cp docs/WINDOWS-VALIDATION.md "$payload/VALIDATION.md"
"$about_bin" generate assets/licenses.hbs --locked --fail --target "$target" \
    --output-file "$payload/THIRD-PARTY-NOTICES.html"

sysroot=$("$rustc_bin" --print sysroot)
cp "$sysroot/share/doc/rust/COPYRIGHT-library.html" "$payload/licenses/rust/"
cp -RL "$sysroot/share/doc/rust/licenses" "$payload/licenses/rust/"
cp "$LLVM_MINGW_ROOT/LICENSE.TXT" "$payload/licenses/toolchain/LLVM-LICENSE.txt"
cp -RL "$LLVM_MINGW_ROOT/x86_64-w64-mingw32/share/mingw32" "$payload/licenses/toolchain/"

# Fresh staging prevents stale files or local configuration entering the ZIP.
zip_path="$dist/$name.zip"
[[ ! -e "$zip_path" ]] || { printf 'Refusing to overwrite existing package: %s\n' "$zip_path" >&2; exit 1; }
(cd "$stage" && zip -qr "$zip_path" "$name")
cp "$payload/audiomix.exe" "$dist/$name.exe"
(cd "$dist" && sha256sum "$name.exe" "$name.zip" > SHA256SUMS)
printf 'Package: %s\nExecutable: %s\nChecksums: %s\n' "$zip_path" "$dist/$name.exe" "$dist/SHA256SUMS"

#!/usr/bin/env bash
# Builds fsearch-setup.exe (one file: service + launcher + first-run wizard) for Windows from Linux.
# Needs: rust target x86_64-pc-windows-gnu and a mingw-compatible cc/linker (set ZIG_WRAP to a folder with zcc/zlink/zar, or have mingw-w64 installed).
set -euo pipefail
cd "$(dirname "$0")"
T=x86_64-pc-windows-gnu
if [ -n "${ZIG_WRAP:-}" ]; then
  export PATH="$ZIG_WRAP:$PATH" CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$ZIG_WRAP/zlink" CC_x86_64_pc_windows_gnu="$ZIG_WRAP/zcc" AR_x86_64_pc_windows_gnu="$ZIG_WRAP/zar"
fi
export FSEARCH_BUILD="${FSEARCH_BUILD:-$(date +%d%H%M)}"
export RUSTFLAGS="-C link-arg=$PWD/assets/fsearch.rc"
mkdir -p payload
cargo build --release --target $T --features gui --bin fsearch --bin fsearch-gui
cp target/$T/release/fsearch.exe target/$T/release/fsearch-gui.exe payload/
# Optional Authenticode signing (needs osslsigncode): SIGN_KEY=key.pem SIGN_CERT=cert.pem ./build-installer.sh
sign() {
  [ -n "${SIGN_KEY:-}" ] || return 0
  osslsigncode sign -certs "$SIGN_CERT" -key "$SIGN_KEY" -n "fsearch" -i "https://github.com/musabasaleh-byte/fsearch" -h sha256 -in "$1" -out "$1.signed" >/dev/null
  mv "$1.signed" "$1"
}
sign payload/fsearch.exe
sign payload/fsearch-gui.exe
cargo build --release --target $T --features bundle --bin fsearch-setup
cp target/$T/release/fsearch-setup.exe ./fsearch-setup.exe
sign ./fsearch-setup.exe
echo "built fsearch-setup.exe (build $FSEARCH_BUILD)"

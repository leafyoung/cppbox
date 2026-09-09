#!/usr/bin/env bash
# Assembles a wasm32-wasip1(-threads) cross-compile toolchain for the host's
# existing native clang++, without requiring a full wasi-sdk install or an
# LLVM rebuild:
#
#   - wasi-sysroot + libclang_rt (wasm32 builtins) from the upstream
#     WebAssembly/wasi-sdk GitHub release (headers + prebuilt libc/libc++).
#   - wasm-ld, which the host clang++ needs as its wasm linker.
#   - a "resource-dir" overlay: a symlink farm over the host clang's own
#     resource dir, with the wasm32 builtins libs added, so clang++ can find
#     both its native runtime (for itself) and the wasm32 runtime (for the
#     --target=wasm32-wasip1* output) without copying 86MB of headers.
#
# Everything lands in .cache/ (gitignored) so nothing here is committed to
# git. Re-running is a no-op if the cache is already populated.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE="$HERE/.cache/wasi-toolchain"
WASI_SDK_VERSION="34.0"
WASI_SDK_TAG="wasi-sdk-34"
BASE_URL="https://github.com/WebAssembly/wasi-sdk/releases/download/${WASI_SDK_TAG}"

mkdir -p "$CACHE"

# --- wasm-ld -----------------------------------------------------------
WASM_LD="$(command -v wasm-ld || true)"
if [ -z "$WASM_LD" ] && command -v brew >/dev/null 2>&1; then
    if [ -x "$(brew --prefix)/opt/lld/bin/wasm-ld" ]; then
        WASM_LD="$(brew --prefix)/opt/lld/bin/wasm-ld"
    else
        echo "==> installing lld (for wasm-ld) via brew"
        brew install lld
        WASM_LD="$(brew --prefix)/opt/lld/bin/wasm-ld"
    fi
fi
if [ -z "$WASM_LD" ]; then
    echo "error: wasm-ld not found and no brew available." >&2
    echo "  install LLVM's lld (it ships wasm-ld) via your package manager," >&2
    echo "  e.g. 'brew install lld' or 'apt install lld' / 'dnf install lld'." >&2
    exit 1
fi
WASM_LD_DIR="$(dirname "$WASM_LD")"
echo "==> wasm-ld: $WASM_LD"

# --- wasi-sysroot + libclang_rt (cached download) -----------------------
SYSROOT="$CACHE/wasi-sysroot-${WASI_SDK_VERSION}"
if [ ! -d "$SYSROOT" ]; then
    echo "==> downloading wasi-sysroot-${WASI_SDK_VERSION} (~114MB)"
    curl -sL -o "$CACHE/wasi-sysroot.tar.gz" "${BASE_URL}/wasi-sysroot-${WASI_SDK_VERSION}.tar.gz"
    tar xzf "$CACHE/wasi-sysroot.tar.gz" -C "$CACHE"
    rm -f "$CACHE/wasi-sysroot.tar.gz"
fi
echo "==> sysroot: $SYSROOT"

LIBCLANG_RT="$CACHE/libclang_rt-${WASI_SDK_VERSION}"
if [ ! -d "$LIBCLANG_RT" ]; then
    echo "==> downloading libclang_rt-${WASI_SDK_VERSION}"
    curl -sL -o "$CACHE/libclang_rt.tar.gz" "${BASE_URL}/libclang_rt-${WASI_SDK_VERSION}.tar.gz"
    tar xzf "$CACHE/libclang_rt.tar.gz" -C "$CACHE"
    rm -f "$CACHE/libclang_rt.tar.gz"
fi
echo "==> libclang_rt: $LIBCLANG_RT"

# --- resource-dir overlay ------------------------------------------------
CLANGXX="${CLANGXX:-clang++}"
HOST_RESDIR="$("$CLANGXX" -print-resource-dir)"
RESDIR="$CACHE/resource-dir"
mkdir -p "$RESDIR/lib"
ln -sfn "$HOST_RESDIR/include" "$RESDIR/include"
for d in "$HOST_RESDIR"/lib/*; do
    ln -sfn "$d" "$RESDIR/lib/$(basename "$d")"
done
for target_dir in "$LIBCLANG_RT"/*/; do
    name="$(basename "$target_dir")"
    mkdir -p "$RESDIR/lib/$name"
    cp -f "$target_dir/libclang_rt.builtins.a" "$RESDIR/lib/$name/"
done
echo "==> resource-dir overlay: $RESDIR"

# --- env.sh --------------------------------------------------------------
cat > "$CACHE/env.sh" <<EOF
# source this file (or let cargo run pick it up via .cargo/config or the
# Rust harness reading these directly) before running the Phase 0 spike.
export CPPBOX_WASI_CLANGXX="$CLANGXX"
export CPPBOX_WASI_SYSROOT="$SYSROOT"
export CPPBOX_WASI_RESOURCE_DIR="$RESDIR"
export PATH="$WASM_LD_DIR:\$PATH"
EOF
echo "==> wrote $CACHE/env.sh"
echo "Done. Run: source \"$CACHE/env.sh\" && cargo run -p cppbox-wasi-poc"

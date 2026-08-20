# Deploying CPPBox

CPPBox ships as a single desktop binary (Tauri embedding the Rust backend).
No Python runtime. Builds for Linux (AppImage/deb), Windows (NSIS exe), and
macOS (dmg, arm64 + x86_64) via GitHub Actions on a `v*` tag.

## Architecture (post-rewrite)

```text
CPPBox (Tauri binary)
  └─ spawns axum server on 127.0.0.1:<dynamic> (in-process, tokio)
      ├─ API + admin (crates/cppbox-core)
      ├─ clangd LSP (/ws/lsp) + clang-format/syntax (host)
      └─ podman compile/run (cpp-sandbox image)
  └─ opens a webview to http://127.0.0.1:{port}
```

Data lives under the OS app-data dir (e.g. `~/.local/share/cppbox` on Linux,
`%APPDATA%/cppbox` on Windows, `~/Library/Application Support/cppbox` on macOS),
or `CPPBOX_ROOT` if set. The frontend is a bundled resource.

## One-time setup

### 1. Updater signing key (for auto-update)

```bash
npx @tauri-apps/cli signer generate -w "$HOME/.tauri/cppbox.key" -p ""
# prints a public key -> paste into src-tauri/tauri.conf.json plugins.updater.pubkey
# store the private key + empty password as GitHub Actions secrets:
#   TAURI_PRIVATE_KEY  (contents of ~/.tauri/cppbox.key)
#   TAURI_KEY_PASSWORD (empty, or the password you chose)
```

Without these the CI build still produces installers; auto-update just isn't
signed (Linux/Windows install silently, macOS Gatekeeper prompts).

### 2. Optional: OS code-signing (silent installs)

- **macOS**: `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` secrets (Apple
  Developer ID, ~$99/yr) — needed for notarization. Without it the dmg runs
  with a right-click→Open on first launch.
- **Windows**: a code-signing cert. Without it SmartScreen shows a click-through.

### 3. Sandbox image on ghcr.io

The CI `publish-sandbox-image` job pushes `ghcr.io/leafyoung/cppbox-sandbox:<tag>`

- `:latest` automatically on release. To point the app at it, set (in Admin →
  Remote collector's sibling, or env):

```bash
export CPPBOX_SANDBOX_IMAGE=ghcr.io/leafyoung/cppbox-sandbox:v0.1.0
```

On first launch the backend pulls it if missing (`podman pull`). Podman must be
installed; on macOS/Windows a `podman machine` must be running.

## Cutting a release

```bash
git tag v0.1.0
git push origin v0.1.0
# -> release.yml builds AppImage/.exe/.dmg, signs, uploads, writes latest.json,
#    and pushes the cpp-sandbox image to ghcr.io
```

## Local desktop run (dev)

```bash
CPPBOX_FRONTEND="$PWD/frontend" cargo run -p cppbox
```

## Retire the Python backend

The Rust backend (R1–R3) is feature-complete and verified against the same
data/cppbox.db + projects/ as Python. Once satisfied, delete `backend/` and
the `uv`/pyproject machinery — only `crates/cppbox-core`, `src-tauri`,
`frontend/`, `worker/`, and `Dockerfile.sandbox` remain.

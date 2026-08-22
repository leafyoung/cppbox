# TODO / Ponytail Deferred Items

Items deliberately skipped for now. Add when the requirement becomes concrete.
Status labels: `[DONE]` shipped · `[DEFERRED]` deliberately parked until its "when" fires · `[N/A]` not actually a gap.

- **User auth** `[DEFERRED]`: no login/accounts. Projects are global on one machine (desktop app). Add when multi-user needed.
- **HTTPS** `[DEFERRED]`: app serves localhost only; behind a reverse proxy (nginx/caddy) if ever exposed. Add when deployed to prod.
- **File uploads** `[DEFERRED]`: files come from the on-disk project tree; submissions arrive via the Cloudflare collector. Add browser upload when needed.
- **Docker image optimization** `[DEFERRED]`: Alpine clang image is ~610MB (now incl. gdb/lldb). A multi-stage build could slim it. Add when image size matters.
- **CodeMirror 6** `[DEFERRED]`: CM5 from CDN (one script tag). Upgrade to CM6 when more editor features needed.
- **LSP diagnostics source** `[DEFERRED]`: squiggles come from `-fsyntax-only`; clangd's `publishDiagnostics` is ignored to avoid double-marking. Switch to clangd-only when cross-file/semantic squiggles are needed.
- **clangd session lifecycle** `[DEFERRED]`: one clangd per WebSocket session (no pooling). Pool/reuse if spin-up becomes noticeable under load.
- **Signature help / go-to-definition** `[DEFERRED]`: clangd supports them; only completion + hover are wired. Add when wanted.
- **Web ↔ VS Code live sync** `[DEFERRED]`: both edit the same on-disk project folder, but the web file tree isn't file-watched — refresh manually after external edits. Add a fs watcher + push when concurrent editing is common.
- **Rename/create via prompt** `[DEFERRED]`: tree actions use `window.prompt`. Swap for inline inputs when polish matters.
- **C++23 keyword set == C++20** `[N/A]`: C++23 adds no new core keywords beyond C++20, so highlighting is identical for 20/23. Accurate, not a gap.
- **SSH/Remote-SSH bootstrap** `[DEFERRED]`: host exposes SSH (ap308:22); the user configures their VS Code Remote-SSH host entry. No auto keygen/config write yet.
- **Formatter indent width** `[TODO]`: clang-format runs with the default style (2-space); it shall use the configured indent size (settings `indent`) via `--style={BasedOnStyle: LLVM, IndentWidth: N}` so formatting matches the editor's indent dropdown.
- **Theme coverage** `[DEFERRED]`: CM editor + app CSS variables are themed per scheme; not every pixel (e.g. modal accents) is tuned. Expand when a scheme looks off.

## Before first release

1. `[DONE]` Tauri updater keypair: pubkey in `src-tauri/tauri.conf.json`; `TAURI_PRIVATE_KEY` + `TAURI_KEY_PASSWORD` secrets set on leafyoung/cppbox.
2. `[DEFERRED]` (optional) Apple/Windows code-signing secrets for silent installs — currently ships unsigned (macOS right-click→Open once; Windows SmartScreen click-through).
3. `[DONE]` `v0.1.0` tag pushed → release CI ran (AppImage/.exe/.dmg + `cppbox-sandbox` image to ghcr).

```bash
# one-time keypair setup (already done):
bunx @tauri-apps/cli signer generate -w cppbox.key
gh secret set TAURI_PRIVATE_KEY --repo leafyoung/cppbox < cppbox.key
gh secret set TAURI_KEY_PASSWORD --repo leafyoung/cppbox < cppbox.key_password
```

## Remaining optional polish (not blocking)

- `[DONE]` Real icon set (icon/ sources installed into src-tauri/icons + bundle.icon).
- `[DONE]` PR-time ci.yml: cargo fmt --check + cargo build --workspace per push/PR.
- `[DONE]` prek pre-commit hooks: hygiene checks + cargo fmt + worker JS syntax.
- `[DONE]` Retire backend/ + pyproject.toml — Python backend removed; Rust is the only backend.

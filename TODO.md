# TODO / Ponytail Deferred Items

Items deliberately skipped for now. Add when the requirement becomes concrete.

- **User auth**: no login/accounts. Snippets are global. Add when multi-user needed.
- **HTTPS**: behind a reverse proxy (nginx/caddy). Add when deployed to prod.
- **File uploads**: just paste code. Add when needed.
- **Docker image optimization**: Alpine's clang 21 is 570MB. A multi-stage build could slim it. Add when image size matters.
- **CodeMirror 6**: used CM5 from CDN (one script tag). Upgrade to CM6 when more editor features needed.
- **LSP diagnostics source**: squiggles currently come from Phase 1 (`-fsyntax-only`), clangd's `publishDiagnostics` is ignored to avoid double-marking. Switch to clangd-only diagnostics when cross-file/semantic squiggles are needed.
- **clangd session lifecycle**: one clangd per WebSocket session (no pooling). Pool/reuse if process spin-up becomes noticeable under load.
- **Signature help / go-to-definition**: clangd supports them; only completion + hover are wired. Add when wanted.
- **Web ↔ VS Code live sync**: both edit the same on-disk project folder, but the web file tree isn't file-watched — refresh manually after external edits. Add a fs watcher + push when concurrent editing is common.
- **Rename/create via prompt**: tree actions use `window.prompt`. Swap for inline inputs when polish matters.
- **C++23 keyword set == C++20**: C++23 adds no new core keywords beyond C++20, so highlighting is identical for 20/23. Accurate, not a gap.
- **SSH/Remote-SSH bootstrap**: host exposes SSH (ap308:22); the user configures their VS Code Remote-SSH host entry. No auto keygen/config write yet.
- **Theme coverage**: CM editor + app CSS variables are themed per scheme; not every pixel (e.g. modal accents) is tuned. Expand when a scheme looks off.

Before first release

1.  bunx @tauri-apps/cli signer generate → paste pubkey into tauri.conf.json, private key → TAURI_PRIVATE_KEY secret.
2.  (optional) Apple/Windows signing secrets for silent installs.
3.  git tag v0.1.0 && git push origin v0.1.0 → CI builds everything + pushes cppbox-sandbox to ghcr.

```bash
bunx @tauri-apps/cli signer generate -w cppbox.key
gh secret set TAURI_PRIVATE_KEY --repo leafyoung/cppbox < cppbox.key
gh secret set TAURI_KEY_PASSWORD --repo leafyoung/cppbox < cppbox.key_password
```

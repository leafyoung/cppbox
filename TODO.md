# TODO / Ponytail Deferred Items

## UI review round 1 — U1–U6

**U1–U5 shipped.** U6 (pedagogy) remains planned.

### U1 — State coherence & error handling (DONE)
- [x] One source of truth = `project` + startup auto-opens last project (`last_project_id` in settings.yaml, merged PUT)
- [x] Empty state overlay; Run/Rebuild/Debug/Format/Save/Submit disabled when no project
- [x] Toast system; transient status text; api() network failure → toast + Retry (the call resumes)

### U2 — Toolbar restructure, icons, hierarchy (DONE)
- [x] Single toolbar row; theme/font/indent moved into ⚙ gear popover; Format/Save neutral ghost; Run green; Submit accent; tooltips with shortcuts everywhere
- [x] Hand-rolled inline SVG icon set (17 icons, no deps); emoji gone from toolbar/sidebar/tree/modals/docs/debug
- [x] Logo subtitle now just `clangd`

### U3 — Bottom panel + status bar (DONE)
- [x] Output | Compile Log | Input | Problems tabs; stdin is a persisted per-project Input tab (`stdin` column on snippets)
- [x] Output header shows `exit N · X.XXs` every run
- [x] Compile Log leads with the exact compile command (Makefile `$(info …)` echo; stale auto-generated Makefiles regenerate at run)
- [x] Problems tab: aggregated clangd diagnostics, click → open file & jump
- [x] Status bar: backend dot (15s poll), clangd dot, ⚠/✕ counts → Problems, Ln/Col, Spaces, std, Saved · HH:MM

### U4 — Sidebar & files affordances (DONE)
- [x] Project rows: hover rename + delete; delete → Undo toast (restore API)
- [x] Trash: labeled restore/purge buttons with tooltips; tree buttons have title + aria-label

### U5 — Submit confirm, a11y, contrast (DONE)
- [x] Submit dialog lists the exact snapshot (file names + count) that gets graded
- [x] aria-labels on icon buttons; role=tablist + arrow-key nav on editor and output tabs; focus-visible rings; --dim contrast bumped in dark themes

### U6 — Pedagogy upgrades (DONE)
- [x] Toolchain popover per project: -Werror, ASan+UBSan (compiler-rt added to the sandbox image; workflow republishes it), -O0/-O2 → snippets.flags → Makefile; binary now depends on the Makefile so flag changes rebuild
- [x] Test runner: Tests tab — named cases (stdin + expected), Run Tests runs ./app per case with ✓/✗ badges and got-vs-expected diff; cases persist per project (snippets.tests)
- [x] Command palette (Ctrl+Shift+P): 16 commands, fuzzy filter, arrows+Enter
- [x] Student-side submission history: submission_log table; submit modal lists previous attempts (#counter · timeAgo)
- Deferred as before: real role gating needs auth

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
- **Formatter indent width** `[DONE]`: /api/format accepts `indent`; style becomes `{BasedOnStyle: LLVM, IndentWidth: N}` (frontend sends the indent setting; N=2 keeps plain LLVM).
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

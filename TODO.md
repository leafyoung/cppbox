# TODO / Ponytail Deferred Items

## UI review round 1 — planned phases U1–U6

Reviewer context: local desktop app (teacher tool), single user, no auth (deferred). Three review points are already shipped and need no work: debugger UI (breakpoints/variables/call stack), autosave (1.2s debounce), multi-file projects.

### U1 — State coherence & error handling (do first)
- [ ] One source of truth = `project`: startup auto-opens last project (persist `last_project_id` in settings.yaml via GET/PUT /api/settings)
- [ ] Empty state overlay in editor when no project ("Create or open a project" + button); disable Run/Rebuild/Debug/Format/Save/Submit/VS Code with explanatory titles
- [ ] `titleInput` disabled + placeholder when no project; Files panel shows the same state text
- [ ] Toast system (top-right, auto-dismiss, kind: info/error, optional action button); replace permanent `statusText` errors with transient status that reverts after ~4s
- [ ] `api()` wrapper: fetch TypeError → toast "Can't reach the CPPBox server" + Retry button re-invoking the failed call (closure); no raw browser strings in UI

### U2 — Toolbar restructure, icons, hierarchy
- [ ] Single toolbar row: [project name] [std] · Format Save (ghost) · spacer · ⚙(gear popover: theme/font/indent) · VS Code · Docs · Admin · Debug · Rebuild · Run(green) · Submit(accent)
- [ ] Gear popover holds theme/font-size/indent selects (settings move out of toolbar row); std stays in toolbar (project-level teaching control)
- [ ] Hand-rolled inline SVG icon set (`icon(name)` helper, stroke style, no new deps): play, bug, wand, save, refresh, upload, code, book, gear, trash, plus, file, folder; replace every emoji button toolbar+sidebar+tree
- [ ] Hierarchy: Run = green filled primary, Submit = accent; everything else neutral ghost; Format loses yellow
- [ ] Tooltips with shortcuts on all icon buttons (Ctrl+Enter, Shift+Ctrl+Enter, Shift+Alt+F, F5, Ctrl+S)
- [ ] Kill logo-subtitle redundancy: `clangd · c++17` → just `clangd` (std already in toolbar)

### U3 — Bottom panel + status bar
- [ ] Output panel tabs: Output | Compile Log | **Input** | **Problems**; delete the detached stdin bar
- [ ] Input tab = multi-line textarea, persisted per project: new `stdin` TEXT column on projects + accept in PUT (migration pattern exists); sent with Run as before
- [ ] Output header shows run metadata: `exit 0 · 1.24s` (wall-clock in frontend, zero backend change); also on success, not just nonzero
- [ ] Compile Log starts with the exact compile command (backend: prepend the clang++ line from the Makefile to compile_output)
- [ ] Problems tab: aggregated lspDiags across open files, file:line:col severity, click → open file & jump
- [ ] New bottom status bar: backend dot (poll /api/sandbox/status 15s; tooltip = init msg), LSP dot, ⚠/✕ counts (click → Problems), Ln/Col, Spaces:N, std, `Saved · HH:MM` (from autosave)

### U4 — Sidebar & files affordances
- [ ] Project row: keep active highlight; add hover ✏ rename (inline edit → same PUT as title input) + 🗑 delete with confirm; delete → Undo toast calling existing restore API (5s window)
- [ ] Trash head: tooltip "Deleted projects"; rows get labeled restore/purge buttons
- [ ] Files-panel icon-only buttons get title + aria-label

### U5 — Submit confirm, a11y, contrast
- [ ] Submit confirmation dialog: assignment name + exact file list (names, sizes) + "this snapshot is what gets graded"
- [ ] aria-label on every icon button; role=tablist/tab + arrow-key nav on editor tabs and output tabs; focus-visible rings on selects/buttons; bump --dim contrast for status strings

### U6 — Pedagogy upgrades (separate follow-up, not this round)
- [ ] Toolchain panel per project: warnings (-Wall -Wextra -Werror), sanitizers (ASan/UBSan), -O level → written into Makefile; compile command reflects it
- [ ] Test runner: expected-vs-actual cases with pass/fail badges before Submit
- [ ] Command palette (Ctrl+Shift+P); clangd quick-fixes on diagnostics
- [ ] Student-side submission history view
- Deferred as before: real role gating needs auth; reviewer's "dead Debug button" is wrong (lldb works)

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

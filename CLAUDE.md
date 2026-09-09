# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

CPPBox — a local-first C++ teaching IDE/compiler, shipped as a single Tauri
desktop binary. Two user-facing sides share one app: **student** (project
editor, run/build/debug/test/submit) and **admin/teacher** (classes,
students, assignments, key minting, collecting & marking submissions).

## Commands

```bash
# Dev run — browser, no Tauri window (prints CPPBOX_PORT=<n>; open it manually)
cargo run -p cppbox-core --bin cppbox-server
CPPBOX_NO_OPEN=1 cargo run -p cppbox-core --bin cppbox-server   # skip auto-open

# Dev run — desktop window (Tauri), same in-process server
CPPBOX_FRONTEND="$PWD/frontend" cargo run -p cppbox

# Format / lint / build (matches CI in .github/workflows/ci.yml)
cargo fmt --all -- --check
cargo fmt --all
cargo build --workspace --locked
```

There is no Rust test suite (`#[test]`) in the workspace today, and CI only
runs `cargo fmt --check` + `cargo build --workspace`. Testing is done by
running the app (see `docs/TEST.md`) or the Worker locally
(`worker/mock_server.py` — a stdlib-only stand-in for the Cloudflare Worker).

Pre-commit (`prek`, config in `.pre-commit-config.yaml`): hygiene checks,
`cargo fmt --all` (aborts the commit if it rewrites files — re-stage after),
and `node --check worker/src/index.js`.

Compile/run/debug prefer a bundled wasm32-wasip1 toolchain (`clang++` +
`wasmtime`, no host install, downloaded once on first launch — see
`docs/WASM_PLAN.md`) over **podman** (docker as fallback); podman remains
the fallback when that toolchain isn't ready, and stays required for
thread-using student code (`wasm32-wasip1-threads` is confirmed
non-functional upstream) and, unless the host also has `lldb-dap`, for
debugging. On first launch the backend pulls `cppbox-sandbox` from
ghcr.io if missing, same as before.

## Architecture

```
CPPBox (Tauri binary)
  └─ spawns axum server on 127.0.0.1:<dynamic> (in-process, tokio)
      ├─ API + admin routes      (crates/cppbox-core)
      ├─ clangd LSP (/ws/lsp) + clang-format/syntax
      │     (bundled wasm32-wasip1 toolchain, falls back to host install)
      ├─ compile+run              (wasmtime, falls back to podman)
      └─ debug (lldb-dap)         (native host process, falls back to podman)
  └─ opens a webview to http://127.0.0.1:{port}
```

- **`crates/cppbox-core`** — all real logic, as a library (`cppbox_core`)
  plus a standalone `cppbox-server` binary (same handshake Tauri uses:
  bind `127.0.0.1:0`, print `CPPBOX_PORT=<n>`, serve). Split by concern:
  - `routes.rs` — student-facing API: projects, file tree/read/move,
    run/check/format, sandbox status, per-project run/rebuild/check.
  - `admin.rs` — teacher-facing API: classes, student import, assignments,
    key minting, submission pull/organize, marking grid, submit endpoint,
    workspace-open, VS Code info.
  - `sandbox.rs` — compile/run/format/syntax-check + podman/docker
    invocation (prefers podman, rootless) as the fallback for thread-using
    code or when the bundled wasm toolchain isn't ready.
  - `wasi_exec.rs` — the bundled wasm32-wasip1 toolchain: fetches/assembles
    it (native `clang++`/`clang-format`/`wasm-ld` from wasi-sdk's own
    release, `clangd` separately), and `wasmtime`-sandboxed execution.
  - `storage.rs` — project file tree / snapshot / git-commit-on-submit logic.
  - `lsp.rs` / `debug.rs` — WebSocket bridges to clangd and to a debugger,
    one process per WS session (no pooling); `debug.rs` runs natively (no
    podman) when the host has `clang++`/`lldb-dap`.
  - `db.rs` — sqlite via sqlx; **migrations are inline `CREATE TABLE IF NOT
    EXISTS` statements in `migrate()`**, not a migration-file tool.
  - `settings.rs` — user prefs at `~/.cppbox/cppbox.yaml` (theme, font size,
    indent, std, last opened project), separate from the sqlite DB.
  - `remote.rs` — talks to the Cloudflare Worker collector (push keys / pull
    submission zips).
  - `lib.rs::build_app()` — wires `routes()` + `admin::routes()` +
    `CorsLayer::permissive()` + a fallback `ServeDir` for the frontend. This
    is the single entry point both `src-tauri` and `cppbox-server` call.
- **`src-tauri`** — thin shell: boots the axum server in a background task,
  waits for it to bind, then opens a webview pointed at it. Single-instance
  plugin (second launch just focuses the existing window). No external
  browser — UI lives in the window. Data dir: OS app-data dir (or
  `CPPBOX_ROOT` override).
- **`frontend/index.html`** — the entire client: one hand-written HTML file
  (markup + CSS + vanilla JS), no bundler/build step, no framework.
  CodeMirror 5 loaded from CDN (`<script>`/`<link>` tags at the top).
  CodeMirror 6 upgrade is explicitly out of scope (see `docs/TODO.md`). Served
  either as a static dir (`ServeDir`) or bundled as a Tauri resource.
- **`worker/`** — Cloudflare Worker (`src/index.js`, R2 + KV) acting as an
  always-on submission collector: students upload zips while the teacher's
  machine may be offline; the teacher's CPPBox instance later pulls the
  queue. `wrangler.toml` is gitignored (`.example` checked in);
  `mock_server.py` mirrors the same routes for local testing without a
  Cloudflare account.

### Submission/marking model (see `docs/DESIGN_SUBMISSION.md` for full design)

Feedback is a **separate** `feedback.md` file with `@file` references, not
in-place edits to student code — teacher and student artifacts are disjoint
files, so there's no merge to perform. Submission zips are named
`{key}+{counter}.zip`; unpacked student folders are named
`{serial:02d}-{name}` (serial always comes from the class import, never
auto-generated). Each submission is also a git commit
(`Submission #{seq} {ISO-ms}Z`) on the student's project, giving `@file`
references a stable target.

### Key env vars

`CPPBOX_ROOT` (data root override), `CPPBOX_FRONTEND` (frontend dir
override, used in dev), `CPPBOX_HOST`, `CPPBOX_NO_OPEN` (skip
auto-open-browser in the standalone server), `CPPBOX_SANDBOX_IMAGE`
(override the podman image), `CPPBOX_WORKER_URL` / `CPPBOX_WORKER_SECRET`
(Cloudflare Worker collector, overridable in Admin → Remote collector as a
DB `Setting` instead).

## Repo hygiene

`cppbox.key*` (Tauri updater signing key) is gitignored — never commit it.
`data/`, `projects/`, `workdir/`, `submissions/` are generated runtime state,
also gitignored — don't treat files found there as source.

# WASM migration plan (Option A)

Status: **approved, Phase 0 in progress**. Supersedes the open-ended
exploration in [WASM.md](WASM.md) with a concrete decision and phased plan.
Work happens in `src_v2/` (backend) and `frontend_v2/` (frontend) alongside
the current `crates/cppbox-core` + `frontend/`, until a phase is proven out
and cut over.

## Decision

[WASM.md](WASM.md) is a transcript of an earlier exploration that designed
for a *browser-only, no-backend* deployment (JupyterLite/xeus-cpp style):
Emscripten, a `clangd.wasm` in a Web Worker, OPFS, COOP/COEP headers. That
doesn't match what CPPBox actually is: a Tauri desktop binary with a
trusted, in-process native backend (`crates/cppbox-core`) and real
filesystem access. We don't need to fit the compiler inside a browser
sandbox — we already have a privileged host process.

**Chosen approach ("Option A"): keep clang++/clangd/clang-format running
natively on the host process (as today), and replace podman with
`wasmtime`-sandboxed *execution* of student code.** Compile with
`clang++ --target=wasm32-wasip1[-threads]`, run the resulting `.wasm` inside
a `wasmtime` sandbox (WASI preopens scoped to the job dir, no network
capability, memory limiter, epoch-based timeout) instead of a podman
container with `--network none --memory 512m`.

Rejected for now: the browser-Emscripten path from WASM.md ("Option B" —
`clangd.wasm`, Web Workers, OPFS). It solves a problem CPPBox doesn't have
(no privileged backend) at the cost of porting all of LLVM/clangd to
Emscripten and re-implementing LSP transport, filesystem, and a
from-scratch DWARF debugger. Revisit only if CPPBox ever needs a pure
static-web deployment with zero native backend.

## Corrected problem framing

Podman today isn't "the whole toolchain in a container" — it's narrower and
the current native-toolchain footprint is actually *broader* than the
CLAUDE.md description suggests:

| Feature | Today | Where |
|---|---|---|
| LSP (`clangd`) | native subprocess, must be on host PATH | `crates/cppbox-core/src/lsp.rs` |
| `clang-format` | native subprocess, must be on host PATH | `sandbox.rs::format_code` |
| syntax check (`-fsyntax-only`) | native subprocess, must be on host PATH | `sandbox.rs::check_syntax` |
| compile + run | **podman/docker container** | `sandbox.rs::compile_and_run`, `make_and_run` |
| debug (`lldb-dap`, ptrace) | **podman/docker container** | `debug.rs`, `sandbox.rs::compile_debug` |

So today, LSP/format/check already assume a native LLVM install on the
student's machine — podman only wraps the two paths that execute untrusted
student code. WASM (either option) should fix *both* problems: removing
podman **and** removing the hidden native-install requirement for
LSP/format/check, by bundling the toolchain instead of requiring it
pre-installed.

## Feasibility (Option A, this codebase)

| Component | Feasibility | Notes |
|---|---|---|
| Native `clang++ --target=wasm32-wasip1` compile | ★★★★★ | Proven in Phase 0 spike — works with off-the-shelf `wasi-libc` (Homebrew) + upstream `wasi-sdk` release sysroot/libclang_rt, no LLVM rebuild needed |
| `wasmtime` execution sandbox (stdin/stdout/exit code/timeout/memory limit) | ★★★★★ | Mature, production-grade (Fastly/Shopify use this exact pattern); replaces `compile_and_run`/`make_and_run` |
| Multi-file projects | ★★★★★ | Same `clang++` invocation shape as today, just a different `--target` |
| libc++ stdlib coverage (vector/string/algorithm/map/iostream) | ★★★★★ | Ships in the wasi-sysroot |
| `<filesystem>`, exceptions, templates | ★★★★☆ | Expected to work via WASI preopens; validate in Phase 0 |
| `std::thread`/`std::atomic`/`std::mutex`/condvar | ★★☆☆☆ | `wasm32-wasip1-threads` + `wasmtime-wasi-threads` exist but are explicitly experimental upstream; validate honestly, don't force it |
| `clang-format`/`clangd`/syntax-check bundling (no wasm needed) | ★★★★★ | Just ship native binaries with the app instead of requiring PATH install — independent of the wasm decision |
| Debugging (`lldb-dap` via ptrace) | ★★☆☆☆ | No wasm equivalent to ptrace; DWARF-based custom debugger is real, separate engineering — deferred (Phase 3) |

## Phases

**Phase 0 — toolchain spike (this step).** Prove compile+run end-to-end
outside the main app, using the *extended* test matrix from the WASM.md
side chat (not just the trivial subset): C++20, libc++, multi-file
compilation, templates, exceptions, `<filesystem>`, stdin/stdout,
`std::thread`/`std::mutex`/`std::atomic`/`condition_variable`, timeout,
crash, infinite loop. Lives in `src_v2/`. No product code touched.

**Phase 1 — replace `compile_and_run`/`make_and_run` internals.** Swap the
podman subprocess calls in `sandbox.rs` for `wasmtime` calls behind the same
function signatures. `routes.rs`, `admin.rs`, and the frontend are untouched
— `sandbox.rs` is already the right abstraction boundary.

**Phase 2 — stop requiring host-installed clang-format/clangd/clang++.**
Bundle native toolchain binaries (or a one-time download) instead of a
podman image pull, same lifecycle shape as `ensure_sandbox_image()` today.

**Phase 3 — debugging (deferred).** Interim: run debug sessions as a native
(non-wasm) debug build directly on the host, no podman — a student
debugging their own process isn't an adversarial scenario, so the
sandboxing podman gave `debug.rs` wasn't buying real security, just
packaging. A real wasm/DWARF debugger is a separate, later project.

**Phase 4 — drop podman/docker entirely.** Update `CLAUDE.md`,
`ensure_sandbox_image()`/`sandbox_status`, `Dockerfile.sandbox`, and
`DEPLOY.md` accordingly. Retire `src_v2`/`frontend_v2` by merging into
`crates/cppbox-core`/`frontend` (or renaming) once Phase 1-3 are validated.

## Open risks to track

- `wasm32-wasip1-threads` maturity in `wasmtime` — Phase 0 will report a
  clear pass/fail, not paper over it.
- Any curriculum content that assumes Linux syscalls (`fork`, `socket`,
  `ptrace` from student code, not just the debugger) won't map to WASI —
  none found in `docs/` as of this writing, but worth a deliberate check
  before Phase 1 ships.
- wasi-sysroot/libclang_rt binaries are ~100MB+ and must **not** be
  committed to git — `src_v2` fetches them into a local cache, gitignored.

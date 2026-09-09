# WASM migration plan (Option A, hybrid on threading)

Status: **Phase 0 complete (including the threading go/no-go gate) and
Phase 1 landed for `compile_and_run`.** Verdict: threading is confirmed
non-functional on wasm32-wasip1-threads today, and the curriculum does
need it (FN6806 has ~6 threading-focused lessons) — so the plan is a
hybrid: `wasmtime` for everything single-threaded (now live in
`crates/cppbox-core/src/wasi_exec.rs`), podman kept specifically for
thread-using assignments. Supersedes the open-ended exploration in
[WASM.md](WASM.md) with a concrete decision and phased plan.
`src_v2`/`frontend_v2` were the Phase 0 spike (kept for reference/reuse);
Phase 1 onward edits `crates/cppbox-core` directly, since it's now proven
out enough to build on rather than prototype separately.

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
| `<filesystem>`, exceptions, templates | ★★★★★ | Confirmed in Phase 0 — real C++ exceptions work (throw/catch, `.at()`), given the right (undocumented) clang/wasi-sysroot flag combination; see `src_v2/README.md` |
| `std::thread`/`std::atomic`/`std::mutex`/condvar | ★☆☆☆☆ | **Confirmed non-functional** (go/no-go gate, not a nice-to-have) — see "Threading go/no-go" below |
| `clang-format`/`clangd`/syntax-check bundling (no wasm needed) | ★★★★★ | Just ship native binaries with the app instead of requiring PATH install — independent of the wasm decision |
| Debugging (`lldb-dap` via ptrace) | ★★☆☆☆ | No wasm equivalent to ptrace; DWARF-based custom debugger is real, separate engineering — deferred (Phase 3) |

## Phases

**Phase 0 — toolchain spike. DONE for single-threaded execution.** Built in
`src_v2/` (see its README for the full writeup and how to reproduce). Result:

- **8/8 pass** on the extended single-threaded matrix (C++20, libc++,
  multi-file, templates, exceptions, `<filesystem>`, stdin/stdout, `abort()`
  trapping cleanly, an infinite loop cut off by epoch interruption) — this
  validates the core Option A thesis end-to-end: native `clang++
  --target=wasm32-wasip1` + `wasmtime` sandboxing is a working replacement
  for podman on ordinary teaching C++, no LLVM rebuild or wasm port of the
  compiler needed.
- **Multithreaded matrix: confirmed non-functional — see below.**

## Threading go/no-go

Threading was re-scoped from "nice to have" to a **hard go/no-go gate**:
`~/work/MFEg/FN6806/FN6806` (one of the two course repos, audited below) has
six threading-focused lesson directories (`54-thread`, `55-thread-atomic`,
`56-thread-struct`, `71-multithread_mc_pi`, `72-thread-mtx-cv`,
`73-thread-local-prng`), including `std::future`/`std::async`. This isn't
optional content.

**Verdict: does not work today, on wasm32-wasip1-threads + wasmtime-46.0.3,
even for the simplest possible case** (spawn one thread running a
zero-argument free function, no captures, no atomics). Reproduced
identically against wasmtime's own official prebuilt CLI binary
(`wasmtime run -W threads=y -W shared-memory=y -S threads=y`), not just
this project's harness — so this is not a bug in `src_v2`. Two distinct
issues were found and isolated:

1. **Fixed**: `wasmtime-wasi`'s `WasiP1Ctx` isn't `Clone`, and
   `wasmtime-wasi-threads` requires `Store<T>`'s `T: Clone` (each spawned
   thread gets its own `Store`). wasmtime's own CLI works around this with
   `Arc<Mutex<WasiP1Ctx>>` + `Arc::get_mut(..).expect(..)` — documented in
   their own source as "not actually compatible with wasi-threads" for
   concurrent access. `src_v2/src/threads.rs` fixes this by moving (not
   cloning) the host state into the `Store` so exactly one owner exists
   once execution starts.
2. **Fixed** (a real, general finding, not threading-specific): the
   compiled module's shared memory defaulted to `max=2 pages` (128KB) —
   nowhere near enough for a thread stack — because wasm-ld doesn't pick a
   sane default; `-Wl,--initial-memory=16777216 -Wl,--max-memory=268435456`
   fixes it. This one flag was also independently needed for a
   non-threading course sample (`73-cache_locality`, ~40MB of static
   arrays) — now applied unconditionally in `Toolchain::compile`.
3. **Not fixed, not fixable within this project's scope**: past both of
   those, thread spawning still traps with `uninitialized element` the
   moment the spawned thread tries to call its own entry function — a
   `call_indirect` into an empty function-table slot in the *newly spawned
   instance*. Root-caused with `wasm-tools print`: wasi-threads'
   multi-instance model re-instantiates the whole module per thread and
   shares only linear memory, not whatever establishes the function
   table's dynamic content — something the main instance's own startup
   path sets up isn't re-established for a `wasi_thread_start`-only
   instantiation. This is upstream wasi-threads/wasi-libc-34 ecosystem
   immaturity (matches why the original WASM.md exploration steered
   towards Emscripten over WASI specifically for thread support), not
   something addressable by CPPBox-side configuration.

**Resulting decision: hybrid, not a switch to Option B.** Rewriting the
whole execution model around Emscripten-in-browser (Option B) just to get
threading would throw away everything Phase 0 already proved works cleanly
for the other ~90% of the curriculum, and would still need its own
validation pass. Instead:

- **Non-threaded compile+run**: `wasmtime` (Option A), per Phase 1 below.
- **Thread-using assignments** (the six directories above): keep the
  existing podman path (`sandbox.rs`'s current `compile_and_run` via
  container), selected per-assignment rather than per-student-machine.
  This is a deliberate, scoped exception, not "give up on removing
  podman" — podman only needs to stay installed for a small, identifiable
  slice of the course.
- Revisit dropping podman entirely if/when wasi-threads matures upstream
  (worth periodically re-testing `src_v2/examples/threads_minimal` against
  new wasi-sdk/wasmtime releases — it's the smallest possible repro).

**Phase 1 — `compile_and_run` done; `make_and_run`/`compile_debug` still
podman-only.** `crates/cppbox-core/src/wasi_exec.rs` ports the Phase 0
spike into production: native `clang++ --target=wasm32-wasip1` +
`wasmtime` sandboxing (memory limiter, epoch-based timeout, WASI preopen
scoped to the job dir), with the same JSON contract `compile_and_run`
already returns. `sandbox::compile_and_run` dispatches to it when the wasi
toolchain is ready *and* `wasi_exec::uses_threading` doesn't flag the
source (textual check for `<thread>`/`<future>`/`<mutex>`/
`<condition_variable>`/`<atomic>`/`<shared_mutex>` — false positives just
mean an unnecessary podman fallback, which is safe) — otherwise it falls
through to the podman path unchanged, so this is purely additive, never a
regression. `routes.rs` gained one field (`wasi` status alongside the
existing podman one on `/api/sandbox/status`); `admin.rs` and the frontend
are untouched, confirming `sandbox.rs` was the right abstraction boundary.

The wasi toolchain (sysroot + resource-dir overlay) is fetched
opportunistically at startup (`ensure_wasi_toolchain`, spawned the same way
`ensure_sandbox_image` already is, in both `cppbox-server` and the Tauri
app) into `<root>/wasi-toolchain/` — gitignored, same pattern as `data/`/
`workdir/`. If the host has no `wasm-ld` (this project doesn't bundle one
yet — see Phase 2) or the download fails, the toolchain is marked
unready and every compile+run silently continues through podman exactly as
before; nothing about this is a hard requirement.

Verified end-to-end against the real HTTP API (not just `src_v2`'s
harness): clean compile+run, a compile error, stdin piping, an infinite
loop cut off at the 15s timeout, and — critically — that threaded code
(the classic 4-thread atomic-counter test) is correctly routed to podman
and returns the right answer (`4000000`), not silently misrouted into the
confirmed-broken wasm threading path. Also verified the toolchain
bootstrap itself from a fully cold start (no cached assets), not just with
`src_v2`'s already-downloaded cache reused.

`make_and_run` (multi-file project builds via the auto-generated
Makefile) and `compile_debug` are unchanged — deliberately out of scope
for this pass; the auto-generated Makefile would need a wasm-aware
variant, which is more surface area than the single-shot `/api/run` path
this phase targeted first.

**Phase 2 — stop requiring host-installed clang-format/clangd/clang++.**
Bundle native toolchain binaries (or a one-time download) instead of a
podman image pull, same lifecycle shape as `ensure_sandbox_image()` today.

**Phase 3 — debugging (deferred).** Interim: run debug sessions as a native
(non-wasm) debug build directly on the host, no podman — a student
debugging their own process isn't an adversarial scenario, so the
sandboxing podman gave `debug.rs` wasn't buying real security, just
packaging. A real wasm/DWARF debugger is a separate, later project.

**Phase 4 — shrink podman to "thread-using assignments only", not drop it
entirely.** Update `CLAUDE.md`, `ensure_sandbox_image()`/`sandbox_status`,
`Dockerfile.sandbox`, and `DEPLOY.md` to describe podman as a narrow,
assignment-scoped fallback rather than the universal execution backend.
Retire `src_v2`/`frontend_v2` by merging into `crates/cppbox-core`/
`frontend` (or renaming) once Phases 1-3 are validated. Fully dropping
podman is now conditional on wasi-threads maturing upstream — track via
the smallest repro (`src_v2/examples/threads_minimal`), don't block the
rest of the migration on it.

## Pinned toolchain versions and real download sizes

Compiler: whatever native `clang++` is already on the host (this project
used Homebrew clang++ 23.1.0 — any reasonably recent clang with wasm32
target support works, since the compiler itself isn't ported, see the
Decision above).

**wasm32-wasip1(-threads) sysroot: [`wasi-sdk-34`](https://github.com/WebAssembly/wasi-sdk/releases/tag/wasi-sdk-34)**
(2026; latest at time of writing). `src_v2` only needs two of its release
assets, not the full SDK (which bundles a redundant second clang+lld):

| Asset | Size | Used for |
|---|---:|---|
| `wasi-sysroot-34.0.tar.gz` | 114 MB | headers + libc/libc++/libc++abi, `eh`/`noeh` variants |
| `libclang_rt-34.0.tar.gz` | 757 KB | wasm32 compiler-rt builtins (`libclang_rt.builtins.a`) |

For reference, the *full* wasi-sdk bundle (own clang+lld+sysroot — **not**
what this project downloads, listed only so the "why not just use this"
tradeoff is explicit) per platform:

| Platform | Full wasi-sdk-34.0 tarball |
|---|---:|
| Linux x86_64 | 184 MB (128 MB `.deb`) |
| Linux arm64 | 184 MB (128 MB `.deb`) |
| macOS x86_64 | 175 MB |
| macOS arm64 | 172 MB |
| Windows x86_64 | 591 MB |
| Windows arm64 | 592 MB |

**For Phase 2 (bundling clangd/clang-format/clang++ instead of requiring a
host install)**: clangd publishes its own slim standalone releases (not
the full LLVM distribution) — [`clangd/clangd` v22.1.6](https://github.com/clangd/clangd/releases/tag/22.1.6):

| Platform | `clangd` release size |
|---|---:|
| Linux x86_64 | 109.5 MB |
| macOS | 93.6 MB |
| Windows | 26.9 MB |

`clang-format` and `clang++` itself have **no equivalently slim standalone
distribution** — the only official prebuilt source is the full
[`llvm/llvm-project` release](https://github.com/llvm/llvm-project/releases/tag/llvmorg-23.1.1)
(v23.1.1: Linux X64 1.9 GB, macOS ARM64 1.5 GB, Windows 610-860 MB). Phase 2
needs to decide between bundling that (a genuinely large download,
comparable to or bigger than today's podman image pull) or continuing to
require a native clang install for those two tools specifically — this
tradeoff should be made explicit to the user/course, not glossed over.

## Course content audit (FN6805 + FN6806)

Compiled every `.cpp` file under `~/work/MFEg/FN6805/FN6805` and
`~/work/MFEg/FN6806/FN6806` (146 files, 77 directories) against the pinned
toolchain, targeting `wasm32-wasip1-threads` (the superset target — compiles
non-threaded code identically to `wasm32-wasip1`), using each course's own
documented standard version (`-std=c++17` by default per both courses'
AGENTS.md, `-std=c++20` only for the two modules that name it explicitly —
the first audit pass had wrongly forced `-std=c++20` everywhere, which
produced a few false failures, corrected below).

**Final result: 74/77 directories compile cleanly — only one real,
unaddressed gap left (`std::execution::par`) across the whole two-course
sample set.** The first pass found 13 failing directories; after checking
each individually (including native `-stdlib=libc++` compiles, to separate
"wasm-specific" from "libc++-specific" from "pre-existing/unrelated"), 12
turned out to be small, genuine source bugs — unrelated to wasm, but only
surfaced because this was the first time this content had been compiled
with libc++/clang++ in this exact configuration — and have been fixed
directly in the course repos:

- **`std::valarray` was never actually missing** — this was **wrong** in
  the first pass of this audit and is corrected here. `52-mc_gbm/gbm_multi.h`
  used `valarray<double>` and `vector<double>` without including
  `<valarray>`/`<vector>` (relying on transitive includes libstdc++
  happens to provide and libc++ doesn't) — fixed by adding both includes.
  `~/work/MFEg/FN6806/FN6806/50-valarray` (the dedicated valarray lesson)
  already explicitly includes `<valarray>` and always compiled fine, which
  is what should have caught this the first time.
- **`std::execution::par` (parallel STL) is a real, confirmed gap** —
  `52-stl/test_for_each_parallel.cpp` already has `#include <execution>`
  and still gets "no member named 'par'"; this libc++ build has no
  parallel-STL backend. Left unfixed per instructions — the only remaining
  real gap in the whole audit.

  **Why**: unlike libstdc++ (which adopted Intel's Parallel STL, backed by
  a mandatory TBB dependency), libc++ has to build and run in contexts
  libstdc++ doesn't worry about as much — freestanding/no-thread builds,
  Apple platforms, cross-compilation targets like our own
  `wasm32-wasip1` — so it can't hang a hard TBB dependency off the
  standard library. Instead it designed a *pluggable* backend model
  (`serial`, `std_thread`, `libdispatch`, an OpenMP-offload backend in
  review) selected at libc++'s own build time via a CMake flag
  (`LIBCXX_PSTL_BACKEND`), not something a downstream consumer can switch
  on later. That architecture is still being finished: a libc++ maintainer
  said in 2023 "currently libc++ is broken with the PSTL... that will take
  a while," and P0024 (the parallel algorithms paper) is still tracked as
  "In Progress" on libc++'s own C++17 conformance page as of this writing.
  Neither Homebrew's general-purpose libc++ nor wasi-sdk's wasm32 sysroot
  build enables any backend for this, so the symbols are simply absent —
  and even a `std_thread`-backed build wouldn't help on
  `wasm32-wasip1-threads` specifically, since thread spawning itself is
  the confirmed-broken feature from the threading go/no-go section above.
  Sources: [libc++ PSTL Integration design
  doc](https://libcxx.llvm.org/DesignDocs/PSTLIntegration.html), [libc++
  C++17 status](https://libcxx.llvm.org/Status/Cxx17.html), [LLVM issue
  #99938](https://github.com/llvm/llvm-project/issues/99938), [LLVM
  Discourse
  thread](https://discourse.llvm.org/t/how-to-build-libc-with-pstl-support/69341).
- **Six small pre-existing source bugs, unrelated to wasm** (would have
  failed under this exact clang++/libc++ config regardless of compile
  target — several reproduce natively): `22-operator/main.cpp` had
  `explicit MyBool(int)` on a constructor the demo immediately relies on
  for implicit conversion — removed `explicit`.
  `3a-unique_ptr/main.cpp` called the wrong overload (`add_three`, which
  returns `void`, instead of `add_three_with_return`) — fixed the call.
  `30a-template_specialize/template_specialize.cpp` had a typo'd include
  (`"template_int.h"` for a file that doesn't exist, instead of
  `"template_specialize.h"`, which does) — fixed. `90-et-infinite-loop/main.cpp`
  had `using '\n';` (not valid C++) where `using std::endl;` was clearly
  intended — fixed. `92-et-vec-benchmark/non_et.h` had two stray bytes
  (`./`) corrupting the end of the file — removed. `70-chrono/main.cpp`
  wasn't a source bug at all — it's designed for `-std=c++17` (mixing a
  third-party date library with C++20's own chrono calendar support is
  what caused the original ambiguous-overload error) and compiles cleanly
  once the audit uses the right standard version.
- **One libc++-vs-libstdc++ portability gap, real but *not* wasm-specific**
  (`54-thread/test_thread.cpp` needed an explicit `#include <functional>`
  for `std::ref` that libstdc++ provides transitively and libc++ doesn't)
  — fixed with the include. This is a genuine migration cost from
  switching standard library implementations, independent of podman vs.
  wasm, worth knowing before Phase 1.
- **`96-test-xtensor-eigen`** (renamed from `96-test-quantlib-xtensor-eigen`
  after removing its QuantLib section — QuantLib is a compiled library,
  not header-only like Eigen/xtensor, and wasn't part of the ask): Eigen
  3.4.1 and xtensor 0.24.7 + xtl 0.7.5 (version-matched — see below) were
  downloaded into `FN6806/FN6806/third_party/`, and both compile and work
  under this target (confirmed by compiling the actual Eigen/xtensor code
  in this file, not just resolving the includes) — this directory now
  compiles cleanly. `52-mc_gbm`'s optional Eigen path
  (`gbm_multi_thread_eigen.*`, gated by `__has_include`) compiles too, as a
  side effect of the same vendoring.
- **Two directories correctly left untouched**: `30-header-file` and
  `72-multiple_inclusion` are documented in `FN6805/AGENTS.md` as
  **intentionally uncompilable teaching examples** — "the link errors are
  the lesson." The first audit pass mis-flagged these as script artifacts
  (compiling independent files together); the real reason is deeper —
  they're deliberately broken by design, not accidentally broken by the
  audit. Left exactly as they are.

Version-pinning note on the xtensor/xtl vendoring: xtensor's current
release (0.27.1) reorganized its headers into subdirectories
(`xtensor/containers/xarray.hpp` instead of `xtensor/xarray.hpp`), which
doesn't match this file's `#include <xtensor/xarray.hpp>` — needed 0.24.7
for the flat layout the existing code expects. 0.24.7 in turn needs an
xtl older than latest (0.8.2 dropped `xtl::sequence_size`/`negation`/
`conjunction`, which 0.24.7 still calls) — used 0.7.5, its own documented
minimum. One small vendor patch was needed: xtl 0.7.5's bundled `<span>`
polyfill (`xspan_impl.hpp`) calls `std::terminate()` without including
`<exception>` — added the include directly in the vendored copy.

## Open risks to track

- ~~`wasm32-wasip1-threads` maturity in `wasmtime`~~ — **resolved (as a
  finding, not a fix)**: confirmed broken, go/no-go gate answered "no" for
  pure Option A, hybrid plan adopted. See "Threading go/no-go" above.
- Any curriculum content that assumes Linux syscalls (`fork`, `socket`,
  `ptrace` from student code, not just the debugger) won't map to WASI —
  none found in the FN6805/FN6806 audit, but that audit checked
  compilability, not a semantic/syscall-usage grep; worth a deliberate
  check before Phase 1 ships.
- wasi-sysroot/libclang_rt binaries are ~100MB+ and must **not** be
  committed to git — `src_v2` fetches them into a local cache, gitignored.
- Phase 2's toolchain-bundling size tradeoff (clangd has a slim standalone
  release; clang-format/clang++ don't — see "Pinned toolchain versions"
  above) needs an explicit decision, not a default assumption that
  bundling is free.
- ~~The "which files belong together" heuristic...~~ — **resolved**: the
  audit script now uses each course's own documented build unit (whole
  directory for FN6806, matching its own AGENTS.md) and per-module
  standard version instead of a blanket guess; re-run clean at 74/77.
- ~~QuantLib remains unaddressed for `96-test-quantlib-xtensor-eigen`~~ —
  **resolved**: the QuantLib section was removed from that sample
  (renamed to `96-test-xtensor-eigen`) rather than vendoring QuantLib,
  which is a compiled library, not header-only like Eigen/xtensor, and
  would be a separate, nontrivial project if ever wanted.
- The vendored `third_party/eigen3`, `third_party/xtensor`, and
  `third_party/xtl` in `FN6806/FN6806` are pinned to specific versions
  chosen for compatibility with the existing `#include` paths and each
  other (see the course content audit above for why) — bumping any of them
  needs re-checking those constraints, not just grabbing "latest".

# src_v2 — Phase 0 WASM/wasmtime spike

Proves (or falsifies) the "Option A" approach from
[docs/WASM_PLAN.md](../docs/WASM_PLAN.md): compile student C++ with a
**native** host clang++ targeting `wasm32-wasip1(-threads)`, then execute it
sandboxed under `wasmtime` instead of podman. Not product code - see
`main.rs`'s module doc.

## Running it

```bash
./toolchain/setup.sh                       # one-time, populates .cache/ (gitignored)
source .cache/wasi-toolchain/env.sh
cargo run -p cppbox-wasi-poc
```

`setup.sh` needs `brew` (for `lld`, i.e. `wasm-ld`) and internet access (to
fetch the upstream `wasi-sysroot`/`libclang_rt` release tarballs from
`WebAssembly/wasi-sdk`). See the script for what it assembles and why.

Expect the multithreaded matrix to end the process with a Rust panic (exit
code 101, not a clean `[FAIL]` line) - that's the confirmed upstream bug
below surfacing through an abrupt thread-panic-then-orphaned-Arc cascade,
not a bug worth catching gracefully in this throwaway harness.

## Results (wasmtime 46.0.3, wasi-sdk 34.0, Homebrew clang++ 23.1.0)

**Single-threaded matrix (`wasm32-wasip1`): 8/8 pass.** hello, multifile
(headers across TUs), templates, exceptions (throw/catch + `.at()`),
`<filesystem>`, stdin/stdout, `abort()` (clean trap, not a hang), and an
infinite loop (cut off by epoch interruption, not left to spin). This is
the actual deliverable: ordinary teaching C++ compiles and runs correctly
under `wasmtime`, with real exceptions, real timeouts, and no host-installed
LLVM required by the *executing* side.

Three non-obvious toolchain findings, now encoded as comments in
`Toolchain::compile` (`src/main.rs`) so they aren't relearned next time:

- The wasi-sysroot ships **two** libc++/libc++abi builds per target - a
  small `noeh` one selected by default, and an `eh` one (needing
  `-fwasm-exceptions` plus explicit `-lunwind`) for real C++ exceptions.
- Clang's wasm backend defaults to the **legacy** wasm exception-handling
  encoding, which wasmtime's release build rejects outright
  ("`wasm_legacy_exceptions` feature not supported on this compiler
  configuration" - not a config knob, cranelift just doesn't implement it).
  `-mllvm -wasm-use-legacy-eh=false` switches clang to the standardized
  `try_table` encoding wasmtime does support. Found by cross-referencing
  `wasm-ld`/wasmtime error text against `llc -mattr=help`; not documented
  anywhere obvious.
- Unlike native compilation, wasm linear memory has a size wasm-ld fixes at
  link time, and the default is just "whatever static data needs" (as low
  as 2 pages / 128KB) - nowhere near enough for a real program's heap/stack
  (confirmed against a course sample needing ~40MB for static arrays, and
  separately against thread-stack allocation, see below).
  `-Wl,--initial-memory=16777216 -Wl,--max-memory=268435456` is now applied
  unconditionally.

**Multithreaded matrix (`wasm32-wasip1-threads`): confirmed non-functional
— this is a go/no-go finding, not a TODO.** Threading was re-scoped to a
hard requirement (the actual course content, `~/work/MFEg/FN6806`, has six
threading lessons including `std::future`/`std::async`), so this needed a
definitive answer, not "revisit later." Reproduced identically against
wasmtime's own official prebuilt CLI binary (`wasmtime run -W threads=y -W
shared-memory=y -S threads=y examples/threads_minimal/*.wasm`), not just
this project's harness. Three issues, isolated in order:

1. **Fixed**: `wasmtime-wasi`'s `WasiP1Ctx` isn't `Clone`, and
   `wasmtime-wasi-threads` requires `Store<T>`'s `T: Clone`. wasmtime's own
   CLI works around this with `Arc<Mutex<WasiP1Ctx>>` +
   `Arc::get_mut(..).expect(..)`, documented in their own source as "not
   actually compatible with wasi-threads" for concurrent access.
   `threads.rs` fixes this by moving (not cloning) the host state into the
   `Store` so exactly one owner exists once execution starts.
2. **Fixed**: the shared-memory-size issue above meant `malloc()` for a new
   thread's stack failed outright, so `std::thread`'s constructor threw
   *before the host was ever called* - the original, misleading symptom.
   Bumping initial/max memory got past this: threads now actually spawn.
3. **Not fixed, not addressable by this project**: past both of those,
   the spawned thread traps with `uninitialized element` the instant it
   tries to call its own entry function - reproduced even for the
   simplest possible case, a zero-argument free function with no
   captures, no atomics (`examples/threads_minimal`). Root-caused with
   `wasm-tools print`: it's a `call_indirect` into an empty function-table
   slot in the *newly spawned instance*. wasi-threads' MVP model
   re-instantiates the whole module per thread and shares only linear
   memory - whatever the main instance's own startup establishes in the
   function table isn't re-established for a `wasi_thread_start`-only
   instantiation. This is upstream wasi-threads/wasi-libc-34 ecosystem
   immaturity (this is exactly why the original WASM.md exploration steered
   toward Emscripten over WASI specifically for thread support), not a
   config knob.

**Decision**: hybrid, not a wholesale switch to Emscripten-in-browser.
Keep `wasmtime` (this project) for everything single-threaded; keep the
existing podman path specifically for the six thread-using assignments.
Full writeup and course-content audit in
[docs/WASM_PLAN.md](../docs/WASM_PLAN.md). If re-testing this against a
newer wasi-sdk/wasmtime release, `examples/threads_minimal/main.cpp` is the
smallest repro - if it passes, re-run `examples/threads_atomic` and
`examples/condvar_mutex` for the fuller check (correctness, not just
"doesn't trap").

## Layout

- `toolchain/setup.sh` - assembles the wasm32-wasip1(-threads) sysroot on
  top of the host's existing native clang++ (no LLVM rebuild).
- `examples/` - the test matrix, one C++ program per case.
- `src/main.rs` - single-threaded compile+run harness.
- `src/threads.rs` - multithreaded attempt, with the panic/exception
  findings above.

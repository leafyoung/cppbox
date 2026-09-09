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

## Results (wasmtime 46.0.3, wasi-sdk 34.0, Homebrew clang++ 23.1.0)

**Single-threaded matrix (`wasm32-wasip1`): 8/8 pass.** hello, multifile
(headers across TUs), templates, exceptions (throw/catch + `.at()`),
`<filesystem>`, stdin/stdout, `abort()` (clean trap, not a hang), and an
infinite loop (cut off by epoch interruption, not left to spin). This is
the actual deliverable: ordinary teaching C++ compiles and runs correctly
under `wasmtime`, with real exceptions, real timeouts, and no host-installed
LLVM required by the *executing* side.

Two non-obvious toolchain findings, now encoded as comments in
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

**Multithreaded matrix (`wasm32-wasip1-threads`): not working, root cause
narrowed but not fixed.** Two distinct issues surfaced:

1. `wasmtime-wasi`'s `WasiP1Ctx` isn't `Clone`, and `wasmtime-wasi-threads`
   requires `Store<T>`'s `T: Clone` (each spawned thread gets its own
   `Store`). The pattern wasmtime's own CLI uses - wrap it in
   `Arc<Mutex<WasiP1Ctx>>`, access via `Arc::get_mut(..).expect(..)` - is
   documented in their own source as "not actually compatible with
   wasi-threads" for concurrent access; it panics if two live clones both
   exist when a WASI call happens. **This part is fixed** in `threads.rs`
   by moving (not cloning) the host state into the `Store` so exactly one
   owner exists once execution starts.
2. Even past that, every threaded test case throws inside `std::thread`'s
   constructor itself (confirmed via `WASMTIME_BACKTRACE_DETAILS=1`, e.g.
   `threads_minimal` - a single spawn-and-join with no atomics - throws at
   `__thread/thread.h:232`), *before* the host's `wasi::thread-spawn` import
   is ever called (confirmed: zero log lines from `wasmtime_wasi_threads`
   even at `RUST_LOG=trace`). Root cause not isolated further - plausibly a
   wasi-libc-34/wasmtime-46 pairing issue in thread-runtime init, not
   something fixable by harness-side plumbing.

This matches what [docs/WASM_PLAN.md](../docs/WASM_PLAN.md) flagged as a
risk to validate honestly rather than paper over: **multithreaded student
code is not ready via this path today.** It does not block Phase 1 - no
curriculum content requiring `std::thread` was found in `docs/`, and
Option A's core value (dropping podman for ordinary compile+run) doesn't
need it. Revisit if/when the curriculum needs it; start from
`examples/threads_minimal/main.cpp`, the smallest repro.

## Layout

- `toolchain/setup.sh` - assembles the wasm32-wasip1(-threads) sysroot on
  top of the host's existing native clang++ (no LLVM rebuild).
- `examples/` - the test matrix, one C++ program per case.
- `src/main.rs` - single-threaded compile+run harness.
- `src/threads.rs` - multithreaded attempt, with the panic/exception
  findings above.

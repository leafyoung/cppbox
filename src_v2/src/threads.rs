//! Multithreaded matrix: wasm32-wasip1-threads + wasmtime-wasi-threads.
//!
//! **Confirmed non-functional as of wasi-sdk-34 + wasmtime-46.0.3 - this is
//! a go/no-go finding, not a TODO.** Reproduced against wasmtime's own
//! official CLI binary (not just this harness), isolated to two distinct,
//! independently-confirmed bugs:
//!
//! 1. `wasmtime-wasi`'s `WasiP1Ctx` isn't `Clone`, and `wasmtime-wasi-threads`
//!    requires `Store<T>`'s `T: Clone` (each spawned thread gets its own
//!    `Store`). The pattern wasmtime's own CLI uses - wrap it in
//!    `Arc<Mutex<WasiP1Ctx>>`, access via `Arc::get_mut(..).expect(..)` - is
//!    documented in their own source as "not actually compatible with
//!    wasi-threads" for concurrent access. **Fixed here** by moving (not
//!    cloning) the host state into the `Store` so exactly one owner exists
//!    once execution starts.
//! 2. Past that, spawning a thread still traps with `uninitialized element`
//!    inside `wasi_thread_start`, for the simplest possible case (one
//!    spawn, a zero-argument free function, no lambda). Root-caused via
//!    `wasm-tools print` + reproduced identically on wasmtime's official
//!    prebuilt CLI (`wasmtime run -W threads=y -S threads=y`): the compiled
//!    module's `call_indirect` for the thread's entry point resolves to an
//!    empty table slot in the *spawned* instance. wasi-threads' model
//!    re-instantiates the whole module per thread and only shares linear
//!    memory, not the function table's dynamic setup - some table content
//!    that the main instance's startup path establishes apparently isn't
//!    re-established for `wasi_thread_start`-only instantiation. This is
//!    upstream wasi-threads/wasi-libc-34 ecosystem immaturity, not a config
//!    knob or a bug in this harness. Not fixed; not planned to be within
//!    this project - see docs/WASM_PLAN.md for the resulting go/no-go call.
//!
//! (A separate, *fixed* red herring along the way: the module's shared
//! memory defaulted to `max=2 pages` (128KB), so plain thread-stack
//! allocation failed before ever reaching the table bug -
//! `Toolchain::compile`'s `--initial-memory`/`--max-memory` flags fix this
//! for every target, not just threads.)
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wasmtime::{Config, Engine, Linker, Module, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi_threads::WasiThreadsCtx;

use crate::Toolchain;

#[derive(Clone)]
struct ThreadHost {
    wasi: Arc<Mutex<WasiP1Ctx>>,
    wasi_threads: Option<Arc<WasiThreadsCtx<ThreadHost>>>,
}

fn wasip1_ctx(host: &mut ThreadHost) -> &mut WasiP1Ctx {
    Arc::get_mut(&mut host.wasi)
        .expect("WasiP1Ctx accessed while another thread clone is alive")
        .get_mut()
        .unwrap()
}

struct Case<'a> {
    name: &'a str,
    dir: &'a str,
    n_threads_hint: usize,
}

pub(crate) fn run_matrix(tc: &Toolchain, examples: &Path) {
    let cases = [
        Case {
            name: "threads_minimal",
            dir: "threads_minimal",
            n_threads_hint: 1,
        },
        Case {
            name: "threads_atomic",
            dir: "threads_atomic",
            n_threads_hint: 4,
        },
        Case {
            name: "condvar_mutex",
            dir: "condvar_mutex",
            n_threads_hint: 2,
        },
    ];

    for c in &cases {
        let dir = examples.join(c.dir);
        let started = Instant::now();
        match run_one(tc, &dir, Duration::from_secs(10)) {
            Ok((stdout, stderr)) => {
                println!(
                    "[DONE] {:<16} ({:?}) stdout={:?} stderr={:?}",
                    c.name,
                    started.elapsed(),
                    stdout.trim(),
                    stderr.trim()
                );
            }
            Err(e) => {
                println!(
                    "[FAIL] {:<16} ({:?}) hint={} threads: {e:#}",
                    c.name,
                    started.elapsed(),
                    c.n_threads_hint
                );
            }
        }
    }
}

fn run_one(tc: &Toolchain, src_dir: &Path, timeout: Duration) -> anyhow::Result<(String, String)> {
    let job = tempfile::tempdir()?;
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), job.path().join(entry.file_name()))?;
        }
    }
    let wasm_path = job.path().join("out.wasm");
    let (ok, log) = tc.compile(job.path(), &["main.cpp"], true, &wasm_path)?;
    if !ok {
        anyhow::bail!("compile failed:\n{log}");
    }

    let mut config = Config::new();
    config.wasm_threads(true);
    config.shared_memory(true);
    config.wasm_exceptions(true);
    config.epoch_interruption(true);
    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &wasm_path)?;

    let mut linker: Linker<ThreadHost> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, wasip1_ctx)?;

    let stdout = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let stderr = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let wasi = WasiCtxBuilder::new()
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .preopened_dir(
            job.path(),
            ".",
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        )?
        .build_p1();

    let mut host = ThreadHost {
        wasi: Arc::new(Mutex::new(wasi)),
        wasi_threads: None,
    };
    let mut store = Store::new(&engine, host.clone());

    // Register `wasi::thread-spawn` + the shared-memory import on the same
    // linker `WasiThreadsCtx` will use to instantiate spawned threads -
    // `WasiThreadsCtx::new` calls `instantiate_pre`, which requires every
    // import (including these two) already resolved.
    wasmtime_wasi_threads::add_to_linker(&mut linker, &store, &module, |h: &mut ThreadHost| {
        h.wasi_threads.as_ref().unwrap()
    })?;

    let linker = Arc::new(linker);
    let threads_ctx = WasiThreadsCtx::new(module.clone(), linker.clone(), false)?;
    host.wasi_threads = Some(Arc::new(threads_ctx));
    // Move (not clone) into the store - `wasip1_ctx`'s `Arc::get_mut` needs
    // refcount 1 the moment any WASI call happens, so `host` must not
    // survive as a second owner past this point.
    *store.data_mut() = host;

    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let engine_for_ticker = engine.clone();
    let ticker = std::thread::spawn(move || {
        if done_rx.recv_timeout(timeout).is_err() {
            engine_for_ticker.increment_epoch();
        }
    });
    store.set_epoch_deadline(1);

    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    let result = start.call(&mut store, ());
    let _ = done_tx.send(());
    let _ = ticker.join();

    let out_text = String::from_utf8_lossy(&stdout.contents()).into_owned();
    let err_text = String::from_utf8_lossy(&stderr.contents()).into_owned();
    if let Err(e) = result {
        anyhow::bail!("{e:#}\n  stdout={out_text:?}\n  stderr={err_text:?}");
    }
    Ok((out_text, err_text))
}

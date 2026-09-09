//! Multithreaded matrix: wasm32-wasip1-threads + wasmtime-wasi-threads.
//!
//! This follows the same pattern wasmtime's own CLI uses (see
//! `Host`/`unwrap_singlethread_context` in bytecodealliance/wasmtime's
//! `src/commands/run.rs`): the store data must be `Clone` for
//! `wasi-threads` to spawn per-thread `Store`s, so `WasiP1Ctx` is wrapped in
//! `Arc<Mutex<..>>` and accessed via `Arc::get_mut(..).expect(..)`. That
//! `expect` is the tell: wasmtime's own maintainers flag WASIp1 as "not
//! actually compatible with wasi-threads" for concurrent access - it panics
//! if two live thread clones both hold the Arc when a WASI call happens.
//! We report exactly what we observe rather than papering over it.
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

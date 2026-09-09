//! Phase 0 spike: prove that a *native* host clang++, targeting
//! wasm32-wasip1(-threads), plus wasmtime as the sandbox, can replace
//! podman for compiling and running student C++ - without porting
//! clang/clangd/LLVM to a wasm target themselves. See docs/WASM_PLAN.md.
//!
//! Not product code: no error-recovery polish, single file is fine here.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use wasmtime::{Config, Engine, Linker, Module, ResourceLimiter, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

mod threads;

pub(crate) struct Toolchain {
    clangxx: String,
    sysroot: PathBuf,
    resource_dir: PathBuf,
}

impl Toolchain {
    pub(crate) fn from_env() -> Result<Self> {
        let sysroot = std::env::var("CPPBOX_WASI_SYSROOT").context(
            "CPPBOX_WASI_SYSROOT not set - run ./toolchain/setup.sh then `source \
             .cache/wasi-toolchain/env.sh` first",
        )?;
        let resource_dir = std::env::var("CPPBOX_WASI_RESOURCE_DIR")
            .context("CPPBOX_WASI_RESOURCE_DIR not set - see CPPBOX_WASI_SYSROOT above")?;
        Ok(Self {
            clangxx: std::env::var("CPPBOX_WASI_CLANGXX").unwrap_or_else(|_| "clang++".into()),
            sysroot: PathBuf::from(sysroot),
            resource_dir: PathBuf::from(resource_dir),
        })
    }

    /// Compile `sources` (all under `dir`) to a single wasm32-wasip1(-threads)
    /// module. Returns (wasm path, compiler stdout+stderr).
    pub(crate) fn compile(
        &self,
        dir: &Path,
        sources: &[&str],
        threads: bool,
        out: &Path,
    ) -> Result<(bool, String)> {
        let target = if threads {
            "wasm32-wasip1-threads"
        } else {
            "wasm32-wasip1"
        };
        let mut cmd = Command::new(&self.clangxx);
        cmd.current_dir(dir)
            .arg(format!("--target={target}"))
            .arg(format!("--sysroot={}", self.sysroot.display()))
            .arg(format!("-resource-dir={}", self.resource_dir.display()))
            .arg("-std=c++20")
            .arg("-Wall")
            .arg("-Wextra")
            .arg("-O0")
            .arg("-g")
            // Teaching C++ needs real exceptions. The wasi-sysroot ships a
            // "noeh" libc++/libc++abi by default (small, no unwinding) and
            // an "eh" variant selected only when these are passed; the "eh"
            // libc++abi additionally needs libunwind. `-fwasm-exceptions`
            // alone (or with just `-mllvm -wasm-enable-eh`) emits the
            // *legacy* wasm exception encoding by default, which wasmtime's
            // release build rejects outright ("wasm_legacy_exceptions
            // feature not supported on this compiler configuration" - it's
            // not in cranelift's supported feature set at all, not a config
            // knob). `-wasm-use-legacy-eh=false` switches codegen to the
            // standardized try_table encoding wasmtime does support. None
            // of this is in the clang manual; found by iterating on
            // wasm-ld/wasmtime error messages against `llc -mattr=help`.
            .arg("-fwasm-exceptions")
            .arg("-mllvm")
            .arg("-wasm-enable-eh")
            .arg("-mllvm")
            .arg("-wasm-use-legacy-eh=false")
            // Unlike native, wasm linear memory has a size wasm-ld must fix
            // at link time; the default is just "whatever static data
            // needs" (as low as 2 pages / 128KB), which a real program's
            // heap/stack blows through immediately - confirmed against the
            // course sample set (`73-cache_locality` needs ~40MB just for
            // its *static* arrays specifically, which is why this is
            // --initial-memory, not just --max-memory: initial data is
            // placed at fixed offsets and can't rely on runtime growth).
            // 64MB initial / 256MB max is generous headroom, not a hard
            // student-code limit.
            .arg("-Wl,--initial-memory=67108864")
            .arg("-Wl,--max-memory=268435456");
        if threads {
            cmd.arg("-pthread");
        }
        for s in sources {
            cmd.arg(s);
        }
        cmd.arg("-lunwind").arg("-o").arg(out);

        let output = cmd
            .output()
            .with_context(|| format!("spawning {}", self.clangxx))?;
        let mut log = String::from_utf8_lossy(&output.stdout).into_owned();
        log.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), log))
    }
}

/// Per-instance memory cap, mirroring podman's `--memory 512m` today.
struct MemLimiter {
    max_bytes: usize,
}

impl ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _max: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= self.max_bytes)
    }
    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _max: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 10_000)
    }
}

struct HostState {
    wasi: WasiP1Ctx,
    limiter: MemLimiter,
}

#[derive(Debug)]
enum Outcome {
    Exited(i32),
    Trapped(String),
    TimedOut,
}

/// Compile + run one single-threaded test case under wasmtime, with a WASI
/// preopen scoped to a throwaway job dir (mirrors podman's per-job tmpdir +
/// `--network none`: no network capability is granted at all here), a
/// memory limiter, and an epoch-based timeout in place of "kill the
/// container".
fn run_case(
    tc: &Toolchain,
    name: &str,
    src_dir: &Path,
    sources: &[&str],
    stdin_data: &str,
    timeout: Duration,
) -> Result<(Outcome, String, String, String)> {
    let job = tempfile::tempdir()?;
    // Copy the whole example dir (not just the compiled sources) so headers
    // like foo.hpp come along too.
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), job.path().join(entry.file_name()))?;
        }
    }
    let wasm_path = job.path().join("out.wasm");
    let (ok, compile_log) = tc.compile(job.path(), sources, false, &wasm_path)?;
    if !ok {
        return Ok((
            Outcome::Trapped("compile failed".into()),
            compile_log,
            String::new(),
            String::new(),
        ));
    }

    let mut config = Config::new();
    config.epoch_interruption(true);
    config.wasm_exceptions(true);
    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &wasm_path)?;

    let mut linker: Linker<HostState> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut HostState| &mut s.wasi)?;

    let stdout = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let stderr = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let stdin = wasmtime_wasi::p2::pipe::MemoryInputPipe::new(stdin_data.to_string());

    let wasi = WasiCtxBuilder::new()
        .stdin(stdin)
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .preopened_dir(
            job.path(),
            ".",
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        )?
        .build_p1();

    let mut store = Store::new(
        &engine,
        HostState {
            wasi,
            limiter: MemLimiter {
                max_bytes: 256 * 1024 * 1024,
            },
        },
    );
    store.limiter(|s| &mut s.limiter);
    store.set_epoch_deadline(1);

    // background ticker: bump the epoch after `timeout`, forcing a trap if
    // the guest is still running - this is the wasmtime equivalent of
    // podman's "kill the container after N seconds".
    let engine_for_ticker = engine.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let ticker = std::thread::spawn(move || {
        if done_rx.recv_timeout(timeout).is_err() {
            engine_for_ticker.increment_epoch();
        }
    });

    let instance = linker.instantiate(&mut store, &module);
    let outcome = match instance.and_then(|i| {
        let start = i.get_typed_func::<(), ()>(&mut store, "_start")?;
        start.call(&mut store, ())
    }) {
        Ok(()) => Outcome::Exited(0),
        Err(e) => {
            // wasi p1 signals process exit via a special trap carrying the
            // exit code, not a normal return.
            if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                Outcome::Exited(exit.0)
            } else if matches!(
                e.downcast_ref::<wasmtime::Trap>(),
                Some(wasmtime::Trap::Interrupt)
            ) {
                Outcome::TimedOut
            } else {
                Outcome::Trapped(e.to_string())
            }
        }
    };
    let _ = done_tx.send(());
    let _ = ticker.join();

    let out_text = String::from_utf8_lossy(&stdout.contents()).into_owned();
    let err_text = String::from_utf8_lossy(&stderr.contents()).into_owned();
    let _ = name; // used only in caller's report line
    Ok((outcome, compile_log, out_text, err_text))
}

struct Case<'a> {
    name: &'a str,
    dir: &'a str,
    sources: &'a [&'a str],
    stdin: &'a str,
    timeout: Duration,
    expect: Expect,
}

enum Expect {
    /// substring that must appear in stdout
    StdoutContains(&'static str),
    NonZeroExit,
    Timeout,
}

fn main() -> Result<()> {
    env_logger::init();
    let tc = Toolchain::from_env()?;
    let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");

    let cases = [
        Case {
            name: "hello",
            dir: "hello",
            sources: &["main.cpp"],
            stdin: "",
            timeout: Duration::from_secs(5),
            expect: Expect::StdoutContains("1\n2\n3"),
        },
        Case {
            name: "multifile",
            dir: "multifile",
            sources: &["main.cpp", "foo.cpp"],
            stdin: "",
            timeout: Duration::from_secs(5),
            expect: Expect::StdoutContains("49"),
        },
        Case {
            name: "templates",
            dir: "templates",
            sources: &["main.cpp"],
            stdin: "",
            timeout: Duration::from_secs(5),
            expect: Expect::StdoutContains("42"),
        },
        Case {
            name: "exceptions",
            dir: "exceptions",
            sources: &["main.cpp"],
            stdin: "",
            timeout: Duration::from_secs(5),
            expect: Expect::StdoutContains("done"),
        },
        Case {
            name: "filesystem",
            dir: "filesystem",
            sources: &["main.cpp"],
            stdin: "",
            timeout: Duration::from_secs(5),
            expect: Expect::StdoutContains("hello from wasm"),
        },
        Case {
            name: "stdio",
            dir: "stdio",
            sources: &["main.cpp"],
            stdin: "1\n2\n3\n",
            timeout: Duration::from_secs(5),
            expect: Expect::StdoutContains("sum=6"),
        },
        Case {
            name: "crash",
            dir: "crash",
            sources: &["main.cpp"],
            stdin: "",
            timeout: Duration::from_secs(5),
            expect: Expect::NonZeroExit,
        },
        Case {
            name: "infinite_loop",
            dir: "infinite_loop",
            sources: &["main.cpp"],
            stdin: "",
            timeout: Duration::from_secs(2),
            expect: Expect::Timeout,
        },
    ];

    println!("=== single-threaded matrix (wasm32-wasip1) ===");
    let mut all_ok = true;
    for c in &cases {
        let dir = examples.join(c.dir);
        let started = Instant::now();
        let result = run_case(&tc, c.name, &dir, c.sources, c.stdin, c.timeout);
        let elapsed = started.elapsed();
        match result {
            Ok((outcome, compile_log, stdout, stderr)) => {
                let pass = match (&outcome, &c.expect) {
                    (Outcome::Exited(0), Expect::StdoutContains(s)) => stdout.contains(s),
                    (Outcome::Exited(code), Expect::NonZeroExit) => *code != 0,
                    (Outcome::Trapped(_), Expect::NonZeroExit) => true,
                    (Outcome::TimedOut, Expect::Timeout) => true,
                    _ => false,
                };
                all_ok &= pass;
                let outcome_desc = match &outcome {
                    Outcome::Exited(code) => format!("Exited({code})"),
                    Outcome::Trapped(msg) => format!("Trapped({msg})"),
                    Outcome::TimedOut => "TimedOut".to_string(),
                };
                println!(
                    "[{}] {:<16} {} ({:?}) stdout={:?}",
                    if pass { "PASS" } else { "FAIL" },
                    c.name,
                    outcome_desc,
                    elapsed,
                    stdout.trim()
                );
                if !pass {
                    println!("       compile log: {}", compile_log.trim());
                    println!("       stderr: {}", stderr.trim());
                }
            }
            Err(e) => {
                all_ok = false;
                println!("[FAIL] {:<16} harness error: {e:#}", c.name);
            }
        }
    }

    println!();
    println!("=== multithreaded matrix (wasm32-wasip1-threads) ===");
    threads::run_matrix(&tc, &examples);

    std::io::stdout().flush().ok();
    if !all_ok {
        bail!("one or more single-threaded cases failed - see log above");
    }
    Ok(())
}

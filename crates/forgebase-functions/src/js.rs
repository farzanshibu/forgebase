//! The embedded JavaScript engine (ADR-0027).
//!
//! [`ENGINE_WASM`] is QuickJS (quickjs-ng) with ForgeBase's glue and prelude,
//! built for wasm32-wasip1 by `scripts/build-js-engine.sh` and committed with
//! its SHA-256 (`js/forgebase-js.wasm.sha256`). It runs in a Wasmtime engine
//! configured like the WebAssembly runtime (fuel, memory limit, epoch
//! deadline, one fresh instance per execution) with a larger stack for the
//! interpreter. The module imports the `forgebase` I/O functions, one generic
//! `call(op, arg)` for every host operation ([`crate::jsops`]), and six WASI
//! functions answered by hand-written shims: real-time and monotonic clocks,
//! a CSPRNG, `fd_write` into the execution log, and `EBADF` for the rest.
//! There is no filesystem, no environment, no sockets and no process exit.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::OnceCell;
use wasmtime::{Caller, Config, Engine, Linker, Module, Trap};

use crate::error::{ExecError, FunctionError, Result};
use crate::host::{self, HostEnv, HostError};
use crate::model::Limits;
use crate::runtime::{
    self, HOST_CALL_FUEL, MAX_HOST_CALLS, MemorySnapshot, RunResult, StoreData, charge, finish, read_guest_max,
    start_ticker, to_i32, write_guest,
};

/// The engine module (QuickJS for wasm32-wasip1).
pub const ENGINE_WASM: &[u8] = include_bytes!("../js/forgebase-js.wasm");
/// `<sha256>  forgebase-js.wasm`, written by the build script.
const ENGINE_SHA256_FILE: &str = include_str!("../js/forgebase-js.wasm.sha256");

/// SHA-256 (hex) of [`ENGINE_WASM`] as recorded by the build script.
pub fn engine_sha256() -> &'static str {
    ENGINE_SHA256_FILE.split_whitespace().next().unwrap_or_default()
}

/// Largest output of a JavaScript execution (a 4 MiB body in base64 plus headers).
pub const MAX_JS_OUTPUT: usize = 6 * 1024 * 1024;
/// Largest argument of one host call (a 4 MiB object in base64).
pub const MAX_JS_ARG: usize = 8 * 1024 * 1024;
/// Smallest memory limit for a JavaScript version (the engine's own heap).
pub const MIN_JS_MEMORY: u64 = 4 * 1024 * 1024;

const ERRNO_BADF: i32 = 8;
const ERRNO_INVAL: i32 = 28;
const ERRNO_FAULT: i32 = 21;

/// The engine, compiled once on first use.
pub(crate) struct JsRuntime {
    engine: Engine,
    linker: Arc<Linker<StoreData>>,
    module: OnceCell<Module>,
    snapshot: OnceCell<Arc<MemorySnapshot>>,
}

/// Fuel and time for the one-time `init` (runtime, context, prelude).
const INIT_FUEL: u64 = 2_000_000_000;
const INIT_TIMEOUT: Duration = Duration::from_secs(30);

impl std::fmt::Debug for JsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JsRuntime(..)")
    }
}

impl JsRuntime {
    pub(crate) fn new() -> Result<Self> {
        let mut c = Config::new();
        c.consume_fuel(true);
        c.epoch_interruption(true);
        // QuickJS recurses on the native stack for nested calls; its own
        // check (768 KiB of the module's 1 MiB shadow stack) usually fires first.
        c.max_wasm_stack(1024 * 1024);
        c.wasm_simd(true);
        c.wasm_relaxed_simd(false);
        let engine = Engine::new(&c).map_err(|e| FunctionError::Internal(format!("js engine: {e}")))?;
        start_ticker(&engine)?;
        let mut linker = Linker::new(&engine);
        define(&mut linker).map_err(|e| FunctionError::Internal(format!("js host functions: {e}")))?;
        Ok(Self {
            engine,
            linker: Arc::new(linker),
            module: OnceCell::new(),
            snapshot: OnceCell::new(),
        })
    }

    /// The engine's memory after `_initialize` and `init`, taken once per
    /// process in a store of its own (no function code has run in it).
    pub(crate) async fn snapshot(&self) -> Result<Arc<MemorySnapshot>> {
        self.snapshot
            .get_or_try_init(|| async {
                let module = self.module().await?;
                let fail = |e: wasmtime::Error| {
                    FunctionError::Internal(format!("the JavaScript engine does not start: {e}"))
                };
                let mut linker: Linker<InitLog> = Linker::new(&self.engine);
                define_wasi(&mut linker).map_err(fail)?;
                linker.define_unknown_imports_as_traps(&module).map_err(fail)?;
                let mut store = wasmtime::Store::new(&self.engine, InitLog::default());
                store.set_fuel(INIT_FUEL).map_err(fail)?;
                store.set_epoch_deadline(INIT_TIMEOUT.as_millis() as u64 / runtime::TICK.as_millis() as u64);
                store.epoch_deadline_trap();
                let run = async {
                    let instance = linker.instantiate_async(&mut store, &module).await?;
                    instance
                        .get_typed_func::<(), ()>(&mut store, "_initialize")?
                        .call_async(&mut store, ())
                        .await?;
                    let status = instance
                        .get_typed_func::<(), i32>(&mut store, "init")?
                        .call_async(&mut store, ())
                        .await?;
                    if status != 0 {
                        return Err(wasmtime::Error::msg(format!(
                            "init returned {status}: {}",
                            store.data().lines.join(" | ")
                        )));
                    }
                    let mem = instance
                        .get_memory(&mut store, "memory")
                        .ok_or_else(|| wasmtime::Error::msg("no memory export"))?;
                    let memory = mem.data(&store).to_vec();
                    let sp = instance
                        .get_global(&mut store, "__stack_pointer")
                        .ok_or_else(|| wasmtime::Error::msg("no __stack_pointer export"))?
                        .get(&mut store);
                    let stack_pointer = sp.i32().ok_or_else(|| wasmtime::Error::msg("__stack_pointer is not i32"))?;
                    Ok(MemorySnapshot { memory, stack_pointer })
                };
                let snap = tokio::time::timeout(INIT_TIMEOUT, run)
                    .await
                    .map_err(|_| FunctionError::Internal("the JavaScript engine did not start in time".into()))?
                    .map_err(fail)?;
                let used = INIT_FUEL - store.get_fuel().unwrap_or(0);
                tracing::info!(
                    memory_bytes = snap.memory.len(),
                    init_fuel = used,
                    "JavaScript engine initialised"
                );
                Ok(Arc::new(snap))
            })
            .await
            .cloned()
    }

    /// The compiled engine module (compiled on a blocking thread the first time).
    pub(crate) async fn module(&self) -> Result<Module> {
        self.module
            .get_or_try_init(|| async {
                let engine = self.engine.clone();
                tokio::task::spawn_blocking(move || Module::from_binary(&engine, ENGINE_WASM))
                    .await
                    .map_err(|e| FunctionError::Internal(format!("js engine compile task: {e}")))?
                    .map_err(|e| FunctionError::Internal(format!("the embedded JavaScript engine does not load: {e}")))
            })
            .await
            .cloned()
    }

    /// Runs `js` (the version's JavaScript) once with `input`, in a fresh
    /// instance whose memory starts as the [`JsRuntime::snapshot`].
    pub(crate) async fn run(&self, limits: &Limits, env: Arc<HostEnv>, js: Vec<u8>, input: Vec<u8>) -> RunResult {
        let ready = async { Ok::<_, FunctionError>((self.module().await?, self.snapshot().await?)) };
        let (module, snap) = match ready.await {
            Ok(v) => v,
            Err(e) => {
                return RunResult {
                    result: Err(ExecError::Trap(e.to_string())),
                    fuel_used: 0,
                    logs: Vec::new(),
                };
            }
        };
        let mut data = StoreData::new(limits, env, input);
        data.source = js;
        data.max_output = MAX_JS_OUTPUT;
        let mut run = runtime::execute(&self.engine, &self.linker, &module, limits, data, Some(&snap)).await;
        if let Err(ExecError::Failed { message: Some(m), .. }) = &mut run.result {
            *m = own_frames(m);
        }
        run
    }
}

/// A JavaScript error trace without the prelude's frames.
fn own_frames(message: &str) -> String {
    message
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("at ") && (t.contains("(forgebase:prelude") || t.ends_with("(native)")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn monotonic_base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

/// The generic host call: `call(op_ptr, op_len, arg_ptr, arg_len)`.
async fn js_call(
    caller: &mut Caller<'_, StoreData>,
    op_ptr: i32,
    op_len: i32,
    arg_ptr: i32,
    arg_len: i32,
) -> wasmtime::Result<i32> {
    charge(
        caller,
        HOST_CALL_FUEL.saturating_add(u64::try_from(arg_len.max(0)).unwrap_or(0)),
    )?;
    let d = caller.data_mut();
    d.calls += 1;
    if d.calls > MAX_HOST_CALLS {
        return Err(wasmtime::Error::msg(format!(
            "more than {MAX_HOST_CALLS} host calls in one execution"
        )));
    }
    let Some(op) = read_guest_max(caller, op_ptr, op_len, 64).and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(finish(caller, Err(HostError::new("bad_argument", "invalid operation name"))));
    };
    let Some(args) = read_guest_max(caller, arg_ptr, arg_len, MAX_JS_ARG) else {
        return Ok(finish(
            caller,
            Err(HostError::new("bad_argument", "the argument is too large")),
        ));
    };
    match op.as_str() {
        "sleep" => {
            let ms = serde_json::from_slice::<serde_json::Value>(&args)
                .ok()
                .and_then(|v| v.get("ms").and_then(serde_json::Value::as_u64))
                .unwrap_or(0);
            // The execution's own timeout ends a sleep past the deadline.
            let left = caller.data().deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(Duration::from_millis(ms).min(left + Duration::from_millis(50))).await;
            return Ok(finish(caller, Ok(b"{}".to_vec())));
        }
        "fetch" => {
            let d = caller.data_mut();
            d.fetches += 1;
            let max = d
                .env
                .fetcher
                .as_ref()
                .map_or(0, |f| f.config().max_requests_per_execution);
            if d.fetches > max {
                let e = HostError::new("fetch_limit", format!("at most {max} fetch calls per execution"));
                return Ok(finish(caller, Err(e)));
            }
        }
        _ => {}
    }
    let env = caller.data().env.clone();
    let res = crate::jsops::run(&env, &op, &args).await;
    Ok(finish(caller, res))
}

fn define(linker: &mut Linker<StoreData>) -> wasmtime::Result<()> {
    runtime::define_io(linker, false)?;
    const M: &str = "forgebase";
    linker.func_wrap(M, "source_len", |caller: Caller<'_, StoreData>| -> i32 {
        to_i32(caller.data().source.len())
    })?;
    linker.func_wrap(
        M,
        "source_read",
        |mut caller: Caller<'_, StoreData>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            charge(&mut caller, HOST_CALL_FUEL)?;
            let n = usize::try_from(len).unwrap_or(0).min(caller.data().source.len());
            let bytes = caller.data().source[..n].to_vec();
            Ok(if write_guest(&mut caller, ptr, &bytes) {
                to_i32(n)
            } else {
                -1
            })
        },
    )?;
    linker.func_wrap_async(
        M,
        "call",
        |mut caller: Caller<'_, StoreData>, (op_ptr, op_len, arg_ptr, arg_len): (i32, i32, i32, i32)| {
            Box::new(async move { js_call(&mut caller, op_ptr, op_len, arg_ptr, arg_len).await })
        },
    )?;

    define_wasi(linker)?;
    Ok(())
}

/// Where `fd_write` output of the engine goes.
pub(crate) trait EngineLog {
    fn line(&mut self, level: &str, message: &str);
}

impl EngineLog for StoreData {
    fn line(&mut self, level: &str, message: &str) {
        host::push_log(self, level, message);
    }
}

/// Lines written while the engine initialises (reported if `init` fails).
#[derive(Debug, Default)]
struct InitLog {
    lines: Vec<String>,
}

impl EngineLog for InitLog {
    fn line(&mut self, _level: &str, message: &str) {
        if self.lines.len() < 20 {
            self.lines.push(message.to_owned());
        }
    }
}

fn mem_of<T>(caller: &mut Caller<'_, T>) -> Option<wasmtime::Memory> {
    caller.get_export("memory").and_then(wasmtime::Extern::into_memory)
}

fn mem_write<T>(caller: &mut Caller<'_, T>, ptr: i32, bytes: &[u8]) -> bool {
    let Ok(ptr) = usize::try_from(ptr) else { return false };
    mem_of(caller).is_some_and(|m| m.write(&mut *caller, ptr, bytes).is_ok())
}

fn mem_read<T>(caller: &mut Caller<'_, T>, ptr: i32, len: usize) -> Option<Vec<u8>> {
    let ptr = usize::try_from(ptr).ok()?;
    let m = mem_of(caller)?;
    let mut buf = vec![0u8; len];
    m.read(&*caller, ptr, &mut buf).ok()?;
    Some(buf)
}

fn charge_any<T>(caller: &mut Caller<'_, T>, cost: u64) -> wasmtime::Result<()> {
    let left = caller.get_fuel()?;
    if left < cost {
        caller.set_fuel(0)?;
        return Err(Trap::OutOfFuel.into());
    }
    caller.set_fuel(left - cost)
}

/// The six WASI functions the engine imports.
fn define_wasi<T: EngineLog + 'static>(linker: &mut Linker<T>) -> wasmtime::Result<()> {
    const W: &str = "wasi_snapshot_preview1";
    linker.func_wrap(
        W,
        "clock_time_get",
        |mut caller: Caller<'_, T>, id: i32, _precision: i64, out: i32| -> i32 {
            let ns: u128 = match id {
                0 => SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
                1..=3 => monotonic_base().elapsed().as_nanos(),
                _ => return ERRNO_INVAL,
            };
            let ns = u64::try_from(ns).unwrap_or(u64::MAX);
            if mem_write(&mut caller, out, &ns.to_le_bytes()) {
                0
            } else {
                ERRNO_FAULT
            }
        },
    )?;
    linker.func_wrap(
        W,
        "random_get",
        |mut caller: Caller<'_, T>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            charge_any(&mut caller, HOST_CALL_FUEL)?;
            let Ok(n) = usize::try_from(len) else { return Ok(ERRNO_INVAL) };
            if n > 65_536 {
                return Ok(ERRNO_INVAL);
            }
            let mut buf = vec![0u8; n];
            if getrandom::fill(&mut buf).is_err() {
                return Err(Trap::UnreachableCodeReached.into());
            }
            Ok(if mem_write(&mut caller, ptr, &buf) { 0 } else { ERRNO_FAULT })
        },
    )?;
    linker.func_wrap(
        W,
        "fd_write",
        |mut caller: Caller<'_, T>, fd: i32, iovs: i32, n_iovs: i32, written: i32| -> wasmtime::Result<i32> {
            charge_any(&mut caller, HOST_CALL_FUEL)?;
            if fd != 1 && fd != 2 {
                return Ok(ERRNO_BADF);
            }
            let n = usize::try_from(n_iovs).unwrap_or(0).min(64);
            let Some(vecs) = mem_read(&mut caller, iovs, n * 8) else {
                return Ok(ERRNO_FAULT);
            };
            let mut text = Vec::new();
            let mut total: u32 = 0;
            for v in vecs.chunks_exact(8) {
                let ptr = i32::from_le_bytes([v[0], v[1], v[2], v[3]]);
                let len = i32::from_le_bytes([v[4], v[5], v[6], v[7]]);
                total = total.saturating_add(u32::try_from(len.max(0)).unwrap_or(0));
                if text.len() < 16 * 1024
                    && let Some(b) = mem_read(&mut caller, ptr, usize::try_from(len.clamp(0, 16 * 1024)).unwrap_or(0))
                {
                    text.extend_from_slice(&b);
                }
            }
            let line = String::from_utf8_lossy(&text);
            let line = line.trim_end();
            if !line.is_empty() {
                caller.data_mut().line(if fd == 2 { "error" } else { "info" }, line);
            }
            Ok(if mem_write(&mut caller, written, &total.to_le_bytes()) {
                0
            } else {
                ERRNO_FAULT
            })
        },
    )?;
    linker.func_wrap(W, "fd_close", |_: Caller<'_, T>, _fd: i32| -> i32 { ERRNO_BADF })?;
    linker.func_wrap(
        W,
        "fd_fdstat_get",
        |_: Caller<'_, T>, _fd: i32, _out: i32| -> i32 { ERRNO_BADF },
    )?;
    linker.func_wrap(
        W,
        "fd_seek",
        |_: Caller<'_, T>, _fd: i32, _off: i64, _whence: i32, _out: i32| -> i32 { ERRNO_BADF },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn traces_keep_the_functions_frames() {
        let m = "Error: x\n    at host (forgebase:prelude:38:73)\n    at Promise (native)\n    at <anonymous> (index.js:20:27)";
        assert_eq!(own_frames(m), "Error: x\n    at <anonymous> (index.js:20:27)");
    }

    #[test]
    fn embedded_engine_matches_its_checksum() {
        let got: String = Sha256::digest(ENGINE_WASM).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, engine_sha256(), "rebuild with scripts/build-js-engine.sh");
    }

    #[test]
    fn engine_imports_only_the_shimmed_functions() {
        let rt = JsRuntime::new().unwrap();
        let module = Module::from_binary(&rt.engine, ENGINE_WASM).unwrap();
        const ALLOWED: &[&str] = &[
            "forgebase.input_len",
            "forgebase.input_read",
            "forgebase.source_len",
            "forgebase.source_read",
            "forgebase.output",
            "forgebase.result_read",
            "forgebase.log",
            "forgebase.call",
            "wasi_snapshot_preview1.clock_time_get",
            "wasi_snapshot_preview1.random_get",
            "wasi_snapshot_preview1.fd_write",
            "wasi_snapshot_preview1.fd_close",
            "wasi_snapshot_preview1.fd_fdstat_get",
            "wasi_snapshot_preview1.fd_seek",
        ];
        for imp in module.imports() {
            let name = format!("{}.{}", imp.module(), imp.name());
            assert!(ALLOWED.contains(&name.as_str()), "unexpected import {name}");
        }
    }
}

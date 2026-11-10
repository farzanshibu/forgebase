//! The Wasmtime sandbox: one engine, module validation, and executions with
//! fuel, memory and wall-clock limits.
//!
//! Each execution gets a fresh [`Store`] and instance: nothing survives
//! between executions except what the function writes through its
//! capabilities. The module sees only the `forgebase` host functions of
//! [`HOST_FUNCTIONS`]; there is no WASI, so no filesystem, clock, random
//! source, network or process access beyond the granted capabilities.
//!
//! Limits:
//!
//! * **Fuel**: `Config::consume_fuel`; every host call also costs fuel
//!   ([`HOST_CALL_FUEL`] plus its argument size). Running out traps.
//! * **Memory**: a [`ResourceLimiter`] refuses (and traps on) any growth of
//!   linear memory beyond the limit, including an initial size above it.
//! * **Wall clock**: epoch interruption (a ticker thread advances the epoch
//!   every [`TICK`]) traps a busy guest; a Tokio timeout around the whole
//!   execution also bounds time spent waiting in host calls such as fetch.
//!   The guest yields to the scheduler every [`YIELD_FUEL`] units of fuel, so
//!   a long computation never blocks a runtime thread for long.

use std::sync::Arc;
use std::time::Duration;

use wasmtime::{Caller, Config, Engine, ExternType, Linker, Module, ResourceLimiter, Store, Trap, ValType};

use crate::error::{ExecError, FunctionError, Result};
use crate::host::{self, HostEnv, Op};
use crate::model::{Limits, LogLine, VersionConfig};

/// Epoch tick.
pub const TICK: Duration = Duration::from_millis(5);
/// Fuel between cooperative yields to the async runtime.
pub const YIELD_FUEL: u64 = 1_000_000;
/// Fuel charged per host call (plus one unit per argument byte).
pub const HOST_CALL_FUEL: u64 = 10_000;
/// Largest argument of one host call.
pub const MAX_ARG_BYTES: usize = 4 * 1024 * 1024;
/// Largest output of one execution.
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// Host calls per execution.
pub const MAX_HOST_CALLS: u32 = 10_000;

/// A host function of the `forgebase` import module.
#[derive(Debug, Clone, Copy)]
pub struct HostFunction {
    /// Import name.
    pub name: &'static str,
    /// Capability it needs (`None`: always available).
    pub capability: Option<&'static str>,
    /// Number of `i32` parameters (every host function returns one `i32`).
    pub params: usize,
}

/// Every host function. ABI: arguments are passed as `(ptr, len)` of a
/// buffer in the module's exported `memory`; a call returns `n >= 0` on
/// success with an `n`-byte result, or `-n` with an `n`-byte JSON error
/// `{"code": ..., "message": ...}`; the result is copied out with
/// `result_read(ptr, len)`. See ADR-0017 §3.
pub const HOST_FUNCTIONS: &[HostFunction] = &[
    HostFunction {
        name: "input_len",
        capability: None,
        params: 0,
    },
    HostFunction {
        name: "input_read",
        capability: None,
        params: 2,
    },
    HostFunction {
        name: "output",
        capability: None,
        params: 2,
    },
    HostFunction {
        name: "result_read",
        capability: None,
        params: 2,
    },
    HostFunction {
        name: "log",
        capability: Some("log"),
        params: 3,
    },
    HostFunction {
        name: "db_get",
        capability: Some("db.read"),
        params: 2,
    },
    HostFunction {
        name: "db_list",
        capability: Some("db.read"),
        params: 2,
    },
    HostFunction {
        name: "db_create",
        capability: Some("db.write"),
        params: 2,
    },
    HostFunction {
        name: "db_update",
        capability: Some("db.write"),
        params: 2,
    },
    HostFunction {
        name: "db_delete",
        capability: Some("db.write"),
        params: 2,
    },
    HostFunction {
        name: "kv",
        capability: Some("kv"),
        params: 2,
    },
    HostFunction {
        name: "env_get",
        capability: Some("env"),
        params: 2,
    },
    HostFunction {
        name: "secret_get",
        capability: Some("secrets"),
        params: 2,
    },
    HostFunction {
        name: "fetch",
        capability: Some("fetch"),
        params: 2,
    },
];

/// Per-execution memory limiter.
#[derive(Debug)]
pub(crate) struct MemLimiter {
    pub(crate) max: usize,
    pub(crate) hit: bool,
}

impl ResourceLimiter for MemLimiter {
    fn memory_growing(&mut self, _current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        if desired > self.max {
            self.hit = true;
            return Err(wasmtime::Error::msg("memory limit exceeded"));
        }
        Ok(true)
    }

    fn table_growing(&mut self, _current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        Ok(desired <= 100_000)
    }

    fn instances(&self) -> usize {
        1
    }

    fn tables(&self) -> usize {
        4
    }

    fn memories(&self) -> usize {
        1
    }
}

/// State of one execution.
pub(crate) struct StoreData {
    pub(crate) limiter: MemLimiter,
    pub(crate) env: Arc<HostEnv>,
    pub(crate) input: Vec<u8>,
    pub(crate) output: Option<Vec<u8>>,
    pub(crate) result: Vec<u8>,
    pub(crate) logs: Vec<LogLine>,
    pub(crate) log_bytes: usize,
    pub(crate) calls: u32,
    pub(crate) fetches: u32,
    /// JavaScript the engine runs (empty for WebAssembly functions).
    pub(crate) source: Vec<u8>,
    /// When the execution's time limit ends (bounds `sleep`).
    pub(crate) deadline: std::time::Instant,
    /// Largest output.
    pub(crate) max_output: usize,
}

impl StoreData {
    pub(crate) fn new(limits: &Limits, env: Arc<HostEnv>, input: Vec<u8>) -> Self {
        Self {
            limiter: MemLimiter {
                max: usize::try_from(limits.memory_bytes).unwrap_or(usize::MAX),
                hit: false,
            },
            env,
            input,
            output: None,
            result: Vec::new(),
            logs: Vec::new(),
            log_bytes: 0,
            calls: 0,
            fetches: 0,
            source: Vec::new(),
            deadline: std::time::Instant::now() + Duration::from_millis(limits.timeout_ms),
            max_output: MAX_OUTPUT_BYTES,
        }
    }
}

/// What an execution produced.
#[derive(Debug)]
pub(crate) struct RunResult {
    pub(crate) result: std::result::Result<Option<Vec<u8>>, ExecError>,
    pub(crate) fuel_used: u64,
    pub(crate) logs: Vec<LogLine>,
}

/// The engine and the linker with every host function.
#[derive(Clone)]
pub(crate) struct Runtime {
    engine: Engine,
    linker: Arc<Linker<StoreData>>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Runtime(..)")
    }
}

pub(crate) fn start_ticker(engine: &Engine) -> Result<()> {
    let weak = engine.weak();
    std::thread::Builder::new()
        .name("forgebase-wasm-epoch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(TICK);
                match weak.upgrade() {
                    Some(e) => e.increment_epoch(),
                    None => break,
                }
            }
        })
        .map(|_| ())
        .map_err(|e| FunctionError::Internal(format!("cannot start the epoch thread: {e}")))
}

impl Runtime {
    pub(crate) fn new() -> Result<Self> {
        let mut c = Config::new();
        c.consume_fuel(true);
        c.epoch_interruption(true);
        c.max_wasm_stack(512 * 1024);
        c.wasm_simd(true);
        c.wasm_relaxed_simd(false);
        let engine = Engine::new(&c).map_err(|e| FunctionError::Internal(format!("wasm engine: {e}")))?;
        start_ticker(&engine)?;
        let mut linker = Linker::new(&engine);
        define_host(&mut linker).map_err(|e| FunctionError::Internal(format!("host functions: {e}")))?;
        Ok(Self {
            engine,
            linker: Arc::new(linker),
        })
    }

    /// Compiles (on a blocking thread) and validates a module for `cfg`.
    pub(crate) async fn compile(&self, wasm: Vec<u8>, cfg: &VersionConfig) -> Result<Module> {
        let engine = self.engine.clone();
        let module = tokio::task::spawn_blocking(move || Module::from_binary(&engine, &wasm))
            .await
            .map_err(|e| FunctionError::Internal(format!("compile task: {e}")))?
            .map_err(|e| {
                FunctionError::invalid(
                    "invalid_module",
                    format!("not a valid WebAssembly module: {}", short(&e.to_string())),
                )
            })?;
        validate(&module, cfg)?;
        Ok(module)
    }

    /// Runs `handle` once with `input`.
    pub(crate) async fn run(&self, module: &Module, limits: &Limits, env: Arc<HostEnv>, input: Vec<u8>) -> RunResult {
        let data = StoreData::new(limits, env, input);
        execute(&self.engine, &self.linker, module, limits, data, None).await
    }
}

/// A memory image of an initialised instance: linear memory and the stack
/// pointer (the module's only mutable global).
#[derive(Debug)]
pub(crate) struct MemorySnapshot {
    pub(crate) memory: Vec<u8>,
    pub(crate) stack_pointer: i32,
}

/// Runs `handle` of `module` once in a fresh store holding `data`. With
/// `restore`, the fresh instance's memory and stack pointer are first set
/// to the snapshot (the JavaScript engine after its one-time `init`).
pub(crate) async fn execute(
    engine: &Engine,
    linker: &Arc<Linker<StoreData>>,
    module: &Module,
    limits: &Limits,
    data: StoreData,
    restore: Option<&MemorySnapshot>,
) -> RunResult {
    {
        let mut store = Store::new(engine, data);
        store.limiter(|d| &mut d.limiter);
        let setup = store
            .set_fuel(limits.fuel)
            .and_then(|()| store.fuel_async_yield_interval(Some(YIELD_FUEL)));
        if let Err(e) = setup {
            return RunResult {
                result: Err(ExecError::Trap(format!("sandbox setup failed: {e}"))),
                fuel_used: 0,
                logs: Vec::new(),
            };
        }
        store.set_epoch_deadline(limits.timeout_ms.div_ceil(TICK.as_millis() as u64) + 1);
        store.epoch_deadline_trap();
        let linker = linker.clone();
        let timeout = Duration::from_millis(limits.timeout_ms);
        let outcome = tokio::time::timeout(timeout, async {
            let instance = linker.instantiate_async(&mut store, module).await?;
            if let Some(snap) = restore {
                let mem = instance
                    .get_memory(&mut store, "memory")
                    .ok_or_else(|| wasmtime::Error::msg("the engine exports no memory"))?;
                let have = mem.data_size(&store);
                if have < snap.memory.len() {
                    let page = mem.page_size(&store);
                    let pages = u64::try_from(snap.memory.len() - have)
                        .unwrap_or(u64::MAX)
                        .div_ceil(page);
                    mem.grow(&mut store, pages)?;
                }
                mem.write(&mut store, 0, &snap.memory)?;
                instance
                    .get_global(&mut store, "__stack_pointer")
                    .ok_or_else(|| wasmtime::Error::msg("the engine exports no stack pointer"))?
                    .set(&mut store, wasmtime::Val::I32(snap.stack_pointer))?;
            }
            let handle = instance.get_typed_func::<(), i32>(&mut store, "handle")?;
            handle.call_async(&mut store, ()).await
        })
        .await;
        let fuel_left = store.get_fuel().unwrap_or(0);
        let fuel_used = limits.fuel.saturating_sub(fuel_left);
        let data = store.into_data();
        let result = match outcome {
            Err(_) => Err(ExecError::Timeout(limits.timeout_ms)),
            Ok(Err(e)) => Err(classify(&e, &data, limits)),
            Ok(Ok(0)) => Ok(data.output),
            Ok(Ok(status)) => Err(ExecError::Failed {
                status,
                message: data.output.as_deref().map(|o| clip(&String::from_utf8_lossy(o), 2000)),
            }),
        };
        RunResult {
            result,
            fuel_used,
            logs: data.logs,
        }
    }
}

/// `s` cut to at most `max` bytes (on a character boundary), marked with `...`.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

pub(crate) fn short(s: &str) -> String {
    let s = s.lines().next().unwrap_or_default();
    if s.len() > 300 {
        let mut end = 300;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    } else {
        s.to_owned()
    }
}

fn classify(e: &wasmtime::Error, data: &StoreData, limits: &Limits) -> ExecError {
    if data.limiter.hit {
        return ExecError::MemoryLimit(limits.memory_bytes);
    }
    match e.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => ExecError::FuelExhausted(limits.fuel),
        Some(Trap::Interrupt) => ExecError::Timeout(limits.timeout_ms),
        Some(t) => ExecError::Trap(t.to_string()),
        None => ExecError::Trap(short(&e.to_string())),
    }
}

/// Checks imports against [`HOST_FUNCTIONS`] and the granted capabilities,
/// and the required exports (`handle: () -> i32`, `memory`).
fn validate(module: &Module, cfg: &VersionConfig) -> Result<()> {
    let bad = |m: String| FunctionError::invalid("invalid_module", m);
    for imp in module.imports() {
        if imp.module() != "forgebase" {
            return Err(bad(format!(
                "import {}.{} is not available: functions may import only the \"forgebase\" host functions",
                imp.module(),
                imp.name()
            )));
        }
        let Some(spec) = HOST_FUNCTIONS.iter().find(|h| h.name == imp.name()) else {
            return Err(bad(format!("forgebase.{} is not a host function", imp.name())));
        };
        let ExternType::Func(ft) = imp.ty() else {
            return Err(bad(format!("forgebase.{} must be imported as a function", imp.name())));
        };
        let params_ok = ft.params().len() == spec.params && ft.params().all(|p| matches!(p, ValType::I32));
        let results_ok = ft.results().len() == 1 && ft.results().all(|p| matches!(p, ValType::I32));
        if !params_ok || !results_ok {
            return Err(bad(format!(
                "forgebase.{} has type ({}) -> i32",
                spec.name,
                vec!["i32"; spec.params].join(", ")
            )));
        }
        if let Some(cap) = spec.capability
            && !cfg.has(cap)
        {
            return Err(FunctionError::invalid(
                "capability_not_granted",
                format!(
                    "the module imports forgebase.{} but the capability {cap:?} is not granted",
                    spec.name
                ),
            ));
        }
    }
    let mut handle = false;
    let mut memory = false;
    for exp in module.exports() {
        match (exp.name(), exp.ty()) {
            ("handle", ExternType::Func(ft)) => {
                if ft.params().len() != 0 || ft.results().len() != 1 || !ft.results().all(|r| matches!(r, ValType::I32))
                {
                    return Err(bad("the export \"handle\" must have type () -> i32".into()));
                }
                handle = true;
            }
            ("memory", ExternType::Memory(mt)) => {
                if mt.is_64() || mt.is_shared() {
                    return Err(bad("the exported memory must be a 32-bit, unshared memory".into()));
                }
                let initial = mt.minimum().saturating_mul(mt.page_size());
                if initial > cfg.limits.memory_bytes {
                    return Err(FunctionError::invalid(
                        "memory_limit_too_small",
                        format!(
                            "the module's initial memory ({initial} bytes) exceeds limits.memory_bytes ({})",
                            cfg.limits.memory_bytes
                        ),
                    ));
                }
                memory = true;
            }
            _ => {}
        }
    }
    if !handle {
        return Err(bad(
            "the module must export a function \"handle\" of type () -> i32".into()
        ));
    }
    if !memory {
        return Err(bad("the module must export its linear memory as \"memory\"".into()));
    }
    Ok(())
}

fn memory_of(caller: &mut Caller<'_, StoreData>) -> Option<wasmtime::Memory> {
    caller.get_export("memory").and_then(wasmtime::Extern::into_memory)
}

pub(crate) fn read_guest(caller: &mut Caller<'_, StoreData>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    read_guest_max(caller, ptr, len, MAX_ARG_BYTES)
}

/// Copies `len` bytes at `ptr` out of the guest's memory (at most `max`).
pub(crate) fn read_guest_max(caller: &mut Caller<'_, StoreData>, ptr: i32, len: i32, max: usize) -> Option<Vec<u8>> {
    let (ptr, len) = (usize::try_from(ptr).ok()?, usize::try_from(len).ok()?);
    if len > max {
        return None;
    }
    let mem = memory_of(caller)?;
    let mut buf = vec![0u8; len];
    mem.read(&*caller, ptr, &mut buf).ok()?;
    Some(buf)
}

pub(crate) fn write_guest(caller: &mut Caller<'_, StoreData>, ptr: i32, bytes: &[u8]) -> bool {
    let Ok(ptr) = usize::try_from(ptr) else { return false };
    let Some(mem) = memory_of(caller) else { return false };
    mem.write(&mut *caller, ptr, bytes).is_ok()
}

pub(crate) fn charge(caller: &mut Caller<'_, StoreData>, cost: u64) -> wasmtime::Result<()> {
    let left = caller.get_fuel()?;
    if left < cost {
        caller.set_fuel(0)?;
        return Err(Trap::OutOfFuel.into());
    }
    caller.set_fuel(left - cost)
}

pub(crate) fn to_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Stores `res` as the call result and returns the ABI status.
pub(crate) fn finish(caller: &mut Caller<'_, StoreData>, res: std::result::Result<Vec<u8>, host::HostError>) -> i32 {
    match res {
        Ok(bytes) => {
            let n = to_i32(bytes.len());
            caller.data_mut().result = bytes;
            n
        }
        Err(e) => {
            let bytes = e.to_json();
            let n = to_i32(bytes.len());
            caller.data_mut().result = bytes;
            -n
        }
    }
}

/// One capability call: fuel, call budget, capability check, argument read,
/// then the operation.
async fn call(caller: &mut Caller<'_, StoreData>, op: Op, ptr: i32, len: i32) -> wasmtime::Result<i32> {
    charge(
        caller,
        HOST_CALL_FUEL.saturating_add(u64::try_from(len.max(0)).unwrap_or(0)),
    )?;
    let data = caller.data_mut();
    data.calls += 1;
    if data.calls > MAX_HOST_CALLS {
        return Err(wasmtime::Error::msg(format!(
            "more than {MAX_HOST_CALLS} host calls in one execution"
        )));
    }
    let env = data.env.clone();
    if !env.config.has(op.capability()) {
        let e = host::HostError::new(
            "capability_denied",
            format!("the capability {:?} is not granted", op.capability()),
        );
        return Ok(finish(caller, Err(e)));
    }
    let Some(args) = read_guest(caller, ptr, len) else {
        let e = host::HostError::new(
            "bad_argument",
            "the argument is outside the module's memory or too large",
        );
        return Ok(finish(caller, Err(e)));
    };
    if matches!(op, Op::Fetch) {
        let d = caller.data_mut();
        d.fetches += 1;
        let max = env
            .fetcher
            .as_ref()
            .map_or(0, |f| f.config().max_requests_per_execution);
        if d.fetches > max {
            let e = host::HostError::new("fetch_limit", format!("at most {max} fetch calls per execution"));
            return Ok(finish(caller, Err(e)));
        }
    }
    let res = host::run(&env, op, args).await;
    Ok(finish(caller, res))
}

fn define_host(linker: &mut Linker<StoreData>) -> wasmtime::Result<()> {
    define_io(linker, true)?;
    for (name, op) in [
        ("db_get", Op::DbGet),
        ("db_list", Op::DbList),
        ("db_create", Op::DbCreate),
        ("db_update", Op::DbUpdate),
        ("db_delete", Op::DbDelete),
        ("kv", Op::Kv),
        ("env_get", Op::EnvGet),
        ("secret_get", Op::SecretGet),
        ("fetch", Op::Fetch),
    ] {
        linker.func_wrap_async(
            "forgebase",
            name,
            move |mut caller: Caller<'_, StoreData>, (ptr, len): (i32, i32)| {
                Box::new(async move { call(&mut caller, op, ptr, len).await })
            },
        )?;
    }
    Ok(())
}

/// Input, output, result and log functions (module `forgebase`). With
/// `log_capability`, `log` needs the `log` capability (WebAssembly ABI);
/// the JavaScript engine's console always logs.
pub(crate) fn define_io(linker: &mut Linker<StoreData>, log_capability: bool) -> wasmtime::Result<()> {
    const M: &str = "forgebase";
    linker.func_wrap(M, "input_len", |caller: Caller<'_, StoreData>| -> i32 {
        to_i32(caller.data().input.len())
    })?;
    linker.func_wrap(
        M,
        "input_read",
        |mut caller: Caller<'_, StoreData>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            charge(&mut caller, HOST_CALL_FUEL)?;
            let n = usize::try_from(len).unwrap_or(0).min(caller.data().input.len());
            let bytes = caller.data().input[..n].to_vec();
            Ok(if write_guest(&mut caller, ptr, &bytes) {
                to_i32(n)
            } else {
                -1
            })
        },
    )?;
    linker.func_wrap(
        M,
        "result_read",
        |mut caller: Caller<'_, StoreData>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            charge(&mut caller, HOST_CALL_FUEL)?;
            let n = usize::try_from(len).unwrap_or(0).min(caller.data().result.len());
            let bytes = caller.data().result[..n].to_vec();
            Ok(if write_guest(&mut caller, ptr, &bytes) {
                to_i32(n)
            } else {
                -1
            })
        },
    )?;
    linker.func_wrap(
        M,
        "output",
        |mut caller: Caller<'_, StoreData>, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            charge(
                &mut caller,
                HOST_CALL_FUEL.saturating_add(u64::try_from(len.max(0)).unwrap_or(0)),
            )?;
            if usize::try_from(len).unwrap_or(usize::MAX) > caller.data().max_output {
                return Ok(-1);
            }
            let max = caller.data().max_output;
            match read_guest_max(&mut caller, ptr, len, max) {
                Some(bytes) => {
                    caller.data_mut().output = Some(bytes);
                    Ok(0)
                }
                None => Ok(-1),
            }
        },
    )?;
    linker.func_wrap(
        M,
        "log",
        move |mut caller: Caller<'_, StoreData>, level: i32, ptr: i32, len: i32| -> wasmtime::Result<i32> {
            charge(
                &mut caller,
                HOST_CALL_FUEL.saturating_add(u64::try_from(len.max(0)).unwrap_or(0)),
            )?;
            if log_capability && !caller.data().env.config.has("log") {
                return Ok(-1);
            }
            let Some(bytes) = read_guest(&mut caller, ptr, len.min(16 * 1024)) else {
                return Ok(-1);
            };
            let level = match level {
                0 => "debug",
                1 => "info",
                2 => "warn",
                _ => "error",
            };
            host::push_log(caller.data_mut(), level, &String::from_utf8_lossy(&bytes));
            Ok(0)
        },
    )?;
    Ok(())
}

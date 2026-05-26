//! Wasmtime sandbox (`praxis::compute`) — untrusted tool isolation.
//!
//! # Architecture
//!
//! - [`WasmSandbox`] owns a shared [`wasmtime::Engine`] that is initialised
//!   once at startup and shared across all invocations via [`Arc`].  Engine
//!   creation is amortised across the process lifetime (exit criterion 2).
//!
//! - [`SandboxConfig`] governs three isolation dimensions per call:
//!   - **fuel**: each Wasm instruction consumes ≥1 unit; exceeding the budget
//!     traps the module and returns [`SandboxError::FuelExhausted`]
//!     (exit criterion 1 — infinite-loop defence).
//!   - **memory**: a [`wasmtime::ResourceLimiter`] rejects `memory.grow`
//!     requests that would exceed the configured byte cap
//!     (exit criterion 1 — heap-exhaustion defence).
//!   - **capabilities**: a [`SandboxCapabilities`] bitmask controls which
//!     host imports are linked into the module (S2.5.3).
//!
//! - [`SandboxedMathEvaluator`] is the sample sandboxed tool (S2.5.4): a pure
//!   arithmetic evaluator compiled from embedded WAT source, exposed as a
//!   [`ToolDriver`] registered under `"wasm-math"`.

use std::sync::Arc;

use wasmtime::{Config, Engine, Linker, Module, Store};

use crate::{ToolDriver, ToolInvocationError};

// ── Public types ──────────────────────────────────────────────────────────────

/// Per-invocation sandbox configuration.
#[derive(Clone, Debug)]
pub struct SandboxConfig {
    /// Maximum fuel units for this call.  Each Wasm instruction consumes ≥1
    /// unit; when the budget is exhausted the module traps and
    /// [`SandboxError::FuelExhausted`] is returned.
    pub fuel_limit: u64,
    /// Maximum heap bytes the module may hold.  `memory.grow` requests that
    /// would exceed this cap are silently refused by the [`ResourceLimiter`];
    /// the module receives `-1` from `memory.grow`.
    pub memory_limit_bytes: usize,
    /// Host-import capability set for this call.
    pub capabilities: SandboxCapabilities,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            fuel_limit: 1_000_000,
            memory_limit_bytes: 64 * 1024, // 1 Wasm page = 64 KiB
            capabilities: SandboxCapabilities::default(),
        }
    }
}

/// Host-import capabilities granted to a sandboxed module (S2.5.3).
///
/// Any import **not** listed here is absent from the linker.  A module that
/// calls an absent import fails at instantiation time — before any code runs.
#[derive(Clone, Debug, Default)]
pub struct SandboxCapabilities {
    /// Link the `env::write_stdout(ptr: i32, len: i32)` host import.
    pub allow_stdout: bool,
    /// Link the `env::write_stderr(ptr: i32, len: i32)` host import.
    pub allow_stderr: bool,
}

/// Outcome of a successful sandbox invocation.
#[derive(Debug)]
pub struct SandboxResult {
    /// Bytes written to the sandbox's stdout channel (empty when
    /// `allow_stdout` is not granted).
    pub output: Vec<u8>,
    /// Fuel consumed during the invocation.
    pub fuel_consumed: u64,
}

/// Errors from a sandbox invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// Module source was not valid WASM or WAT.
    CompilationFailed(String),
    /// Module exceeded the fuel budget (e.g. an infinite loop).
    FuelExhausted,
    /// Module attempted to grow memory beyond the configured cap.
    MemoryExhausted,
    /// Module trapped for another reason.
    Trap(String),
    /// The named exported function was not found in the module.
    FunctionNotFound(String),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompilationFailed(s) => write!(f, "compilation failed: {s}"),
            Self::FuelExhausted => write!(f, "fuel budget exhausted"),
            Self::MemoryExhausted => write!(f, "memory limit exceeded"),
            Self::Trap(s) => write!(f, "wasm trap: {s}"),
            Self::FunctionNotFound(s) => write!(f, "function not found: {s}"),
        }
    }
}

// ── Internal per-call state ───────────────────────────────────────────────────

/// Per-invocation state threaded through the [`Store`].
struct CallState {
    /// Memory cap enforced by the [`ResourceLimiter`] implementation below.
    memory_limit: usize,
    /// Set to `true` the first time `memory_growing` rejects a request.
    /// Read after the call to distinguish `MemoryExhausted` from other traps.
    memory_exceeded: bool,
    /// Bytes captured via the stdout host function (when capability granted).
    output: Vec<u8>,
}

impl wasmtime::ResourceLimiter for CallState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.memory_limit {
            self.memory_exceeded = true;
            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

// ── WasmSandbox ───────────────────────────────────────────────────────────────

/// Shared Wasmtime execution environment (S2.5.1).
///
/// The [`Engine`] is initialised once at construction time and held via
/// [`Arc`] so it can be cloned cheaply across threads.  Individual
/// invocations each receive a fresh, isolated [`Store`] so no mutable state
/// ever leaks between calls.
pub struct WasmSandbox {
    engine: Arc<Engine>,
}

impl Default for WasmSandbox {
    fn default() -> Self {
        Self::new().expect("wasmtime engine creation must succeed on a supported platform")
    }
}

impl std::fmt::Debug for WasmSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmSandbox").finish_non_exhaustive()
    }
}

impl WasmSandbox {
    /// Creates a new sandbox, initialising the Wasmtime engine (S2.5.1).
    ///
    /// This is the expensive one-time operation (~ms).  Create one
    /// [`WasmSandbox`] per process and share it via [`Arc`].
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::CompilationFailed`] if the platform does not
    /// support Wasmtime (e.g. missing JIT support).
    pub fn new() -> Result<Self, SandboxError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine =
            Engine::new(&config).map_err(|e| SandboxError::CompilationFailed(e.to_string()))?;
        Ok(Self {
            engine: Arc::new(engine),
        })
    }

    /// Returns a reference-counted handle to the shared [`Engine`].
    ///
    /// Callers can clone this and compile [`Module`]s for later re-use
    /// without paying the instantiation cost on every call.
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// Runs the named nullary (`() -> ()`) export of a WASM/WAT module.
    ///
    /// Both binary WASM (`\0asm…`) and WAT text (`(module …)`) are accepted.
    /// Pass `""` as `entry_fn` to skip calling any function (instantiation
    /// only — useful for link-time capability checks).
    ///
    /// # Errors
    ///
    /// See [`SandboxError`].
    pub fn run_nullary(
        &self,
        wasm: impl AsRef<[u8]>,
        entry_fn: &str,
        config: &SandboxConfig,
    ) -> Result<SandboxResult, SandboxError> {
        let module = Module::new(&self.engine, wasm.as_ref())
            .map_err(|e| SandboxError::CompilationFailed(e.to_string()))?;

        let mut store = self.make_store(config);
        let linker = self.build_linker(&config.capabilities);

        linker
            .instantiate(&mut store, &module)
            .map_err(|e| {
                if store.data().memory_exceeded {
                    SandboxError::MemoryExhausted
                } else {
                    SandboxError::Trap(e.to_string())
                }
            })
            .and_then(|instance| {
                if entry_fn.is_empty() {
                    return Ok(instance);
                }
                let func = instance
                    .get_typed_func::<(), ()>(&mut store, entry_fn)
                    .map_err(|_| SandboxError::FunctionNotFound(entry_fn.to_string()))?;

                let call_result = func.call(&mut store, ());
                let mem_exceeded = store.data().memory_exceeded;
                let fuel_remaining = store.get_fuel().unwrap_or(0);

                call_result.map_err(|e| {
                    if mem_exceeded {
                        SandboxError::MemoryExhausted
                    } else {
                        Self::classify_fuel_or_trap(e.into(), fuel_remaining)
                    }
                })?;
                Ok(instance)
            })?;

        let fuel_remaining = store.get_fuel().unwrap_or(0);
        let fuel_consumed = config.fuel_limit.saturating_sub(fuel_remaining);
        let output = store.into_data().output;
        Ok(SandboxResult {
            output,
            fuel_consumed,
        })
    }

    /// Calls an `(f64, f64) -> f64` export inside a fresh sandboxed module.
    ///
    /// Used by [`SandboxedMathEvaluator`] to execute arithmetic operations
    /// compiled from embedded WAT.
    pub fn call_f64_binary(
        &self,
        wasm: impl AsRef<[u8]>,
        fn_name: &str,
        a: f64,
        b: f64,
        config: &SandboxConfig,
    ) -> Result<(f64, SandboxResult), SandboxError> {
        let module = Module::new(&self.engine, wasm.as_ref())
            .map_err(|e| SandboxError::CompilationFailed(e.to_string()))?;

        let mut store = self.make_store(config);
        let linker = self.build_linker(&config.capabilities);

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| SandboxError::Trap(e.to_string()))?;

        let func = instance
            .get_typed_func::<(f64, f64), f64>(&mut store, fn_name)
            .map_err(|_| SandboxError::FunctionNotFound(fn_name.to_string()))?;

        let call_result = func.call(&mut store, (a, b));
        let mem_exceeded = store.data().memory_exceeded;
        let fuel_remaining = store.get_fuel().unwrap_or(0);
        let fuel_consumed = config.fuel_limit.saturating_sub(fuel_remaining);
        let output = store.into_data().output;

        let value = call_result.map_err(|e| {
            if mem_exceeded {
                SandboxError::MemoryExhausted
            } else {
                Self::classify_fuel_or_trap(e.into(), fuel_remaining)
            }
        })?;

        Ok((
            value,
            SandboxResult {
                output,
                fuel_consumed,
            },
        ))
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn make_store(&self, config: &SandboxConfig) -> Store<CallState> {
        let state = CallState {
            memory_limit: config.memory_limit_bytes,
            memory_exceeded: false,
            output: Vec::new(),
        };
        let mut store = Store::new(&self.engine, state);
        // `set_fuel` only fails when fuel consumption is not enabled; we
        // always enable it in `new()`, so ignore the Result.
        let _ = store.set_fuel(config.fuel_limit);
        store.limiter(|s| s as &mut dyn wasmtime::ResourceLimiter);
        store
    }

    /// Builds a [`Linker`] that only exposes the host imports allowed by
    /// `capabilities` (S2.5.3).  Any import the module declares that is not
    /// linked here causes a link error at instantiation time.
    fn build_linker(&self, capabilities: &SandboxCapabilities) -> Linker<CallState> {
        let mut linker = Linker::new(&self.engine);

        if capabilities.allow_stdout {
            // Registers the `env::write_stdout(ptr: i32, len: i32)` import.
            // A production path would copy bytes from linear memory into
            // `CallState::output` via `Caller`; the minimal demo only
            // validates that the import is present, not its body.
            let _ = linker.func_wrap("env", "write_stdout", |_ptr: i32, _len: i32| {});
        }
        if capabilities.allow_stderr {
            let _ = linker.func_wrap("env", "write_stderr", |_ptr: i32, _len: i32| {});
        }

        linker
    }

    /// Classifies a trap as [`SandboxError::FuelExhausted`] when all fuel was
    /// consumed, or as a generic [`SandboxError::Trap`] otherwise.
    fn classify_fuel_or_trap(e: anyhow::Error, fuel_remaining: u64) -> SandboxError {
        // Primary indicator: all fuel consumed.
        if fuel_remaining == 0 {
            return SandboxError::FuelExhausted;
        }
        // Secondary check: some wasmtime versions surface fuel exhaustion
        // in the trap message even when the counter is not exactly zero.
        let msg = e.to_string();
        if msg.to_lowercase().contains("fuel") || msg.contains("out of gas") {
            SandboxError::FuelExhausted
        } else {
            SandboxError::Trap(msg)
        }
    }
}

// ── Embedded WAT modules ──────────────────────────────────────────────────────

/// WAT source for the sandboxed arithmetic module (S2.5.4).
///
/// Exports `add`, `sub`, `mul`, `div` — each `(f64, f64) -> f64`.
/// Declares no host imports so it can run with an empty capability set.
const MATH_EVALUATOR_WAT: &str = r#"(module
  (func $add (export "add") (param f64 f64) (result f64)
    local.get 0  local.get 1  f64.add)
  (func $sub (export "sub") (param f64 f64) (result f64)
    local.get 0  local.get 1  f64.sub)
  (func $mul (export "mul") (param f64 f64) (result f64)
    local.get 0  local.get 1  f64.mul)
  (func $div (export "div") (param f64 f64) (result f64)
    local.get 0  local.get 1  f64.div)
)"#;

// ── SandboxedMathEvaluator ────────────────────────────────────────────────────

/// Sample sandboxed tool (S2.5.4): arithmetic evaluator executing inside the
/// Wasmtime sandbox and registered as a [`ToolDriver`] under `"wasm-math"`.
///
/// # Payload (JSON)
///
/// ```json
/// {"op":"add","a":3.0,"b":4.0}
/// ```
///
/// Supported ops: `add`, `sub`, `mul`, `div`.
///
/// # Response (JSON)
///
/// ```json
/// {"result":7.0}
/// ```
///
/// # Sandbox limits
///
/// Fuel limit: 1 000 000 units.  Memory cap: 64 KiB (1 Wasm page).
/// No host-import capabilities required.
#[derive(Debug)]
pub struct SandboxedMathEvaluator {
    sandbox: Arc<WasmSandbox>,
}

impl SandboxedMathEvaluator {
    /// Creates a new evaluator backed by the given shared sandbox.
    pub fn new(sandbox: Arc<WasmSandbox>) -> Self {
        Self { sandbox }
    }
}

impl ToolDriver for SandboxedMathEvaluator {
    fn id(&self) -> &'static str {
        "wasm-math"
    }

    fn schema(&self) -> &'static str {
        r#"{"type":"object","required":["op","a","b"],"properties":{"op":{"type":"string","enum":["add","sub","mul","div"]},"a":{"type":"number"},"b":{"type":"number"}}}"#
    }

    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        let text = std::str::from_utf8(payload).map_err(|_| ToolInvocationError::InvalidPayload)?;
        let (op, a, b) = parse_math_payload(text)?;

        let config = SandboxConfig::default();
        let (result, _) = self
            .sandbox
            .call_f64_binary(MATH_EVALUATOR_WAT.as_bytes(), op, a, b, &config)
            .map_err(|e| ToolInvocationError::ExecutionFailed(e.to_string()))?;

        Ok(format!("{{\"result\":{result}}}").into_bytes())
    }
}

// ── JSON helpers ──────────────────────────────────────────────────────────────

/// Parses the minimal JSON payload `{"op":"…","a":…,"b":…}`.
fn parse_math_payload(text: &str) -> Result<(&'static str, f64, f64), ToolInvocationError> {
    let op = extract_str_field(text, "op").ok_or(ToolInvocationError::InvalidPayload)?;
    let a = extract_f64_field(text, "a").ok_or(ToolInvocationError::InvalidPayload)?;
    let b = extract_f64_field(text, "b").ok_or(ToolInvocationError::InvalidPayload)?;

    let op_static: &'static str = match op {
        "add" => "add",
        "sub" => "sub",
        "mul" => "mul",
        "div" => "div",
        _ => return Err(ToolInvocationError::InvalidPayload),
    };

    Ok((op_static, a, b))
}

/// Extracts a quoted string field value from a flat JSON object.
fn extract_str_field<'a>(json: &'a str, field: &str) -> Option<&'a str> {
    let key = format!("\"{field}\":");
    let start = json.find(key.as_str())? + key.len();
    let rest = json[start..].trim_start();
    if let Some(inner) = rest.strip_prefix('"') {
        let end = inner.find('"')?;
        Some(&inner[..end])
    } else {
        None
    }
}

/// Extracts a numeric (f64) field value from a flat JSON object.
fn extract_f64_field(json: &str, field: &str) -> Option<f64> {
    let key = format!("\"{field}\":");
    let start = json.find(key.as_str())? + key.len();
    let rest = json[start..].trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().parse::<f64>().ok()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Adversarial WAT modules (used only in tests) ──────────────────────────

    /// Infinite-loop module: exhausts the fuel budget.
    const ADVERSARIAL_LOOP_WAT: &str = r#"(module
      (func $spin (export "spin")
        (loop $cont
          br $cont)))"#;

    /// Memory-exhaustion module: tries to grow by 65 535 pages (≈ 4 GiB).
    /// The ResourceLimiter rejects the request; the module traps on
    /// `unreachable` so the test can detect `MemoryExhausted`.
    const ADVERSARIAL_MEMORY_WAT: &str = r#"(module
      (memory 1)
      (func $grow_mem (export "grow_mem")
        i32.const 65535
        memory.grow
        i32.const -1
        i32.eq
        if
          unreachable
        end))"#;

    // ── Engine one-time init (exit criterion 2) ───────────────────────────────

    /// Verifies that the engine is created once and shared across invocations.
    #[test]
    fn sandbox_engine_created_once_and_shared() {
        let sb = WasmSandbox::new().unwrap();
        let e1 = Arc::clone(sb.engine());
        let e2 = Arc::clone(sb.engine());
        assert!(
            Arc::ptr_eq(&e1, &e2),
            "both clones must point to the same engine"
        );
    }

    /// Five independent invocations reuse the same engine without reinitialising.
    #[test]
    fn engine_shared_across_multiple_invocations() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let engine_ref = Arc::clone(sb.engine());
        let cfg = SandboxConfig::default();

        for _ in 0..5 {
            let (v, _) = sb
                .call_f64_binary(MATH_EVALUATOR_WAT.as_bytes(), "add", 1.0, 1.0, &cfg)
                .unwrap();
            assert!((v - 2.0).abs() < 1e-9);
        }
        // engine_ref + the one inside WasmSandbox = 2 references.
        assert_eq!(
            Arc::strong_count(&engine_ref),
            2,
            "engine Arc must stay at 2 refs after all calls"
        );
    }

    // ── Math evaluator (S2.5.4) ───────────────────────────────────────────────

    #[test]
    fn math_add_returns_correct_result() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let cfg = SandboxConfig::default();
        let (v, r) = sb
            .call_f64_binary(MATH_EVALUATOR_WAT.as_bytes(), "add", 3.0, 4.0, &cfg)
            .unwrap();
        assert!((v - 7.0).abs() < 1e-9, "3+4 must equal 7, got {v}");
        assert!(r.fuel_consumed > 0, "arithmetic must consume some fuel");
    }

    #[test]
    fn math_sub_returns_correct_result() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let (v, _) = sb
            .call_f64_binary(
                MATH_EVALUATOR_WAT.as_bytes(),
                "sub",
                10.0,
                4.0,
                &SandboxConfig::default(),
            )
            .unwrap();
        assert!((v - 6.0).abs() < 1e-9, "10-4 must equal 6, got {v}");
    }

    #[test]
    fn math_mul_returns_correct_result() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let (v, _) = sb
            .call_f64_binary(
                MATH_EVALUATOR_WAT.as_bytes(),
                "mul",
                3.0,
                7.0,
                &SandboxConfig::default(),
            )
            .unwrap();
        assert!((v - 21.0).abs() < 1e-9, "3*7 must equal 21, got {v}");
    }

    #[test]
    fn math_div_returns_correct_result() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let (v, _) = sb
            .call_f64_binary(
                MATH_EVALUATOR_WAT.as_bytes(),
                "div",
                9.0,
                3.0,
                &SandboxConfig::default(),
            )
            .unwrap();
        assert!((v - 3.0).abs() < 1e-9, "9/3 must equal 3, got {v}");
    }

    // ── ToolDriver integration ────────────────────────────────────────────────

    #[test]
    fn sandboxed_math_evaluator_add_via_tool_driver() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let tool = SandboxedMathEvaluator::new(sb);
        let out = tool.invoke(br#"{"op":"add","a":5.0,"b":3.0}"#).unwrap();
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains('8'), "expected result 8 in '{s}'");
    }

    #[test]
    fn sandboxed_math_evaluator_unknown_op_returns_error() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let tool = SandboxedMathEvaluator::new(sb);
        assert!(
            tool.invoke(br#"{"op":"pow","a":2.0,"b":8.0}"#).is_err(),
            "unknown op must return an error"
        );
    }

    #[test]
    fn sandboxed_math_evaluator_non_utf8_returns_invalid_payload() {
        let sb = Arc::new(WasmSandbox::new().unwrap());
        let tool = SandboxedMathEvaluator::new(sb);
        assert_eq!(
            tool.invoke(&[0xFF, 0xFE]).unwrap_err(),
            ToolInvocationError::InvalidPayload
        );
    }

    // ── Adversarial: infinite loop — fuel exhaustion (exit criterion 1) ───────

    /// An infinite-loop module is killed by the fuel budget.
    #[test]
    fn adversarial_infinite_loop_is_bounded_by_fuel() {
        let sb = WasmSandbox::new().unwrap();
        let cfg = SandboxConfig {
            fuel_limit: 10_000,
            ..SandboxConfig::default()
        };
        let err = sb
            .run_nullary(ADVERSARIAL_LOOP_WAT.as_bytes(), "spin", &cfg)
            .unwrap_err();
        assert_eq!(
            err,
            SandboxError::FuelExhausted,
            "expected FuelExhausted, got {err:?}"
        );
    }

    // ── Adversarial: memory exhaustion (exit criterion 1) ────────────────────

    /// A module that attempts massive heap growth is bounded by the memory cap.
    #[test]
    fn adversarial_memory_exhaustion_is_bounded_by_limit() {
        let sb = WasmSandbox::new().unwrap();
        let cfg = SandboxConfig {
            fuel_limit: 1_000_000,
            memory_limit_bytes: 64 * 1024, // 1 page — growth attempt denied
            ..SandboxConfig::default()
        };
        let err = sb
            .run_nullary(ADVERSARIAL_MEMORY_WAT.as_bytes(), "grow_mem", &cfg)
            .unwrap_err();
        assert_eq!(
            err,
            SandboxError::MemoryExhausted,
            "expected MemoryExhausted, got {err:?}"
        );
    }

    // ── Capability gating (S2.5.3) ────────────────────────────────────────────

    const STDOUT_MODULE_WAT: &str = r#"(module
      (import "env" "write_stdout" (func $ws (param i32 i32)))
      (func $run (export "run")
        i32.const 0
        i32.const 5
        call $ws))"#;

    /// A module that imports `env::write_stdout` fails at link time when the
    /// capability is not granted.
    #[test]
    fn missing_capability_blocks_instantiation() {
        let sb = WasmSandbox::new().unwrap();
        let cfg = SandboxConfig {
            capabilities: SandboxCapabilities {
                allow_stdout: false,
                allow_stderr: false,
            },
            ..SandboxConfig::default()
        };
        let err = sb
            .run_nullary(STDOUT_MODULE_WAT.as_bytes(), "run", &cfg)
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::Trap(_)),
            "expected Trap (link failure), got {err:?}"
        );
    }

    /// A module that imports `env::write_stdout` succeeds when the capability is granted.
    #[test]
    fn granted_capability_allows_instantiation() {
        let sb = WasmSandbox::new().unwrap();
        let cfg = SandboxConfig {
            capabilities: SandboxCapabilities {
                allow_stdout: true,
                allow_stderr: false,
            },
            ..SandboxConfig::default()
        };
        let result = sb.run_nullary(STDOUT_MODULE_WAT.as_bytes(), "run", &cfg);
        assert!(
            result.is_ok(),
            "expected Ok with stdout capability granted, got {result:?}"
        );
    }

    // ── Fuel accounting ───────────────────────────────────────────────────────

    #[test]
    fn fuel_consumed_is_positive_for_arithmetic() {
        let sb = WasmSandbox::new().unwrap();
        let (_, r) = sb
            .call_f64_binary(
                MATH_EVALUATOR_WAT.as_bytes(),
                "add",
                1.0,
                2.0,
                &SandboxConfig::default(),
            )
            .unwrap();
        assert!(
            r.fuel_consumed > 0,
            "arithmetic must consume at least 1 fuel unit"
        );
    }

    #[test]
    fn simple_arithmetic_does_not_exhaust_generous_fuel_budget() {
        let sb = WasmSandbox::new().unwrap();
        let cfg = SandboxConfig {
            fuel_limit: 1_000_000,
            ..SandboxConfig::default()
        };
        let result = sb.call_f64_binary(MATH_EVALUATOR_WAT.as_bytes(), "add", 1.0, 2.0, &cfg);
        assert!(
            result.is_ok(),
            "simple arithmetic must not exhaust a 1M fuel budget"
        );
    }

    // ── JSON field parsers ────────────────────────────────────────────────────

    #[test]
    fn extract_str_field_finds_op_field() {
        let json = r#"{"op":"add","a":1.0,"b":2.0}"#;
        assert_eq!(extract_str_field(json, "op"), Some("add"));
    }

    #[test]
    fn extract_f64_field_finds_a_field() {
        let json = r#"{"op":"add","a":3.5,"b":2.0}"#;
        let v = extract_f64_field(json, "a").unwrap();
        assert!((v - 3.5).abs() < 1e-9);
    }

    #[test]
    fn extract_f64_field_returns_none_for_missing_field() {
        let json = r#"{"op":"add","b":2.0}"#;
        assert!(extract_f64_field(json, "a").is_none());
    }
}

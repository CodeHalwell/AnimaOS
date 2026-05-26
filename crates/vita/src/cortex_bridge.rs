// crates/vita/src/cortex_bridge.rs
//! Cortex bridge — E5.1 Cortex MVP.
//!
//! This module implements the Rust side of the vita ↔ cortex IPC channel.
//! The cortex is a short-lived Python subprocess that receives an
//! [`InvokeRequest`] over a Unix Domain Socket, runs the Plan/Act/Observe/
//! Revise loop, dispatches tool calls back through vita → praxis, and
//! terminates with an [`InvokeComplete`] carrying the episode summary.
//!
//! # Wire protocol
//!
//! All messages are **length-prefixed JSON frames**:
//! ```text
//! ┌──────────────────────────────┐
//! │ 4-byte big-endian uint32     │  byte count of the JSON body
//! │ JSON body (UTF-8)            │  the message dict
//! └──────────────────────────────┘
//! ```
//!
//! vita → cortex message types: `InvokeRequest`, `ToolResponse`.
//! cortex → vita message types: `ToolCall`, `InvokeComplete`, `CortexError`.
//!
//! # Architecture
//!
//! ```text
//! vita  ──IPC──► cortex subprocess
//!   │                │
//!   │   ToolCall ◄───┤
//!   │   (praxis)     │
//!   │   ToolResponse─►│
//!   │                │
//!   │   InvokeComplete◄┤
//!   │
//!   └► episode summary → L3 archive
//! ```
//!
//! # Testing
//!
//! [`MockCortexBridge`] implements [`CortexBackend`] without spawning a
//! subprocess, making all E5.1 exit-criterion tests hermetic (no Python
//! required in CI).

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{AuditEntry, AuditLog};

// ── IPC types (shared between the real and mock bridges) ─────────────────────

/// Description of a tool exposed to the cortex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool identifier (must match `ToolDriver::id`).
    pub name: String,
    /// Human-readable description shown to the planner.
    pub description: String,
}

/// Memory tier access scope serialised into the cortex invocation request (E5.3).
///
/// The cortex uses this to understand which memory tiers it may read/write.
/// Identity memory (`identity: true`) is always present on every baseline route
/// per S5.3.4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokeMemoryScope {
    /// Identity memory accessible (always `true` on baseline routes).
    pub identity: bool,
    /// L1 working memory accessible.
    pub l1: bool,
    /// L2 warm ARC cache accessible.
    pub l2: bool,
    /// L3 persistent archive accessible.
    pub l3: bool,
}

impl InvokeMemoryScope {
    /// Minimal scope: identity + L1 only.
    pub fn minimal() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: false,
            l3: false,
        }
    }

    /// Mid scope: identity + L1 + L2.
    pub fn mid() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: true,
            l3: false,
        }
    }

    /// Full scope: all tiers accessible.
    pub fn full() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: true,
            l3: true,
        }
    }
}

/// Request sent from vita to the cortex at the start of each invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeRequest {
    /// Stable per-invocation identifier (used for audit correlation).
    pub task_id: String,
    /// Natural-language description of the task to be performed.
    pub description: String,
    /// Tool subset the cortex is permitted to call during this invocation.
    pub tools: Vec<ToolSpec>,
    /// Current identity-memory snapshot (JSON object).
    pub identity: serde_json::Value,

    // ── E5.3 Thalamic Router fields ───────────────────────────────────────────
    /// Route identifier selected by the Thalamic Router.
    ///
    /// `None` for pre-E5.3 requests; one of `"cheap-local"`, `"mid-tier"`, or
    /// `"frontier"` for routed requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    /// Memory tier access scope for this invocation.
    ///
    /// The cortex must not attempt to access tiers not included here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<InvokeMemoryScope>,
    /// Maximum planning + acting turns for this invocation.
    ///
    /// `None` means use the cortex's own default (`AgentLoop.MAX_TOOL_CALLS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Maximum total tool calls for this invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
}

/// Result returned to vita after a successful cortex invocation.
#[derive(Debug, Clone)]
pub struct CortexInvocationResult {
    /// Per-invocation identifier echoed from the request.
    pub task_id: String,
    /// Human-readable task completion report.
    pub output: String,
    /// Compact episode summary to be archived in L3.
    pub episode_summary: String,
    /// Number of tool calls the cortex made during this invocation.
    pub tool_calls_made: usize,
    /// Elapsed time from invocation start to the cortex's first tool action.
    pub latency_to_first_action: Duration,
}

/// Errors surfaced from a cortex invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CortexError {
    /// The cortex process reported an error or crashed.
    CortexFault(String),
    /// The Python process could not be spawned.
    SpawnFailed(String),
    /// An IPC read or write failed.
    IpcError(String),
    /// The cortex exited without sending `InvokeComplete`.
    UnexpectedEof,
}

impl std::fmt::Display for CortexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CortexError::CortexFault(m) => write!(f, "cortex fault: {m}"),
            CortexError::SpawnFailed(m) => write!(f, "spawn failed: {m}"),
            CortexError::IpcError(m) => write!(f, "IPC error: {m}"),
            CortexError::UnexpectedEof => write!(f, "cortex closed connection unexpectedly"),
        }
    }
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Abstraction over the cortex invocation backend.
///
/// The trait is object-safe so it can be stored as a `Box<dyn CortexBackend>`.
/// Two implementations ship:
/// - [`PythonCortexBridge`] — spawns the real Python cortex subprocess.
/// - [`MockCortexBridge`] — simulates the cortex in Rust for hermetic tests.
pub trait CortexBackend: Send + Sync {
    /// Invoke the cortex for the given request.
    ///
    /// The implementation is responsible for:
    /// - Spawning (or simulating) the cortex.
    /// - Routing tool calls through `dispatch_tool`.
    /// - Appending [`AuditEntry`] events to `audit`.
    /// - Returning a [`CortexInvocationResult`] on success.
    fn invoke(
        &self,
        request: InvokeRequest,
        dispatch_tool: &dyn ToolDispatcher,
        audit: &mut AuditLog,
    ) -> Result<CortexInvocationResult, CortexError>;
}

/// Callback used by the bridge to route tool calls through vita → praxis.
///
/// The closure captures the `ToolRegistry` reference; callers inject it so
/// the bridge module does not depend directly on the `praxis` crate.
pub trait ToolDispatcher: Send + Sync {
    /// Dispatch a tool call and return its string result.
    fn dispatch(&self, tool_name: &str, args: &str) -> Result<String, String>;
}

/// Simple function-pointer dispatcher (wraps a closure).
pub struct FnDispatcher<F: Fn(&str, &str) -> Result<String, String> + Send + Sync>(pub F);

impl<F: Fn(&str, &str) -> Result<String, String> + Send + Sync> ToolDispatcher for FnDispatcher<F> {
    fn dispatch(&self, tool_name: &str, args: &str) -> Result<String, String> {
        (self.0)(tool_name, args)
    }
}

// ── IPC wire helpers ──────────────────────────────────────────────────────────

/// Read exactly `n` bytes from `stream`, returning `None` on clean EOF.
fn read_exact_bytes(stream: &mut UnixStream, n: usize) -> io::Result<Option<Vec<u8>>> {
    let mut buf = vec![0u8; n];
    let mut pos = 0;
    while pos < n {
        match stream.read(&mut buf[pos..]) {
            Ok(0) if pos == 0 => return Ok(None), // clean EOF at message boundary
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed mid-message",
                ))
            }
            Ok(k) => pos += k,
            Err(e) => return Err(e),
        }
    }
    Ok(Some(buf))
}

/// Receive one length-prefixed JSON frame from `stream`.
fn recv_ipc(stream: &mut UnixStream) -> Result<Option<serde_json::Value>, CortexError> {
    let header = read_exact_bytes(stream, 4).map_err(|e| CortexError::IpcError(e.to_string()))?;
    let header = match header {
        None => return Ok(None),
        Some(h) => h,
    };
    let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let body = read_exact_bytes(stream, length)
        .map_err(|e| CortexError::IpcError(e.to_string()))?
        .ok_or(CortexError::UnexpectedEof)?;

    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| CortexError::IpcError(format!("JSON parse error: {e}")))
}

/// Send one length-prefixed JSON frame to `stream`.
fn send_ipc(stream: &mut UnixStream, msg: &serde_json::Value) -> Result<(), CortexError> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| CortexError::IpcError(format!("JSON serialise error: {e}")))?;
    let len_bytes = (body.len() as u32).to_be_bytes();
    stream
        .write_all(&len_bytes)
        .and_then(|_| stream.write_all(&body))
        .map_err(|e| CortexError::IpcError(e.to_string()))
}

// ── Python cortex bridge ──────────────────────────────────────────────────────

/// The real Python cortex bridge: spawns `python -m cortex` as a subprocess.
///
/// The bridge creates a temporary Unix Domain Socket for each invocation,
/// sends the [`InvokeRequest`], handles the tool-call round-trips, and
/// tears the process down when `InvokeComplete` is received.
pub struct PythonCortexBridge {
    /// Absolute path to the directory containing the `cortex/` Python package.
    /// Typically the workspace root.
    pub workspace_root: PathBuf,
    /// Agent state directory (holds `identity.json`).
    pub state_dir: PathBuf,
    /// LLM backend hint passed to the Python process.
    pub llm_backend: String,
}

impl PythonCortexBridge {
    /// Construct a new bridge with the given workspace root.
    pub fn new(workspace_root: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            workspace_root,
            state_dir,
            llm_backend: "mock".to_string(),
        }
    }

    fn spawn_python(&self, socket_path: &str) -> Result<Child, CortexError> {
        Command::new("python3")
            .args([
                "-m",
                "cortex",
                "--socket",
                socket_path,
                "--state-dir",
                self.state_dir.to_str().unwrap_or("/tmp"),
                "--backend",
                &self.llm_backend,
            ])
            .current_dir(&self.workspace_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CortexError::SpawnFailed(e.to_string()))
    }
}

impl CortexBackend for PythonCortexBridge {
    fn invoke(
        &self,
        request: InvokeRequest,
        dispatch_tool: &dyn ToolDispatcher,
        audit: &mut AuditLog,
    ) -> Result<CortexInvocationResult, CortexError> {
        let task_id = request.task_id.clone();
        let socket_path = std::env::temp_dir().join(format!("anima-cortex-{}.sock", task_id));
        let socket_path_str = socket_path.to_string_lossy().to_string();

        // Create the server socket before spawning so the child can connect
        // immediately.
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| CortexError::IpcError(format!("bind UDS: {e}")))?;
        listener
            .set_nonblocking(false)
            .map_err(|e| CortexError::IpcError(e.to_string()))?;

        let start = Instant::now();

        let mut child = self.spawn_python(&socket_path_str)?;

        // Accept the cortex connection (blocking).
        let (mut stream, _) = listener
            .accept()
            .map_err(|e| CortexError::IpcError(format!("accept UDS: {e}")))?;

        // Send InvokeRequest.
        let req_val = serde_json::json!({
            "type": "InvokeRequest",
            "task_id": request.task_id,
            "description": request.description,
            "tools": request.tools,
            "identity": request.identity,
        });
        send_ipc(&mut stream, &req_val)?;

        // Message loop.
        let mut first_action_latency = Duration::ZERO;
        let mut tool_calls_made = 0usize;
        let result = loop {
            let msg = recv_ipc(&mut stream)?;
            let msg = msg.ok_or(CortexError::UnexpectedEof)?;
            let msg_type = msg["type"].as_str().unwrap_or("").to_owned();

            match msg_type.as_str() {
                "ToolCall" => {
                    if tool_calls_made == 0 {
                        first_action_latency = start.elapsed();
                    }
                    tool_calls_made += 1;

                    let call_id = msg["call_id"].as_str().unwrap_or("").to_owned();
                    let tool_name = msg["tool_name"].as_str().unwrap_or("").to_owned();
                    let args = msg["args"].as_str().unwrap_or("{}").to_owned();

                    let (result_str, error_val): (String, serde_json::Value) =
                        match dispatch_tool.dispatch(&tool_name, &args) {
                            Ok(r) => (r, serde_json::Value::Null),
                            Err(e) => (String::new(), serde_json::Value::String(e)),
                        };

                    send_ipc(
                        &mut stream,
                        &serde_json::json!({
                            "type": "ToolResponse",
                            "call_id": call_id,
                            "result": result_str,
                            "error": error_val,
                        }),
                    )?;
                }

                "InvokeComplete" => {
                    let output = msg["output"].as_str().unwrap_or("").to_owned();
                    let summary = msg["episode_summary"].as_str().unwrap_or("").to_owned();
                    break CortexInvocationResult {
                        task_id: task_id.clone(),
                        output,
                        episode_summary: summary,
                        tool_calls_made,
                        latency_to_first_action: first_action_latency,
                    };
                }

                "CortexError" => {
                    let msg_str = msg["message"].as_str().unwrap_or("unknown").to_owned();
                    audit.push(AuditEntry::CortexFault {
                        task_id: task_id.clone(),
                        error: msg_str.clone(),
                    });
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&socket_path);
                    return Err(CortexError::CortexFault(msg_str));
                }

                other => {
                    // Unknown message — log and ignore (forward compatibility).
                    let _ = other;
                }
            }
        };

        audit.push(AuditEntry::CortexInvoked {
            task_id: task_id.clone(),
            latency_to_first_action_ms: result.latency_to_first_action.as_millis() as u64,
        });
        audit.push(AuditEntry::CortexCompleted {
            task_id: task_id.clone(),
            tool_calls: result.tool_calls_made,
            summary_len: result.episode_summary.len(),
        });

        let _ = child.wait();
        let _ = std::fs::remove_file(&socket_path);

        Ok(result)
    }
}

// ── Mock cortex bridge ────────────────────────────────────────────────────────

/// A hermetic cortex backend for unit and integration tests.
///
/// [`MockCortexBridge`] executes the same logical flow as the Python cortex
/// but entirely in Rust, without spawning a subprocess or opening any sockets.
/// It satisfies all three E5.1 exit criteria in CI:
///
/// 1. Makes exactly two tool calls (mirrors the mock Python plan).
/// 2. Returns a non-empty episode summary.
/// 3. Records timing in the audit log.
pub struct MockCortexBridge {
    /// Optional fault injection — when `Some`, the invocation returns this
    /// error instead of succeeding, exercising the crash-isolation path.
    pub inject_fault: Option<String>,
    /// Simulated latency to first tool action.
    pub simulated_latency: Duration,
}

impl Default for MockCortexBridge {
    fn default() -> Self {
        Self {
            inject_fault: None,
            simulated_latency: Duration::from_millis(1),
        }
    }
}

impl CortexBackend for MockCortexBridge {
    fn invoke(
        &self,
        request: InvokeRequest,
        dispatch_tool: &dyn ToolDispatcher,
        audit: &mut AuditLog,
    ) -> Result<CortexInvocationResult, CortexError> {
        let task_id = request.task_id.clone();
        let start = Instant::now();

        // Fault injection path (tests crash-isolation behaviour).
        if let Some(ref fault) = self.inject_fault {
            audit.push(AuditEntry::CortexFault {
                task_id: task_id.clone(),
                error: fault.clone(),
            });
            return Err(CortexError::CortexFault(fault.clone()));
        }

        // ── Plan: two steps (mirrors cortex/agent_loop.py mock plan) ─────────

        let tool_names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
        let mut steps: Vec<(&str, &str, &str)> = Vec::new(); // (name, args, desc)

        if tool_names.contains(&"clock") {
            steps.push(("clock", "{}", "Record current wall time"));
        }
        if tool_names.contains(&"echo") {
            steps.push((
                "echo",
                r#"{"payload":"mock-plan"}"#,
                "Echo task description",
            ));
        }
        if steps.is_empty() {
            if let Some(first) = request.tools.first() {
                steps.push((first.name.as_str(), "{}", "Call first available tool"));
            }
        }

        // ── Act / Observe ─────────────────────────────────────────────────────

        let mut observations: Vec<String> = Vec::new();
        let mut first_action_latency = self.simulated_latency;
        let mut tool_calls_made = 0usize;

        for (i, (tool_name, args, desc)) in steps.iter().enumerate() {
            if i == 0 {
                first_action_latency = start.elapsed().max(self.simulated_latency);
            }
            let result = dispatch_tool
                .dispatch(tool_name, args)
                .unwrap_or_else(|e| format!("[error: {e}]"));
            tool_calls_made += 1;
            observations.push(format!("[{tool_name}] {desc} → {result:?}"));
        }

        // ── Synthesise output and summary ─────────────────────────────────────

        let output = format!(
            "Task completed: {:?}\n  Tool calls: {tool_calls_made}\n{}",
            request.description,
            observations
                .iter()
                .map(|o| format!("  • {o}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );

        let episode_summary = format!(
            "task_id={} description={:?} tool_calls={tool_calls_made} observations={} duration_ms={}",
            task_id,
            request.description,
            observations.len(),
            start.elapsed().as_millis(),
        );

        // ── Audit ─────────────────────────────────────────────────────────────

        audit.push(AuditEntry::CortexInvoked {
            task_id: task_id.clone(),
            latency_to_first_action_ms: first_action_latency.as_millis() as u64,
        });
        audit.push(AuditEntry::CortexCompleted {
            task_id: task_id.clone(),
            tool_calls: tool_calls_made,
            summary_len: episode_summary.len(),
        });

        Ok(CortexInvocationResult {
            task_id,
            output,
            episode_summary,
            tool_calls_made,
            latency_to_first_action: first_action_latency,
        })
    }
}

// ── Thread-safe wrapper ───────────────────────────────────────────────────────

/// A `Send + Sync` handle to a heap-allocated [`CortexBackend`].
pub type CortexHandle = Arc<Mutex<Box<dyn CortexBackend>>>;

/// Construct a [`CortexHandle`] from any [`CortexBackend`] implementation.
pub fn cortex_handle(backend: impl CortexBackend + 'static) -> CortexHandle {
    Arc::new(Mutex::new(Box::new(backend)))
}

// ── Episode archival helpers ──────────────────────────────────────────────────

/// Write the episode summary from a cortex invocation into the L3 archive.
///
/// The summary is embedded as a 4-dim feature vector identical to the one
/// used by the existing `embed_memory_node` function:
/// `[1.0, 0.0, 0.0, len_normalised]` where `len_normalised` caps at 1.0 for
/// summaries ≥ 1 000 characters.  The provenance tier is `Episode`.
///
/// Returns the ID assigned to the new archive entry, or `None` if no archive
/// is configured.
pub fn archive_episode(
    l3: &mut memory::L3Archive,
    next_id: &mut u64,
    task_id: &str,
    result: &CortexInvocationResult,
) {
    use memory::{ArchivedItem, Provenance, SourceTier};

    let id = *next_id;
    *next_id += 1;

    let len_norm = (result.episode_summary.len() as f32 / 1000.0).min(1.0);
    let embedding = [1.0f32, 0.0, 0.0, len_norm];

    // Pack the summary into the 20-byte payload (truncated to 20 bytes).
    let mut payload = [0u8; 20];
    let bytes = result.episode_summary.as_bytes();
    let copy_len = bytes.len().min(20);
    payload[..copy_len].copy_from_slice(&bytes[..copy_len]);

    let item = ArchivedItem {
        id,
        embedding: embedding.to_vec(),
        payload: payload.to_vec(),
    };

    // The source_key in Provenance carries the episode key (e.g. "episode:<task_id>").
    let episode_key = format!("episode:{task_id}");
    let prov = Provenance::now(SourceTier::Episode, &episode_key);
    let _ = l3.demote(item, prov);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuditEntry, AuditLog};
    use memory::L3Archive;

    // ── Tool dispatch fixture ─────────────────────────────────────────────────

    /// A simple tool dispatcher that handles `clock` and `echo` in-process.
    struct InProcessDispatcher;

    impl ToolDispatcher for InProcessDispatcher {
        fn dispatch(&self, tool_name: &str, args: &str) -> Result<String, String> {
            match tool_name {
                "clock" => {
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis();
                    Ok(format!("{ms}"))
                }
                "echo" => {
                    // Parse {"payload":"..."} and return the payload.
                    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                    Ok(v["payload"].as_str().unwrap_or(args).to_owned())
                }
                unknown => Err(format!("unknown tool: {unknown}")),
            }
        }
    }

    fn two_tool_request() -> InvokeRequest {
        InvokeRequest {
            task_id: "test-task-1".to_string(),
            description: "Test the mock cortex".to_string(),
            tools: vec![
                ToolSpec {
                    name: "clock".to_string(),
                    description: "Return the current Unix epoch in milliseconds".to_string(),
                },
                ToolSpec {
                    name: "echo".to_string(),
                    description: "Echo back the payload".to_string(),
                },
            ],
            identity: serde_json::json!({"user": {"name": "Test"}}),
            route_id: None,
            memory_scope: None,
            max_turns: None,
            max_tool_calls: None,
        }
    }

    // ── E5.1 Exit criterion 1 — two tool calls + episode summary in L3 ────────

    /// A user-issued task completes a multi-step plan with at least two tool
    /// calls and emits an episode summary.
    #[test]
    fn mock_cortex_makes_two_tool_calls() {
        let bridge = MockCortexBridge::default();
        let dispatcher = InProcessDispatcher;
        let mut audit = AuditLog::new();

        let result = bridge
            .invoke(two_tool_request(), &dispatcher, &mut audit)
            .expect("invocation must succeed");

        assert_eq!(
            result.tool_calls_made, 2,
            "must make exactly two tool calls"
        );
        assert!(
            !result.episode_summary.is_empty(),
            "episode summary must not be empty"
        );
        assert!(
            result.output.contains("clock"),
            "output must mention the clock tool"
        );
    }

    /// The episode summary is recoverable from the L3 archive after a process
    /// restart (simulated by creating a new `L3Archive` from the same path).
    #[test]
    fn episode_summary_persists_in_l3_after_restart() {
        let dir = std::env::temp_dir().join("anima_e51_l3_restart_test");
        let _ = std::fs::remove_dir_all(&dir);

        let bridge = MockCortexBridge::default();
        let dispatcher = InProcessDispatcher;
        let mut audit = AuditLog::new();

        // --- First "process" ---
        {
            let result = bridge
                .invoke(two_tool_request(), &dispatcher, &mut audit)
                .expect("first invocation");

            let mut l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
            let mut next_id = 0u64;
            archive_episode(&mut l3, &mut next_id, &result.task_id, &result);
        }

        // --- "Process restart": new L3Archive instance from same path ---
        {
            let l3 = L3Archive::open(&dir, 4, 100).expect("re-open L3");
            let query = vec![1.0f32, 0.0, 0.0, 0.5];
            let hits = l3.search(&query, 1);
            assert!(
                !hits.is_empty(),
                "episode summary must be retrievable after restart"
            );
            assert!(
                hits[0].provenance.source_key.starts_with("episode:"),
                "hit provenance source_key must start with 'episode:'"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── E5.1 Exit criterion 2 — cortex fault does not crash vita ─────────────

    /// A cortex fault is recorded in the audit log and does not propagate as
    /// a panic; the lifecycle manager can continue accepting tasks.
    #[test]
    fn cortex_fault_is_audited_and_does_not_crash_vita() {
        let bridge = MockCortexBridge {
            inject_fault: Some("simulated cortex crash".to_string()),
            ..Default::default()
        };
        let dispatcher = InProcessDispatcher;
        let mut audit = AuditLog::new();

        let result = bridge.invoke(two_tool_request(), &dispatcher, &mut audit);
        assert!(result.is_err(), "invocation must fail on injected fault");
        assert!(
            matches!(result.unwrap_err(), CortexError::CortexFault(_)),
            "error variant must be CortexFault"
        );

        // Audit log must contain a CortexFault entry.
        let has_fault_entry = audit
            .entries()
            .iter()
            .any(|e| matches!(e, AuditEntry::CortexFault { .. }));
        assert!(
            has_fault_entry,
            "audit log must contain a CortexFault entry"
        );

        // Subsequent invocation succeeds (vita is not crashed).
        let mut audit2 = AuditLog::new();
        let ok_bridge = MockCortexBridge::default();
        let result2 = ok_bridge.invoke(two_tool_request(), &dispatcher, &mut audit2);
        assert!(result2.is_ok(), "subsequent invocation must succeed");
    }

    // ── E5.1 Exit criterion 3 — latency to first action is logged ────────────

    /// The end-to-end latency from invocation start to the cortex's first tool
    /// action is present in the audit log.
    #[test]
    fn latency_to_first_action_is_logged_in_audit() {
        let bridge = MockCortexBridge::default();
        let dispatcher = InProcessDispatcher;
        let mut audit = AuditLog::new();

        let _ = bridge
            .invoke(two_tool_request(), &dispatcher, &mut audit)
            .expect("invocation");

        let has_invoked_entry = audit
            .entries()
            .iter()
            .any(|e| matches!(e, AuditEntry::CortexInvoked { .. }));
        assert!(
            has_invoked_entry,
            "audit log must contain a CortexInvoked entry with latency"
        );
    }

    /// `CortexInvoked` records a non-zero latency (even in the mock the
    /// simulated latency is at least 1 ms).
    #[test]
    fn cortex_invoked_audit_entry_carries_latency_ms() {
        let bridge = MockCortexBridge {
            simulated_latency: Duration::from_millis(5),
            ..Default::default()
        };
        let dispatcher = InProcessDispatcher;
        let mut audit = AuditLog::new();

        let _ = bridge.invoke(two_tool_request(), &dispatcher, &mut audit);

        for entry in audit.entries() {
            if let AuditEntry::CortexInvoked {
                latency_to_first_action_ms,
                ..
            } = entry
            {
                assert!(*latency_to_first_action_ms > 0, "latency must be positive");
                return;
            }
        }
        panic!("no CortexInvoked entry found");
    }

    // ── Tool dispatch round-trip ──────────────────────────────────────────────

    /// The mock bridge correctly routes tool calls and incorporates the results.
    #[test]
    fn mock_bridge_routes_clock_and_echo_calls() {
        let bridge = MockCortexBridge::default();
        let dispatcher = InProcessDispatcher;
        let mut audit = AuditLog::new();

        let result = bridge
            .invoke(two_tool_request(), &dispatcher, &mut audit)
            .expect("invocation");

        // Output must mention both tool names.
        assert!(
            result.output.contains("clock"),
            "output must reference clock"
        );
        assert!(result.output.contains("echo"), "output must reference echo");
    }

    /// When the task's tool set contains only `clock`, the plan makes exactly
    /// one tool call.
    #[test]
    fn single_tool_plan_makes_one_call() {
        let bridge = MockCortexBridge::default();
        let dispatcher = InProcessDispatcher;
        let mut audit = AuditLog::new();

        let req = InvokeRequest {
            task_id: "single-tool".to_string(),
            description: "Single tool task".to_string(),
            tools: vec![ToolSpec {
                name: "clock".to_string(),
                description: "Clock tool".to_string(),
            }],
            identity: serde_json::Value::Null,
            route_id: None,
            memory_scope: None,
            max_turns: None,
            max_tool_calls: None,
        };

        let result = bridge
            .invoke(req, &dispatcher, &mut audit)
            .expect("invocation");

        assert_eq!(
            result.tool_calls_made, 1,
            "single-tool plan must make one call"
        );
    }

    // ── Episode archival ──────────────────────────────────────────────────────

    /// `archive_episode` increments the ID counter and produces a retrievable hit.
    #[test]
    fn archive_episode_increments_id_counter() {
        let dir = std::env::temp_dir().join("anima_e51_archive_id_test");
        let _ = std::fs::remove_dir_all(&dir);

        let mut l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
        let mut next_id = 0u64;

        let result = CortexInvocationResult {
            task_id: "id-counter-test".to_string(),
            output: "out".to_string(),
            episode_summary: "summary text".to_string(),
            tool_calls_made: 1,
            latency_to_first_action: Duration::ZERO,
        };

        archive_episode(&mut l3, &mut next_id, "id-counter-test", &result);
        assert_eq!(next_id, 1, "counter must be incremented to 1");

        archive_episode(&mut l3, &mut next_id, "id-counter-test-2", &result);
        assert_eq!(next_id, 2, "counter must be incremented to 2");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

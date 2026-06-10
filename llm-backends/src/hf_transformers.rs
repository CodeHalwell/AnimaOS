//! Hugging Face `transformers` sidecar backend (E8 S8.2.2).
//!
//! [`HfTransformersBackend`] runs HF model inference through a Python
//! `transformers` worker subprocess that speaks the same
//! **4-byte big-endian length-prefix + JSON body** wire protocol as the
//! cortex IPC bridge.  This allows operators to run any HF model without a
//! separate HTTP server.
//!
//! # Modes
//!
//! | Mode    | Activated by                          | Network? | Python? |
//! |---------|---------------------------------------|----------|---------|
//! | Fixture | default (always available)            | no       | no      |
//! | Live    | `ANIMA_HF_TRANSFORMERS_LIVE=1`        | no       | **yes** |
//!
//! In **fixture mode** (default, CI-safe) the backend replays a pre-recorded
//! token stream without spawning any subprocess.  In **live mode** it spawns
//! `cortex/transformers_worker.py` on a temporary Unix Domain Socket and
//! streams tokens back through the worker process.
//!
//! # Adding to the factory
//!
//! The backend is registered as [`crate::factory::BackendKind::HfTransformers`]
//! and constructed via [`crate::factory::BackendFactory::fixture`].
//!
//! # Live-mode prerequisites
//!
//! ```text
//! pip install transformers accelerate torch
//! export ANIMA_HF_TRANSFORMERS_LIVE=1
//! export ANIMA_HF_MODEL=microsoft/Phi-3.5-mini-instruct
//! cargo run --bin anima-hosted
//! ```

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

// ── Wire-protocol helpers ─────────────────────────────────────────────────────

/// Maximum frame body accepted from the worker (guards against corrupt length fields).
const MAX_FRAME_LEN: usize = 16 * 1024 * 1024; // 16 MiB

/// Overall deadline for the worker to connect to the listener after spawning.
/// A worker that hangs without exiting (e.g. stuck loading a model) is detected
/// here rather than pinning the calling task forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-read timeout on the connected stream so the cancellation check between
/// frames can fire even when the worker stalls mid-stream.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of a non-blocking frame read.
enum FrameRead {
    /// A complete frame was decoded.
    Frame(serde_json::Value),
    /// No complete frame is available yet and the read timed out — the caller
    /// should re-check cancellation and retry. Any bytes already received are
    /// retained in the reader's buffer.
    Timeout,
    /// The peer closed the connection cleanly.
    Eof,
}

/// Incremental, timeout-tolerant frame reader.
///
/// Using `read_exact` directly with a socket read timeout is unsafe: a timeout
/// mid-header or mid-body consumes bytes that are then lost on retry, which
/// desyncs the length-prefix framing. This reader accumulates whatever bytes
/// arrive into an internal buffer and only decodes a frame once the full
/// `4-byte length + body` is present, so a timeout never discards progress.
struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Removes and returns one complete frame from the buffer, if present.
    fn take_frame(&mut self) -> std::io::Result<Option<serde_json::Value>> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("worker frame too large: {len} bytes (max {MAX_FRAME_LEN})"),
            ));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let body = self.buf[4..4 + len].to_vec();
        self.buf.drain(0..4 + len);
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    fn read_frame(&mut self, r: &mut impl Read) -> std::io::Result<FrameRead> {
        loop {
            if let Some(frame) = self.take_frame()? {
                return Ok(FrameRead::Frame(frame));
            }
            let mut chunk = [0u8; 8192];
            match r.read(&mut chunk) {
                Ok(0) => return Ok(FrameRead::Eof),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Ok(FrameRead::Timeout);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

fn write_frame(w: &mut impl Write, value: &serde_json::Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = (body.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(&body)?;
    w.flush()
}

// ── Fixture data ──────────────────────────────────────────────────────────────

static FIXTURE_TOKENS: &[&str] = &[
    "This",
    " is",
    " a",
    " fixture",
    " response",
    " from",
    " the",
    " HF",
    " Transformers",
    " backend",
    ".",
    " In",
    " live",
    " mode",
    " the",
    " configured",
    " model",
    " would",
    " generate",
    " tokens",
    " here",
    ".",
];

// ── Backend modes ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum TransformersMode {
    Fixture,
    Live,
}

// ── Main struct ───────────────────────────────────────────────────────────────

/// An [`LlmBackend`] that runs inference through a Python `transformers` worker.
///
/// **Fixture mode is the default** — produces a reproducible token stream
/// without spawning any subprocess.  Set `ANIMA_HF_TRANSFORMERS_LIVE=1`
/// and `ANIMA_HF_MODEL=<model_id>` to enable the live subprocess path.
pub struct HfTransformersBackend {
    mode: TransformersMode,
    /// Model ID stored for `model_id()` reporting and live invocation.
    live_model_id: String,
}

impl HfTransformersBackend {
    /// Creates a fixture-mode backend (default, CI-safe).
    pub fn new() -> Self {
        Self {
            mode: TransformersMode::Fixture,
            live_model_id: String::new(),
        }
    }

    /// Creates a live backend that will load `model_id` via `transformers`.
    pub fn live(model_id: impl Into<String>) -> Self {
        Self {
            mode: TransformersMode::Live,
            live_model_id: model_id.into(),
        }
    }

    /// Selects mode from environment variables.
    ///
    /// Uses live mode when `ANIMA_HF_TRANSFORMERS_LIVE=1`; otherwise fixture
    /// mode.  The model ID comes from `ANIMA_HF_MODEL`.
    pub fn from_env() -> Self {
        if std::env::var("ANIMA_HF_TRANSFORMERS_LIVE")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            let model_id = std::env::var("ANIMA_HF_MODEL")
                .unwrap_or_else(|_| "microsoft/Phi-3.5-mini-instruct".to_string());
            Self::live(model_id)
        } else {
            Self::new()
        }
    }

    // ── Private: live subprocess call ─────────────────────────────────────────

    fn run_live_completion(
        model_id: &str,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<StreamingCompletion>, LlmBackendError> {
        let worker_path = locate_worker_script().ok_or_else(|| {
            LlmBackendError::Provider(
                "transformers_worker.py not found; \
                 set ANIMA_TRANSFORMERS_WORKER_PATH or run from the repo root"
                    .to_string(),
            )
        })?;

        // Unique per-call socket path avoids collisions when the backend is
        // shared across concurrent scheduler tasks (Arc<dyn LlmBackend>).
        static CALL_COUNTER: AtomicU64 = AtomicU64::new(0);
        let call_id = CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket_path =
            std::env::temp_dir().join(format!("anima-hf-{}-{}.sock", std::process::id(), call_id));
        if socket_path.exists() {
            std::fs::remove_file(&socket_path).ok();
        }

        let listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .map_err(|e| LlmBackendError::Provider(e.to_string()))?;

        // stdout → /dev/null avoids filling the OS pipe buffer (64 KB) and
        // deadlocking when output is never read.  stderr is inherited so that
        // worker startup errors remain visible on the host terminal.
        let mut child = std::process::Command::new("python3")
            .arg(&worker_path)
            .arg("--socket")
            .arg(&socket_path)
            .arg("--model")
            .arg(model_id)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| {
                LlmBackendError::Provider(format!("failed to spawn transformers worker: {e}"))
            })?;

        // Non-blocking accept with child-exit detection: if the Python worker
        // crashes before connecting (bad environment, missing deps, etc.) the
        // loop returns an error instead of hanging forever.  An overall connect
        // deadline also bounds the wait so a worker that hangs *without* exiting
        // (e.g. wedged loading a model) is detected too.
        listener
            .set_nonblocking(true)
            .map_err(|e| LlmBackendError::Provider(e.to_string()))?;
        let connect_deadline = std::time::Instant::now() + CONNECT_TIMEOUT;
        let (mut stream, _addr) = loop {
            match listener.accept() {
                Ok(res) => break res,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            std::fs::remove_file(&socket_path).ok();
                            return Err(LlmBackendError::Provider(format!(
                                "transformers worker exited before connecting (status: {status})"
                            )));
                        }
                        Ok(None) => {
                            if std::time::Instant::now() >= connect_deadline {
                                child.kill().ok();
                                child.wait().ok();
                                std::fs::remove_file(&socket_path).ok();
                                return Err(LlmBackendError::Provider(format!(
                                    "transformers worker did not connect within {:?}",
                                    CONNECT_TIMEOUT
                                )));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50))
                        }
                        Err(e) => {
                            child.kill().ok();
                            std::fs::remove_file(&socket_path).ok();
                            return Err(LlmBackendError::Provider(e.to_string()));
                        }
                    }
                }
                Err(e) => {
                    child.kill().ok();
                    std::fs::remove_file(&socket_path).ok();
                    return Err(LlmBackendError::Provider(e.to_string()));
                }
            }
        };
        stream
            .set_nonblocking(false)
            .map_err(|e| LlmBackendError::Provider(e.to_string()))?;
        // Read timeout so the cancellation check between frames can fire even if
        // the worker stalls mid-stream (a blocked read would otherwise pin the
        // task forever).  A timed-out read surfaces as WouldBlock/TimedOut, which
        // we treat as "retry after re-checking cancellation".
        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_err(|e| LlmBackendError::Provider(e.to_string()))?;

        let request = serde_json::json!({
            "type": "infer",
            "prompt": prompt,
            "max_new_tokens": 256,
        });
        write_frame(&mut stream, &request).map_err(|e| LlmBackendError::Provider(e.to_string()))?;

        let mut completions = Vec::new();
        let mut reader = FrameReader::new();
        loop {
            if cancel.is_cancelled() {
                child.kill().ok();
                child.wait().ok();
                std::fs::remove_file(&socket_path).ok();
                return Err(LlmBackendError::Cancelled);
            }
            match reader.read_frame(&mut stream) {
                Ok(FrameRead::Frame(frame)) => {
                    let frame_type = frame.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match frame_type {
                        "token" => {
                            if let Some(tok) = frame.get("text").and_then(|t| t.as_str()) {
                                completions.push(StreamingCompletion::Token(tok.to_string()));
                            }
                        }
                        "done" => {
                            completions.push(StreamingCompletion::Done);
                            break;
                        }
                        "error" => {
                            let msg = frame
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("unknown worker error");
                            child.kill().ok();
                            child.wait().ok();
                            std::fs::remove_file(&socket_path).ok();
                            return Err(LlmBackendError::Provider(msg.to_string()));
                        }
                        _ => {}
                    }
                }
                // Clean EOF: the worker closed after streaming.
                Ok(FrameRead::Eof) => {
                    completions.push(StreamingCompletion::Done);
                    break;
                }
                // Read timed out: no full frame yet but the stream is still
                // alive and partial bytes are preserved. Loop back so the
                // cancellation check above can fire.
                Ok(FrameRead::Timeout) => continue,
                Err(e) => {
                    child.kill().ok();
                    child.wait().ok();
                    std::fs::remove_file(&socket_path).ok();
                    return Err(LlmBackendError::Provider(e.to_string()));
                }
            }
        }

        child.wait().ok();
        std::fs::remove_file(&socket_path).ok();
        Ok(completions)
    }
}

impl Default for HfTransformersBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ── LlmBackend impl ───────────────────────────────────────────────────────────

impl LlmBackend for HfTransformersBackend {
    fn id(&self) -> &'static str {
        "hf-transformers"
    }

    fn model_id(&self) -> &str {
        if self.live_model_id.is_empty() {
            "fixture/hf-transformers"
        } else {
            &self.live_model_id
        }
    }

    fn max_context_tokens(&self) -> u32 {
        131_072
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        match self.mode {
            TransformersMode::Fixture => {
                if cancel.is_cancelled() {
                    return Box::pin(async { Err(LlmBackendError::Cancelled) });
                }
                let tokens: Vec<StreamingCompletion> = FIXTURE_TOKENS
                    .iter()
                    .map(|t| StreamingCompletion::Token(t.to_string()))
                    .chain(std::iter::once(StreamingCompletion::Done))
                    .collect();
                Box::pin(async move { Ok(tokens) })
            }
            TransformersMode::Live => {
                let model_id = self.live_model_id.clone();
                Box::pin(async move { Self::run_live_completion(&model_id, prompt, cancel) })
            }
        }
    }
}

// ── Worker script locator ─────────────────────────────────────────────────────

fn locate_worker_script() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("ANIMA_TRANSFORMERS_WORKER_PATH") {
        let p = std::path::PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("cortex").join("transformers_worker.py");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    let mut dir = std::env::current_dir().ok()?;
    for _ in 0..8 {
        let candidate = dir.join("cortex").join("transformers_worker.py");
        if candidate.exists() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }

    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match Pin::as_mut(&mut future).poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn fixture_mode_id_is_hf_transformers() {
        let b = HfTransformersBackend::new();
        assert_eq!(b.id(), "hf-transformers");
    }

    #[test]
    fn fixture_mode_model_id_is_fixture_path() {
        let b = HfTransformersBackend::new();
        assert!(b.model_id().contains("fixture"));
    }

    #[test]
    fn fixture_mode_reports_nonzero_max_context() {
        let b = HfTransformersBackend::new();
        assert!(b.max_context_tokens() > 0);
    }

    #[test]
    fn fixture_mode_stream_yields_tokens_then_done() {
        let b = HfTransformersBackend::new();
        let cancel = CancellationToken::new();
        let events = block_on(b.stream_completion("test prompt", &cancel)).unwrap();

        assert!(!events.is_empty());
        let (init, last) = events.split_at(events.len() - 1);
        for ev in init {
            assert!(
                matches!(ev, StreamingCompletion::Token(_)),
                "expected Token, got {ev:?}"
            );
        }
        assert!(
            matches!(last[0], StreamingCompletion::Done),
            "last event must be Done"
        );
    }

    #[test]
    fn fixture_mode_stream_is_byte_for_byte_reproducible() {
        let b = HfTransformersBackend::new();
        let cancel = CancellationToken::new();

        let run = || {
            block_on(b.stream_completion("same-prompt", &cancel))
                .unwrap()
                .into_iter()
                .filter_map(|c| match c {
                    StreamingCompletion::Token(t) => Some(t),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(run(), run(), "fixture output must be reproducible");
    }

    #[test]
    fn cancelled_token_causes_fixture_to_return_cancelled() {
        let b = HfTransformersBackend::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = block_on(b.stream_completion("x", &cancel));
        assert!(
            matches!(result, Err(LlmBackendError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
    }

    #[test]
    fn from_env_returns_fixture_when_live_not_set() {
        std::env::remove_var("ANIMA_HF_TRANSFORMERS_LIVE");
        let b = HfTransformersBackend::from_env();
        assert_eq!(b.id(), "hf-transformers");
        assert_eq!(b.mode, TransformersMode::Fixture);
    }

    #[test]
    fn live_constructor_stores_model_id() {
        let b = HfTransformersBackend::live("my-org/my-model");
        assert_eq!(b.model_id(), "my-org/my-model");
        assert_eq!(b.mode, TransformersMode::Live);
    }

    #[test]
    fn default_is_fixture_mode() {
        let b = HfTransformersBackend::default();
        assert_eq!(b.mode, TransformersMode::Fixture);
    }
}

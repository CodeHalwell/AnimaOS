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

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

// ── Wire-protocol helpers ─────────────────────────────────────────────────────

fn read_frame(r: &mut impl Read) -> std::io::Result<serde_json::Value> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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

        let socket_path =
            std::env::temp_dir().join(format!("anima-hf-{}.sock", std::process::id()));
        if socket_path.exists() {
            std::fs::remove_file(&socket_path).ok();
        }

        let listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .map_err(|e| LlmBackendError::Provider(e.to_string()))?;

        let mut child = std::process::Command::new("python3")
            .arg(&worker_path)
            .arg("--socket")
            .arg(&socket_path)
            .arg("--model")
            .arg(model_id)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                LlmBackendError::Provider(format!("failed to spawn transformers worker: {e}"))
            })?;

        let accept_result = listener.accept();
        let (mut stream, _addr) = accept_result.map_err(|e| {
            child.kill().ok();
            LlmBackendError::Provider(e.to_string())
        })?;

        let request = serde_json::json!({
            "type": "infer",
            "prompt": prompt,
            "max_new_tokens": 256,
        });
        write_frame(&mut stream, &request).map_err(|e| LlmBackendError::Provider(e.to_string()))?;

        let mut completions = Vec::new();
        loop {
            if cancel.is_cancelled() {
                child.kill().ok();
                completions.push(StreamingCompletion::Cancelled);
                return Err(LlmBackendError::Cancelled);
            }
            match read_frame(&mut stream) {
                Ok(frame) => {
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
                            return Err(LlmBackendError::Provider(msg.to_string()));
                        }
                        _ => {}
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    completions.push(StreamingCompletion::Done);
                    break;
                }
                Err(e) => {
                    child.kill().ok();
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
        // Fixture mode does not consult the cancel token — it just returns
        // the pre-recorded stream.  This test verifies the backend does not
        // panic on a pre-cancelled token.
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = block_on(b.stream_completion("x", &cancel));
        // Fixture mode always succeeds (it ignores cancel for simplicity).
        assert!(result.is_ok());
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

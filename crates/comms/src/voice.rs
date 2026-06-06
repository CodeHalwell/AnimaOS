//! Speech-to-text and text-to-speech provider traits for E10 — Presence.
//!
//! Voice input arrives as PCM audio (already modelled by
//! `senses::SensoryPacket::Pcm`).  Before the agent can act on voice, PCM
//! frames need to be transcribed to text — that is the job of an
//! [`SttProvider`].  When the agent responds, an optional TTS stage renders
//! the text reply as audio so it can be delivered back over a voice-capable
//! channel — that is the job of a [`TtsProvider`].
//!
//! # Default provider selection
//!
//! Both traits follow the local-first, CI-hermetic discipline used throughout
//! AnimaOS: the **fixture** implementations (`FixtureStt`, `FixtureTts`) are
//! the default and require no network access or local model installation.
//! Production deployments can swap in whisper.cpp (STT) or Piper (TTS) via
//! the same trait, without changing any call sites.
//!
//! ```text
//! PCM frame  ──► SttProvider ──► transcript (text)
//! text reply ──► TtsProvider ──► PCM audio  ──► channel voice note
//! ```

use std::collections::HashMap;

// ── SttProvider ───────────────────────────────────────────────────────────────

/// Speech-to-text provider: converts a PCM audio frame to a transcript string.
///
/// # Contract
///
/// - Implementations must be `Send + Sync` (shared across threads in the
///   gateway poll loop).
/// - On error the provider returns `Err` with a human-readable message; it
///   **must not** panic.
/// - CI/default implementations must be fully hermetic (no network or model
///   file access).
pub trait SttProvider: Send + Sync {
    /// Transcribes `pcm_samples` (16-bit LE, 16 kHz mono) to text.
    ///
    /// Returns `Ok(transcript)` on success or `Err(reason)` on failure.
    fn transcribe(&self, pcm_samples: &[i16]) -> Result<String, String>;

    /// Human-readable provider identifier used in logs and audit entries.
    fn provider_id(&self) -> &str;
}

/// Fixture STT provider — returns pre-configured transcripts keyed by frame
/// length.
///
/// The lookup key is `pcm_samples.len()`.  If the length is not in the map,
/// the provider falls back to the `default_transcript` if set, or returns
/// `Err`.
///
/// # CI usage
///
/// ```
/// use comms::voice::{FixtureStt, SttProvider};
///
/// let stt = FixtureStt::new()
///     .with_transcript(3, "hello world")
///     .with_default("default transcript");
///
/// assert_eq!(stt.transcribe(&[1, 2, 3]).unwrap(), "hello world");
/// assert_eq!(stt.transcribe(&[0]).unwrap(), "default transcript");
/// ```
pub struct FixtureStt {
    transcripts: HashMap<usize, String>,
    default_transcript: Option<String>,
}

impl FixtureStt {
    /// Creates an empty fixture with no pre-configured transcripts.
    pub fn new() -> Self {
        Self {
            transcripts: HashMap::new(),
            default_transcript: None,
        }
    }

    /// Registers a transcript to return for PCM frames of exactly `len` samples.
    pub fn with_transcript(mut self, len: usize, transcript: impl Into<String>) -> Self {
        self.transcripts.insert(len, transcript.into());
        self
    }

    /// Sets the fallback transcript returned for any frame length not in the map.
    pub fn with_default(mut self, transcript: impl Into<String>) -> Self {
        self.default_transcript = Some(transcript.into());
        self
    }
}

impl Default for FixtureStt {
    fn default() -> Self {
        Self::new()
    }
}

impl SttProvider for FixtureStt {
    fn transcribe(&self, pcm_samples: &[i16]) -> Result<String, String> {
        if let Some(t) = self.transcripts.get(&pcm_samples.len()) {
            return Ok(t.clone());
        }
        if let Some(ref t) = self.default_transcript {
            return Ok(t.clone());
        }
        Err(format!(
            "FixtureStt: no transcript registered for frame length {}",
            pcm_samples.len()
        ))
    }

    fn provider_id(&self) -> &str {
        "fixture-stt"
    }
}

// ── TtsProvider ───────────────────────────────────────────────────────────────

/// Text-to-speech provider: converts a text string to a PCM audio frame.
///
/// # Contract
///
/// Same `Send + Sync` and hermetic-default discipline as [`SttProvider`].
pub trait TtsProvider: Send + Sync {
    /// Synthesises `text` into a PCM audio frame (16-bit LE, 16 kHz mono).
    ///
    /// Returns `Ok(samples)` on success or `Err(reason)` on failure.
    fn synthesise(&self, text: &str) -> Result<Vec<i16>, String>;

    /// Human-readable provider identifier used in logs and audit entries.
    fn provider_id(&self) -> &str;
}

/// Fixture TTS provider — returns pre-configured PCM frames keyed by the
/// input text string.
///
/// If the text is not in the map, the provider falls back to the
/// `default_samples` if set, or returns `Err`.
///
/// # CI usage
///
/// ```
/// use comms::voice::{FixtureTts, TtsProvider};
///
/// let tts = FixtureTts::new()
///     .with_audio("hello", vec![100i16, 200, 300])
///     .with_default(vec![0i16]);
///
/// assert_eq!(tts.synthesise("hello").unwrap(), vec![100i16, 200, 300]);
/// assert_eq!(tts.synthesise("other").unwrap(), vec![0i16]);
/// ```
pub struct FixtureTts {
    audio: HashMap<String, Vec<i16>>,
    default_samples: Option<Vec<i16>>,
}

impl FixtureTts {
    /// Creates an empty fixture with no pre-configured audio clips.
    pub fn new() -> Self {
        Self {
            audio: HashMap::new(),
            default_samples: None,
        }
    }

    /// Registers a PCM clip to return when the input text is exactly `text`.
    pub fn with_audio(mut self, text: impl Into<String>, samples: Vec<i16>) -> Self {
        self.audio.insert(text.into(), samples);
        self
    }

    /// Sets the fallback PCM clip returned for any text not in the map.
    pub fn with_default(mut self, samples: Vec<i16>) -> Self {
        self.default_samples = Some(samples);
        self
    }
}

impl Default for FixtureTts {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsProvider for FixtureTts {
    fn synthesise(&self, text: &str) -> Result<Vec<i16>, String> {
        if let Some(samples) = self.audio.get(text) {
            return Ok(samples.clone());
        }
        if let Some(ref samples) = self.default_samples {
            return Ok(samples.clone());
        }
        Err(format!(
            "FixtureTts: no audio registered for text {:?}",
            text
        ))
    }

    fn provider_id(&self) -> &str {
        "fixture-tts"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── FixtureStt ───────────────────────────────────────────────────────────

    #[test]
    fn fixture_stt_provider_id_is_fixture_stt() {
        assert_eq!(FixtureStt::new().provider_id(), "fixture-stt");
    }

    #[test]
    fn fixture_stt_returns_registered_transcript_by_frame_length() {
        let stt = FixtureStt::new().with_transcript(4, "four samples");
        assert_eq!(stt.transcribe(&[0, 1, 2, 3]).unwrap(), "four samples");
    }

    #[test]
    fn fixture_stt_falls_back_to_default_transcript_when_length_missing() {
        let stt = FixtureStt::new().with_default("default");
        assert_eq!(stt.transcribe(&[0; 99]).unwrap(), "default");
    }

    #[test]
    fn fixture_stt_returns_error_when_no_match_and_no_default() {
        let stt = FixtureStt::new();
        assert!(stt.transcribe(&[0]).is_err());
    }

    #[test]
    fn fixture_stt_specific_transcript_takes_precedence_over_default() {
        let stt = FixtureStt::new()
            .with_transcript(2, "specific")
            .with_default("default");
        assert_eq!(stt.transcribe(&[0, 1]).unwrap(), "specific");
        assert_eq!(stt.transcribe(&[0, 1, 2]).unwrap(), "default");
    }

    #[test]
    fn fixture_stt_empty_frame_uses_zero_key() {
        let stt = FixtureStt::new().with_transcript(0, "empty frame");
        assert_eq!(stt.transcribe(&[]).unwrap(), "empty frame");
    }

    // ── FixtureTts ───────────────────────────────────────────────────────────

    #[test]
    fn fixture_tts_provider_id_is_fixture_tts() {
        assert_eq!(FixtureTts::new().provider_id(), "fixture-tts");
    }

    #[test]
    fn fixture_tts_returns_registered_audio_for_text() {
        let tts = FixtureTts::new().with_audio("hello", vec![1i16, 2, 3]);
        assert_eq!(tts.synthesise("hello").unwrap(), vec![1i16, 2, 3]);
    }

    #[test]
    fn fixture_tts_falls_back_to_default_audio_when_text_missing() {
        let tts = FixtureTts::new().with_default(vec![0i16]);
        assert_eq!(tts.synthesise("anything").unwrap(), vec![0i16]);
    }

    #[test]
    fn fixture_tts_returns_error_when_no_match_and_no_default() {
        let tts = FixtureTts::new();
        assert!(tts.synthesise("unknown").is_err());
    }

    #[test]
    fn fixture_tts_specific_audio_takes_precedence_over_default() {
        let tts = FixtureTts::new()
            .with_audio("hi", vec![42i16])
            .with_default(vec![0i16]);
        assert_eq!(tts.synthesise("hi").unwrap(), vec![42i16]);
        assert_eq!(tts.synthesise("bye").unwrap(), vec![0i16]);
    }

    #[test]
    fn fixture_tts_empty_text_is_a_valid_key() {
        let tts = FixtureTts::new().with_audio("", vec![99i16]);
        assert_eq!(tts.synthesise("").unwrap(), vec![99i16]);
    }

    #[test]
    fn fixture_tts_default_returns_clone_not_reference() {
        let tts = FixtureTts::new().with_default(vec![7i16]);
        let a = tts.synthesise("x").unwrap();
        let b = tts.synthesise("y").unwrap();
        assert_eq!(a, b);
    }

    // ── Provider trait objects ────────────────────────────────────────────────

    #[test]
    fn stt_provider_trait_object_works() {
        let stt: Box<dyn SttProvider> =
            Box::new(FixtureStt::new().with_default("transcript"));
        assert_eq!(stt.transcribe(&[1, 2]).unwrap(), "transcript");
    }

    #[test]
    fn tts_provider_trait_object_works() {
        let tts: Box<dyn TtsProvider> =
            Box::new(FixtureTts::new().with_default(vec![5i16]));
        assert_eq!(tts.synthesise("hello").unwrap(), vec![5i16]);
    }
}

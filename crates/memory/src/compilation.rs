//! Policy compilation — trace-to-training-pair compiler (E3.8).
//!
//! During the `PolicyCompilation` sleep phase the agent compiles the raw
//! lifecycle audit trail into structured training datasets.  Three output
//! formats are supported so the corpus is ready for instruction-tuning,
//! conversation fine-tuning, and chain-of-thought distillation workflows.
//!
//! # Formats
//!
//! | Variant | Schema | Use-case |
//! |---------|--------|----------|
//! | [`TrainingFormat::Alpaca`] | `{ instruction, input, output }` | General instruction-following |
//! | [`TrainingFormat::Conversation`] | `{ conversations: [{ role, content }] }` | Dialogue fine-tuning |
//! | [`TrainingFormat::ChainOfThought`] | `{ prompt, chain_of_thought, answer }` | Reasoning distillation |
//!
//! # Persistence
//!
//! Compiled corpora are written as JSON files under the directory specified by
//! [`CompilationConfig::output_dir`].  The filename for each format is
//! `<format_name>.jsonl` (one JSON object per line).  A sibling `.tmp` file is
//! written first and then renamed atomically so readers never observe a
//! partial corpus.
//!
//! # Exit criteria (E3.8)
//!
//! 1. Output corpora validate against the documented schemas above.
//! 2. Emergency consolidation can trigger and recover under stress injection.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── TrainingFormat ────────────────────────────────────────────────────────────

/// Selects which output format(s) the compiler emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrainingFormat {
    /// Alpaca-style `{ instruction, input, output }` pairs.
    Alpaca,
    /// ShareGPT-style `{ conversations: [{ role, content }] }` records.
    Conversation,
    /// Chain-of-thought `{ prompt, chain_of_thought, answer }` records.
    ChainOfThought,
}

impl TrainingFormat {
    /// Stable file-system name for this format (used as the JSONL filename stem).
    pub fn filename_stem(self) -> &'static str {
        match self {
            TrainingFormat::Alpaca => "alpaca",
            TrainingFormat::Conversation => "conversation",
            TrainingFormat::ChainOfThought => "chain_of_thought",
        }
    }
}

// ── TrainingPair ──────────────────────────────────────────────────────────────

/// A single compiled training example.
///
/// The representation is format-independent; callers serialise to the desired
/// JSONL schema via the format-specific methods below.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingPair {
    /// Prompt / task description / user turn.
    pub prompt: String,
    /// Model response / assistant turn.
    pub response: String,
    /// Task tier at dispatch time (0 = High, 1 = Medium, 2 = Low).
    pub tier: u8,
    /// Task identifier from the audit trail.
    pub task_id: u64,
}

// ── Serialisable schemas ──────────────────────────────────────────────────────

/// Alpaca-style record `{ instruction, input, output }`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AlpacaRecord {
    pub instruction: String,
    pub input: String,
    pub output: String,
}

/// A single conversational turn.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
}

/// ShareGPT-style record `{ conversations: [...] }`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ConversationRecord {
    pub conversations: Vec<ConversationTurn>,
}

/// Chain-of-thought record `{ prompt, chain_of_thought, answer }`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChainOfThoughtRecord {
    pub prompt: String,
    pub chain_of_thought: String,
    pub answer: String,
}

// ── CompilationConfig ─────────────────────────────────────────────────────────

/// Configuration for the policy-compilation phase.
#[derive(Debug, Clone, PartialEq)]
pub struct CompilationConfig {
    /// Directory under which the JSONL corpus files are written.
    ///
    /// Created if it does not yet exist.
    pub output_dir: PathBuf,
    /// Formats to emit.  An empty list disables file output (useful for tests
    /// that only need the in-memory `TrainingPair` list).
    pub formats: Vec<TrainingFormat>,
    /// When `true`, the compiler appends to an existing JSONL file rather than
    /// overwriting it.  Set to `false` (default) to start fresh each cycle.
    pub append: bool,
}

impl Default for CompilationConfig {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("training_corpus"),
            formats: vec![
                TrainingFormat::Alpaca,
                TrainingFormat::Conversation,
                TrainingFormat::ChainOfThought,
            ],
            append: false,
        }
    }
}

// ── CompilationReport ─────────────────────────────────────────────────────────

/// Statistics produced by a single policy-compilation pass.
#[derive(Debug, Clone, PartialEq)]
pub struct CompilationReport {
    /// Number of raw audit entries that were processed.
    pub entries_processed: usize,
    /// Number of complete task pairs extracted (TaskStarted + TaskCompleted).
    pub pairs_compiled: usize,
    /// Number of format files successfully written.
    pub files_written: usize,
    /// Whether emergency consolidation was triggered (stress-injection path).
    pub emergency_consolidation: bool,
}

// ── AuditTraceEntry — minimal audit-entry mirror ──────────────────────────────
// Rather than depending on the vita crate's AuditEntry directly (which would
// create a circular dependency), we accept a slice of the lightweight struct
// below.  Callers (vita::sleep) translate AuditEntry → AuditTraceEntry before
// calling the compiler.

/// Minimal representation of an audit event used by the compiler.
#[derive(Debug, Clone, PartialEq)]
pub enum AuditTraceEntry {
    /// A task was dispatched to the backend.
    TaskStarted {
        task_id: u64,
        tier: u8,
        prompt: String,
    },
    /// The backend returned a successful response.
    TaskCompleted {
        task_id: u64,
        tokens_emitted: u32,
        response: String,
    },
    /// The backend returned an error.
    TaskFailed { task_id: u64, error: String },
    /// Any other event (sleep transitions, phase boundaries, etc.).
    Other,
}

// ── compile_traces_to_pairs ───────────────────────────────────────────────────

/// Compiles `entries` into [`TrainingPair`]s and optionally writes JSONL files.
///
/// # Algorithm
///
/// 1. Scan `entries` in order and match each `TaskStarted` with the first
///    subsequent `TaskCompleted` bearing the same `task_id`.
/// 2. Build a [`TrainingPair`] for each matched pair.
/// 3. For each enabled format in `config.formats`, serialise every pair to
///    the corresponding JSONL file under `config.output_dir`.
///
/// Failed tasks (`TaskFailed`) are skipped — the corpus only contains
/// successful completions.
///
/// # Errors
///
/// I/O errors during file writes are collected into the returned error list.
/// Compilation continues even when some files cannot be written; the
/// [`CompilationReport`] reports how many files were successfully written.
pub fn compile_traces_to_pairs(
    entries: &[AuditTraceEntry],
    config: &CompilationConfig,
) -> (CompilationReport, Vec<TrainingPair>, Vec<io::Error>) {
    // ── 1. Pair-extraction ────────────────────────────────────────────────────
    // Build an index of TaskStarted entries keyed by task_id.
    use std::collections::HashMap;
    let mut started: HashMap<u64, (u8, String)> = HashMap::new();
    let mut pairs: Vec<TrainingPair> = Vec::new();

    for entry in entries {
        match entry {
            AuditTraceEntry::TaskStarted {
                task_id,
                tier,
                prompt,
            } => {
                started.insert(*task_id, (*tier, prompt.clone()));
            }
            AuditTraceEntry::TaskCompleted {
                task_id, response, ..
            } => {
                if let Some((tier, prompt)) = started.remove(task_id) {
                    pairs.push(TrainingPair {
                        prompt,
                        response: response.clone(),
                        tier,
                        task_id: *task_id,
                    });
                }
            }
            _ => {}
        }
    }

    let pairs_compiled = pairs.len();
    let entries_processed = entries.len();

    // ── 2. File output ────────────────────────────────────────────────────────
    let mut files_written = 0usize;
    let mut errors: Vec<io::Error> = Vec::new();

    if !config.formats.is_empty() && !pairs.is_empty() {
        // Ensure the output directory exists.
        if let Err(e) = std::fs::create_dir_all(&config.output_dir) {
            errors.push(e);
        } else {
            for &fmt in &config.formats {
                match write_format(&config.output_dir, fmt, &pairs, config.append) {
                    Ok(()) => files_written += 1,
                    Err(e) => errors.push(e),
                }
            }
        }
    }

    let report = CompilationReport {
        entries_processed,
        pairs_compiled,
        files_written,
        emergency_consolidation: false,
    };

    (report, pairs, errors)
}

/// Emergency consolidation path — called when stress injection signals that
/// the corpus should be flushed immediately even with an incomplete pair set.
///
/// Returns `pairs_flushed` and marks `emergency_consolidation = true` in the
/// report.
pub fn emergency_consolidate(
    entries: &[AuditTraceEntry],
    config: &CompilationConfig,
) -> (CompilationReport, Vec<TrainingPair>, Vec<io::Error>) {
    let (mut report, pairs, errors) = compile_traces_to_pairs(entries, config);
    report.emergency_consolidation = true;
    (report, pairs, errors)
}

// ── Format writers ────────────────────────────────────────────────────────────

fn write_format(
    dir: &Path,
    fmt: TrainingFormat,
    pairs: &[TrainingPair],
    append: bool,
) -> io::Result<()> {
    let filename = format!("{}.jsonl", fmt.filename_stem());
    let target = dir.join(&filename);
    let tmp = dir.join(format!("{}.tmp", filename));

    // Build the JSONL content.
    let mut buf = Vec::new();
    for pair in pairs {
        let line = match fmt {
            TrainingFormat::Alpaca => {
                let record = AlpacaRecord {
                    instruction: pair.prompt.clone(),
                    input: String::new(),
                    output: pair.response.clone(),
                };
                serde_json::to_string(&record)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            }
            TrainingFormat::Conversation => {
                let record = ConversationRecord {
                    conversations: vec![
                        ConversationTurn {
                            role: "human".into(),
                            content: pair.prompt.clone(),
                        },
                        ConversationTurn {
                            role: "gpt".into(),
                            content: pair.response.clone(),
                        },
                    ],
                };
                serde_json::to_string(&record)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            }
            TrainingFormat::ChainOfThought => {
                // Split the response into a reasoning prefix and a final answer.
                // Convention: the last sentence (after the final ". " or "\n") is
                // the answer; everything before is treated as the chain-of-thought.
                let (cot, answer) = split_chain_of_thought(&pair.response);
                let record = ChainOfThoughtRecord {
                    prompt: pair.prompt.clone(),
                    chain_of_thought: cot,
                    answer,
                };
                serde_json::to_string(&record)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            }
        };
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
    }

    if append && target.exists() {
        // Append mode: read existing content, merge, write atomically.
        let existing = std::fs::read(&target)?;
        let mut merged = existing;
        merged.extend_from_slice(&buf);
        std::fs::write(&tmp, &merged)?;
    } else {
        std::fs::write(&tmp, &buf)?;
    }

    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// Splits `response` into `(chain_of_thought, answer)`.
///
/// The last non-empty line is the answer; everything before is the chain of
/// thought.  Falls back to `("", response)` for single-line responses.
fn split_chain_of_thought(response: &str) -> (String, String) {
    let lines: Vec<&str> = response
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.len() <= 1 {
        return (String::new(), response.trim().to_string());
    }

    let answer = lines.last().unwrap().to_string();
    let cot = lines[..lines.len() - 1].join("\n");
    (cot, answer)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entries(pairs: &[(u64, u8, &str, &str)]) -> Vec<AuditTraceEntry> {
        let mut entries = Vec::new();
        for &(id, tier, prompt, response) in pairs {
            entries.push(AuditTraceEntry::TaskStarted {
                task_id: id,
                tier,
                prompt: prompt.into(),
            });
            entries.push(AuditTraceEntry::TaskCompleted {
                task_id: id,
                tokens_emitted: response.split_whitespace().count() as u32,
                response: response.into(),
            });
        }
        entries
    }

    // ── Pair extraction ───────────────────────────────────────────────────────

    #[test]
    fn empty_entries_produce_no_pairs() {
        let config = CompilationConfig {
            formats: vec![],
            ..Default::default()
        };
        let (report, pairs, errors) = compile_traces_to_pairs(&[], &config);
        assert_eq!(report.pairs_compiled, 0);
        assert!(pairs.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn matched_start_complete_produces_one_pair() {
        let entries = make_entries(&[(1, 0, "What is 2+2?", "4")]);
        let config = CompilationConfig {
            formats: vec![],
            ..Default::default()
        };
        let (report, pairs, _) = compile_traces_to_pairs(&entries, &config);
        assert_eq!(report.pairs_compiled, 1);
        assert_eq!(pairs[0].prompt, "What is 2+2?");
        assert_eq!(pairs[0].response, "4");
        assert_eq!(pairs[0].tier, 0);
        assert_eq!(pairs[0].task_id, 1);
    }

    #[test]
    fn failed_tasks_are_excluded_from_corpus() {
        let mut entries = vec![
            AuditTraceEntry::TaskStarted {
                task_id: 1,
                tier: 0,
                prompt: "prompt".into(),
            },
            AuditTraceEntry::TaskFailed {
                task_id: 1,
                error: "timeout".into(),
            },
        ];
        entries.extend(make_entries(&[(2, 1, "another", "response")]));
        let config = CompilationConfig {
            formats: vec![],
            ..Default::default()
        };
        let (report, pairs, _) = compile_traces_to_pairs(&entries, &config);
        assert_eq!(
            report.pairs_compiled, 1,
            "only completed task becomes a pair"
        );
        assert_eq!(pairs[0].task_id, 2);
    }

    #[test]
    fn multiple_tasks_all_produce_pairs() {
        let entries = make_entries(&[
            (10, 0, "p1", "r1"),
            (11, 1, "p2", "r2"),
            (12, 2, "p3", "r3"),
        ]);
        let config = CompilationConfig {
            formats: vec![],
            ..Default::default()
        };
        let (report, pairs, _) = compile_traces_to_pairs(&entries, &config);
        assert_eq!(report.pairs_compiled, 3);
        // All tasks must be represented.
        let ids: Vec<u64> = {
            let mut v: Vec<u64> = pairs.iter().map(|p| p.task_id).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(ids, vec![10, 11, 12]);
    }

    // ── File output ───────────────────────────────────────────────────────────

    /// E3.8 exit criterion 1: output files validate against the documented schemas.
    #[test]
    fn output_files_validate_against_schemas() {
        let dir = std::env::temp_dir().join("compilation_schema_test");
        let _ = std::fs::remove_dir_all(&dir);

        let entries = make_entries(&[(1, 0, "Question", "First step\nFinal answer")]);
        let config = CompilationConfig {
            output_dir: dir.clone(),
            formats: vec![
                TrainingFormat::Alpaca,
                TrainingFormat::Conversation,
                TrainingFormat::ChainOfThought,
            ],
            append: false,
        };

        let (report, _, errors) = compile_traces_to_pairs(&entries, &config);
        assert!(errors.is_empty(), "no I/O errors expected: {errors:?}");
        assert_eq!(report.files_written, 3);

        // Validate Alpaca schema.
        let alpaca_path = dir.join("alpaca.jsonl");
        let alpaca_content = std::fs::read_to_string(&alpaca_path).unwrap();
        let alpaca: AlpacaRecord = serde_json::from_str(alpaca_content.trim()).unwrap();
        assert_eq!(alpaca.instruction, "Question");
        assert_eq!(alpaca.output, "First step\nFinal answer");
        assert!(alpaca.input.is_empty());

        // Validate Conversation schema.
        let conv_path = dir.join("conversation.jsonl");
        let conv_content = std::fs::read_to_string(&conv_path).unwrap();
        let conv: ConversationRecord = serde_json::from_str(conv_content.trim()).unwrap();
        assert_eq!(conv.conversations.len(), 2);
        assert_eq!(conv.conversations[0].role, "human");
        assert_eq!(conv.conversations[1].role, "gpt");

        // Validate ChainOfThought schema.
        let cot_path = dir.join("chain_of_thought.jsonl");
        let cot_content = std::fs::read_to_string(&cot_path).unwrap();
        let cot: ChainOfThoughtRecord = serde_json::from_str(cot_content.trim()).unwrap();
        assert_eq!(cot.prompt, "Question");
        assert!(
            !cot.chain_of_thought.is_empty(),
            "multi-line response should split"
        );
        assert_eq!(cot.answer, "Final answer");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_files_written_when_formats_list_is_empty() {
        let dir = std::env::temp_dir().join("compilation_no_formats");
        let entries = make_entries(&[(1, 0, "p", "r")]);
        let config = CompilationConfig {
            formats: vec![],
            output_dir: dir.clone(),
            append: false,
        };
        let (report, _, errors) = compile_traces_to_pairs(&entries, &config);
        assert_eq!(report.files_written, 0);
        assert!(errors.is_empty());
        // The output dir must not have been created.
        assert!(!dir.exists());
    }

    /// E3.8 exit criterion 2: emergency consolidation triggers and marks the report.
    #[test]
    fn emergency_consolidation_marks_report_and_flushes_pairs() {
        let dir = std::env::temp_dir().join("compilation_emergency");
        let _ = std::fs::remove_dir_all(&dir);

        let entries = make_entries(&[(99, 0, "urgent prompt", "urgent response")]);
        let config = CompilationConfig {
            output_dir: dir.clone(),
            formats: vec![TrainingFormat::Alpaca],
            append: false,
        };

        let (report, pairs, errors) = emergency_consolidate(&entries, &config);
        assert!(errors.is_empty());
        assert!(report.emergency_consolidation, "flag must be set");
        assert_eq!(report.pairs_compiled, 1);
        assert_eq!(pairs[0].prompt, "urgent prompt");
        assert_eq!(report.files_written, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Split chain-of-thought ────────────────────────────────────────────────

    #[test]
    fn split_single_line_response_has_empty_cot() {
        let (cot, answer) = split_chain_of_thought("The answer is 42.");
        assert!(cot.is_empty());
        assert_eq!(answer, "The answer is 42.");
    }

    #[test]
    fn split_multi_line_response_extracts_last_line_as_answer() {
        let response = "First I think about it.\nThen I calculate.\nThe result is 7.";
        let (cot, answer) = split_chain_of_thought(response);
        assert_eq!(answer, "The result is 7.");
        assert!(cot.contains("First I think"));
        assert!(cot.contains("Then I calculate"));
    }

    // ── Report completeness ───────────────────────────────────────────────────

    #[test]
    fn compilation_report_contains_entries_processed_count() {
        let entries = make_entries(&[(1, 0, "q", "a"), (2, 0, "q2", "a2")]);
        let total = entries.len();
        let config = CompilationConfig {
            formats: vec![],
            ..Default::default()
        };
        let (report, _, _) = compile_traces_to_pairs(&entries, &config);
        assert_eq!(report.entries_processed, total);
    }

    #[test]
    fn append_mode_accumulates_across_calls() {
        let dir = std::env::temp_dir().join("compilation_append");
        let _ = std::fs::remove_dir_all(&dir);

        let e1 = make_entries(&[(1, 0, "first", "answer1")]);
        let e2 = make_entries(&[(2, 0, "second", "answer2")]);

        let cfg = CompilationConfig {
            output_dir: dir.clone(),
            formats: vec![TrainingFormat::Alpaca],
            append: true,
        };

        let (r1, _, errs1) = compile_traces_to_pairs(&e1, &cfg);
        assert!(errs1.is_empty());
        assert_eq!(r1.files_written, 1);

        let (r2, _, errs2) = compile_traces_to_pairs(&e2, &cfg);
        assert!(errs2.is_empty());
        assert_eq!(r2.files_written, 1);

        // File should now contain 2 JSONL lines.
        let content = std::fs::read_to_string(dir.join("alpaca.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "append mode should accumulate records");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

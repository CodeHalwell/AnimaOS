#![forbid(unsafe_code)]

//! Persistent job registry — E32 S32.3.
//!
//! [`JobRegistry`] stores [`ScheduledJob`]s keyed by `job_id` and persists
//! them to a JSON file under the agent's state directory.  Writes are atomic:
//! the file is written to a `.tmp` sibling before being renamed, preventing
//! corruption on a crash mid-write.
//!
//! Default path: `~/.anima/<agent_id>/jobs.json`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::job::ScheduledJob;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors returned by [`JobRegistry`] operations.
#[derive(Debug, PartialEq)]
pub enum JobRegistryError {
    /// A job with this `job_id` already exists.
    AlreadyExists { job_id: String },
    /// No job with this `job_id` was found.
    NotFound { job_id: String },
    /// A serialisation or I/O error occurred.
    Io(String),
}

impl std::fmt::Display for JobRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists { job_id } => write!(f, "job already exists: {job_id}"),
            Self::NotFound { job_id } => write!(f, "job not found: {job_id}"),
            Self::Io(msg) => write!(f, "registry I/O error: {msg}"),
        }
    }
}

// ── On-disk schema ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    schema_version: u32,
    jobs: HashMap<String, ScheduledJob>,
}

// ── JobRegistry ───────────────────────────────────────────────────────────────

/// An in-memory registry of [`ScheduledJob`]s with optional atomic JSON
/// persistence.
#[derive(Debug)]
pub struct JobRegistry {
    jobs: HashMap<String, ScheduledJob>,
    path: Option<PathBuf>,
}

impl JobRegistry {
    /// Default storage path for a given `agent_id`.
    ///
    /// Resolves to `~/.anima/<agent_id>/jobs.json`.
    pub fn default_path(agent_id: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        PathBuf::from(home)
            .join(".anima")
            .join(agent_id)
            .join("jobs.json")
    }

    /// Opens (or creates) a registry at `path`.
    ///
    /// If the file does not exist an empty registry is returned; the file is
    /// only written on the first [`flush`](Self::flush) call.
    pub fn open(path: &Path) -> Result<Self, JobRegistryError> {
        if path.exists() {
            let data =
                std::fs::read_to_string(path).map_err(|e| JobRegistryError::Io(e.to_string()))?;
            let file: RegistryFile =
                serde_json::from_str(&data).map_err(|e| JobRegistryError::Io(e.to_string()))?;
            Ok(Self {
                jobs: file.jobs,
                path: Some(path.to_owned()),
            })
        } else {
            Ok(Self {
                jobs: HashMap::new(),
                path: Some(path.to_owned()),
            })
        }
    }

    /// Creates an in-memory registry with no persistence.
    ///
    /// [`flush`](Self::flush) is a no-op in this mode.
    pub fn in_memory() -> Self {
        Self {
            jobs: HashMap::new(),
            path: None,
        }
    }

    /// Returns `true` when the registry has a backing file path.
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    // ── mutations ─────────────────────────────────────────────────────────────

    /// Adds a new job to the registry.
    ///
    /// Returns [`JobRegistryError::AlreadyExists`] when a job with the same
    /// `job_id` is already present.
    pub fn add(&mut self, job: ScheduledJob) -> Result<(), JobRegistryError> {
        let id = job.job_id.clone();
        if self.jobs.contains_key(&id) {
            return Err(JobRegistryError::AlreadyExists { job_id: id });
        }
        self.jobs.insert(id, job);
        Ok(())
    }

    /// Removes a job by ID, returning it.  Returns `None` if the job did not
    /// exist.
    pub fn remove(&mut self, job_id: &str) -> Option<ScheduledJob> {
        self.jobs.remove(job_id)
    }

    // ── queries ───────────────────────────────────────────────────────────────

    /// Returns a shared reference to a job.
    pub fn get(&self, job_id: &str) -> Option<&ScheduledJob> {
        self.jobs.get(job_id)
    }

    /// Returns a mutable reference to a job.
    pub fn get_mut(&mut self, job_id: &str) -> Option<&mut ScheduledJob> {
        self.jobs.get_mut(job_id)
    }

    /// Iterates over all `(job_id, &ScheduledJob)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ScheduledJob)> {
        self.jobs.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Returns the number of registered jobs.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Returns `true` when no jobs are registered.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    // ── persistence ───────────────────────────────────────────────────────────

    /// Atomically writes the registry to its backing file.
    ///
    /// Writes to a `.tmp` sibling then renames — safe against mid-write
    /// crashes.  Returns `Ok(())` immediately when no path is configured
    /// (in-memory mode).
    pub fn flush(&self) -> Result<(), JobRegistryError> {
        let path = match &self.path {
            Some(p) => p,
            None => return Ok(()),
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| JobRegistryError::Io(e.to_string()))?;
        }

        let file = RegistryFile {
            schema_version: 1,
            jobs: self.jobs.clone(),
        };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| JobRegistryError::Io(e.to_string()))?;

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| JobRegistryError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| JobRegistryError::Io(e.to_string()))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{job::ScheduledJob, schedule::JobSchedule};

    fn make_job(id_hint: &str) -> ScheduledJob {
        let ns = id_hint.len() as u64 * 1_000_000;
        ScheduledJob::new(id_hint, "", "{}", JobSchedule::Immediate, ns)
    }

    #[test]
    fn empty_registry_is_empty() {
        let reg = JobRegistry::in_memory();
        assert_eq!(reg.len(), 0);
        assert!(reg.is_empty());
        assert!(!reg.is_persistent());
    }

    #[test]
    fn add_inserts_job() {
        let mut reg = JobRegistry::in_memory();
        reg.add(make_job("backup")).unwrap();
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
    }

    #[test]
    fn add_rejects_duplicate_job_id() {
        let mut reg = JobRegistry::in_memory();
        let job = make_job("backup");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        // Build a second job with the same id
        let mut job2 = make_job("backup2");
        job2.job_id = id.clone();
        let err = reg.add(job2).unwrap_err();
        assert_eq!(err, JobRegistryError::AlreadyExists { job_id: id });
    }

    #[test]
    fn get_returns_none_for_missing_job() {
        let reg = JobRegistry::in_memory();
        assert!(reg.get("no-such-job").is_none());
    }

    #[test]
    fn get_returns_job_after_add() {
        let mut reg = JobRegistry::in_memory();
        let job = make_job("sync");
        let id = job.job_id.clone();
        reg.add(job).unwrap();
        assert!(reg.get(&id).is_some());
    }

    #[test]
    fn remove_extracts_job() {
        let mut reg = JobRegistry::in_memory();
        let job = make_job("cleanup");
        let id = job.job_id.clone();
        reg.add(job).unwrap();
        let removed = reg.remove(&id);
        assert!(removed.is_some());
        assert!(reg.is_empty());
    }

    #[test]
    fn remove_returns_none_for_missing() {
        let mut reg = JobRegistry::in_memory();
        assert!(reg.remove("no-such").is_none());
    }

    #[test]
    fn get_mut_allows_mutation() {
        let mut reg = JobRegistry::in_memory();
        let job = make_job("report");
        let id = job.job_id.clone();
        reg.add(job).unwrap();
        reg.get_mut(&id).unwrap().status = crate::job::JobStatus::Paused;
        assert_eq!(reg.get(&id).unwrap().status, crate::job::JobStatus::Paused);
    }

    #[test]
    fn iter_returns_all_jobs() {
        let mut reg = JobRegistry::in_memory();
        reg.add(make_job("a")).unwrap();
        reg.add(make_job("b")).unwrap();
        let ids: Vec<&str> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn len_reflects_count() {
        let mut reg = JobRegistry::in_memory();
        assert_eq!(reg.len(), 0);
        reg.add(make_job("x")).unwrap();
        assert_eq!(reg.len(), 1);
        reg.add(make_job("y")).unwrap();
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");

        let job = make_job("nightly-backup");
        let id = job.job_id.clone();

        let mut reg = JobRegistry::open(&path).unwrap();
        reg.add(job).unwrap();
        reg.flush().unwrap();

        let restored = JobRegistry::open(&path).unwrap();
        assert_eq!(restored.len(), 1);
        assert!(restored.get(&id).is_some());
        assert_eq!(restored.get(&id).unwrap().description, "nightly-backup");
    }

    #[test]
    fn open_creates_empty_registry_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");
        let reg = JobRegistry::open(&path).unwrap();
        assert!(reg.is_empty());
        assert!(reg.is_persistent());
    }

    #[test]
    fn in_memory_flush_is_no_op() {
        let reg = JobRegistry::in_memory();
        assert!(reg.flush().is_ok());
    }

    #[test]
    fn multiple_jobs_survive_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");

        let mut reg = JobRegistry::open(&path).unwrap();
        for i in 0..5 {
            reg.add(make_job(&format!("job-{i}"))).unwrap();
        }
        reg.flush().unwrap();

        let restored = JobRegistry::open(&path).unwrap();
        assert_eq!(restored.len(), 5);
    }
}

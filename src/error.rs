// src/error.rs
//
// Unified error type for PyFreeze-RS.
// Every module returns `Result<T, FreezeError>`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FreezeError {
    // ── I/O ──────────────────────────────────────────────────────────────────
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // ── Snapshot integrity ────────────────────────────────────────────────────
    #[error("snapshot is stale — source hash mismatch (expected {expected}, got {actual})")]
    StaleSnapshot { expected: String, actual: String },

    #[error("snapshot version mismatch: snapshot was built with {snapshot_ver}, current is {current_ver}")]
    VersionMismatch {
        snapshot_ver: String,
        current_ver:  String,
    },

    #[error("snapshot file is corrupt: {reason}")]
    CorruptSnapshot { reason: String },

    // ── Capture ───────────────────────────────────────────────────────────────
    #[error("failed to capture module '{module}': {reason}")]
    CaptureFailure { module: String, reason: String },

    #[error("failed to capture file descriptor {fd}: {reason}")]
    FdCaptureFailure { fd: i32, reason: String },

    // ── Restore ───────────────────────────────────────────────────────────────
    #[error("failed to restore module '{module}': {reason}")]
    RestoreFailure { module: String, reason: String },

    #[error("failed to restore file descriptor {fd}: {reason}")]
    FdRestoreFailure { fd: i32, reason: String },

    // ── Python / PyO3 ─────────────────────────────────────────────────────────
    #[error("Python error: {0}")]
    Python(#[from] pyo3::PyErr),

    // ── Serialization ─────────────────────────────────────────────────────────
    #[error("serialization error: {0}")]
    Serialization(String),

    // ── Unsupported ───────────────────────────────────────────────────────────
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),

    #[error("unsupported Python version: {version} (need CPython 3.10+)")]
    UnsupportedPythonVersion { version: String },
}

impl From<bincode::Error> for FreezeError {
    fn from(e: bincode::Error) -> Self {
        FreezeError::Serialization(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, FreezeError>;

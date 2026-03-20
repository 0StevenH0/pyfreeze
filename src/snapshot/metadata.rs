// src/snapshot/metadata.rs
//
// Builds and validates the `SnapshotMetadata` struct.
// Also handles the JSON sidecar file (`<snapshot>.meta.json`) which lets
// human operators inspect a snapshot without parsing the binary blob.

use chrono::Utc;
use serde_json;
use std::path::{Path, PathBuf};

use crate::{
    error::{FreezeError, Result},
    snapshot::format::SnapshotMetadata,
};

/// Minimum CPython minor version we support (3.10).
const MIN_PYTHON_MINOR: u64 = 10;

// ─── Construction ─────────────────────────────────────────────────────────────

pub struct MetadataBuilder {
    source_hash:      String,
    python_impl_ver:  String,
    target_triple:    String,
    framework:        String,
    import_phase_ms:  u64,
}

impl MetadataBuilder {
    pub fn new() -> Self {
        Self {
            source_hash:     String::new(),
            python_impl_ver: String::new(),
            target_triple:   current_target_triple(),
            framework:       "generic".into(),
            import_phase_ms: 0,
        }
    }

    pub fn source_hash(mut self, h: impl Into<String>) -> Self {
        self.source_hash = h.into();
        self
    }

    pub fn python_impl_version(mut self, v: impl Into<String>) -> Self {
        self.python_impl_ver = v.into();
        self
    }

    pub fn framework(mut self, f: impl Into<String>) -> Self {
        self.framework = f.into();
        self
    }

    pub fn import_phase_ms(mut self, ms: u64) -> Self {
        self.import_phase_ms = ms;
        self
    }

    pub fn build(self) -> SnapshotMetadata {
        SnapshotMetadata {
            source_hash:             self.source_hash,
            python_impl_version:     self.python_impl_ver,
            target_triple:           self.target_triple,
            captured_at:             Utc::now().to_rfc3339(),
            framework:               self.framework,
            import_phase_ms:         self.import_phase_ms,
        }
    }
}

// ─── Validation ───────────────────────────────────────────────────────────────

/// Check that the snapshot's runtime matches the *current* runtime.
/// Returns `Err` if there is a mismatch that would make restoration unsafe.
pub fn validate_runtime_compatibility(
    meta:           &SnapshotMetadata,
    current_py_ver: &str,
    current_triple: &str,
) -> Result<()> {
    // Architecture must match exactly.
    if meta.target_triple != current_triple {
        return Err(FreezeError::VersionMismatch {
            snapshot_ver: meta.target_triple.clone(),
            current_ver:  current_triple.to_string(),
        });
    }

    // CPython major.minor must match; patch may differ.
    let snap_mm    = parse_major_minor(&meta.python_impl_version);
    let current_mm = parse_major_minor(current_py_ver);

    match (snap_mm, current_mm) {
        (Some((sm, sn)), Some((cm, cn))) => {
            if sm != cm || sn != cn {
                return Err(FreezeError::VersionMismatch {
                    snapshot_ver: meta.python_impl_version.clone(),
                    current_ver:  current_py_ver.to_string(),
                });
            }
            if sn < MIN_PYTHON_MINOR {
                return Err(FreezeError::UnsupportedPythonVersion {
                    version: meta.python_impl_version.clone(),
                });
            }
        }
        _ => {
            // Couldn't parse — be conservative and reject.
            return Err(FreezeError::VersionMismatch {
                snapshot_ver: meta.python_impl_version.clone(),
                current_ver:  current_py_ver.to_string(),
            });
        }
    }

    Ok(())
}

// ─── Sidecar JSON ─────────────────────────────────────────────────────────────

/// Write a human-readable JSON sidecar next to the binary snapshot.
/// e.g. `/tmp/myapp.pyfreeze`  →  `/tmp/myapp.pyfreeze.meta.json`
pub fn write_sidecar(snapshot_path: &Path, meta: &SnapshotMetadata) -> Result<()> {
    let sidecar = sidecar_path(snapshot_path);
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| FreezeError::Serialization(e.to_string()))?;
    std::fs::write(&sidecar, json)?;
    Ok(())
}

pub fn read_sidecar(snapshot_path: &Path) -> Result<SnapshotMetadata> {
    let sidecar = sidecar_path(snapshot_path);
    let json = std::fs::read_to_string(&sidecar)?;
    serde_json::from_str(&json).map_err(|e| FreezeError::Serialization(e.to_string()))
}

fn sidecar_path(snapshot_path: &Path) -> PathBuf {
    let mut p = snapshot_path.to_path_buf();
    let name = p
        .file_name()
        .map(|n| format!("{}.meta.json", n.to_string_lossy()))
        .unwrap_or_else(|| "snapshot.meta.json".into());
    p.set_file_name(name);
    p
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Returns the current compile-time target triple.
fn current_target_triple() -> String {
    // We embed this at compile time via environment variables.
    // Falls back to a reasonable default if the env var isn't set.
    option_env!("PYFREEZE_TARGET_TRIPLE")
        .unwrap_or(std::env::consts::ARCH)
        .to_string()
        + "-"
        + std::env::consts::OS
}

/// Parse "CPython 3.12.3" → Some((3, 12)).
fn parse_major_minor(version_str: &str) -> Option<(u64, u64)> {
    // Strip "CPython " prefix if present.
    let digits = version_str
        .split_whitespace()
        .last()
        .unwrap_or(version_str);

    let mut parts = digits.splitn(3, '.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_versions() {
        assert_eq!(parse_major_minor("CPython 3.12.3"), Some((3, 12)));
        assert_eq!(parse_major_minor("3.11.9"),         Some((3, 11)));
        assert_eq!(parse_major_minor("garbage"),        None);
    }

    #[test]
    fn runtime_mismatch_rejected() {
        let meta = MetadataBuilder::new()
            .python_impl_version("CPython 3.11.9")
            .source_hash("abc")
            .build();

        let result = validate_runtime_compatibility(&meta, "CPython 3.12.3", "x86_64-linux");
        assert!(matches!(result, Err(FreezeError::VersionMismatch { .. })));
    }
}

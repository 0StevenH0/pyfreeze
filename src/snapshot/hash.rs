// src/snapshot/hash.rs
//
// Computes a deterministic hash over the set of Python source files that were
// imported during the capture phase.  If ANY of those files change on disk,
// the hash changes → the snapshot is declared stale and is rebuilt.
//
// The hash includes:
//   • The content of every .py file in sys.modules
//   • The CPython version string
//   • The target architecture
//
// What it does NOT include (intentionally):
//   • Timestamps — we use content hashes to be robust against `touch` and
//     build systems that restore mtimes.
//   • .pyc files — if the .py changed, the .pyc changes too; no double-work.

use sha2::{Digest, Sha256};
use std::path::Path;
use tracing::{debug, warn};

use crate::error::{FreezeError, Result};

/// Aggregate SHA-256 over a list of source-file paths + the runtime identity.
pub fn compute_source_hash(
    source_paths: &[String],
    python_version: &str,
    target_triple: &str,
) -> Result<String> {
    let mut hasher = Sha256::new();

    // Salt the hash with the runtime identity so that a snapshot built on
    // CPython 3.11 is never loaded by CPython 3.12.
    hasher.update(python_version.as_bytes());
    hasher.update(b"\x00");
    hasher.update(target_triple.as_bytes());
    hasher.update(b"\x00");

    // Sort paths so the hash is order-independent (sys.modules ordering can
    // differ between runs on some platforms).
    let mut sorted = source_paths.to_vec();
    sorted.sort_unstable();

    for path_str in &sorted {
        let path = Path::new(path_str);

        // Feed the path itself so that moving a file invalidates the snapshot.
        hasher.update(path_str.as_bytes());
        hasher.update(b"\x00");

        match std::fs::read(path) {
            Ok(contents) => {
                debug!("hashing {}", path_str);
                hasher.update(&contents);
            }
            Err(e) => {
                // File disappeared between capture and validation — treat as stale.
                warn!("cannot read '{}' for hash: {e}", path_str);
                hasher.update(b"<missing>");
            }
        }

        hasher.update(b"\xFF"); // separator between files
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Verify that a snapshot's recorded hash still matches the current disk state.
/// Returns `Ok(())` if valid, or `Err(FreezeError::StaleSnapshot)` if not.
pub fn verify_snapshot_hash(
    expected_hash: &str,
    source_paths:  &[String],
    python_version: &str,
    target_triple:  &str,
) -> Result<()> {
    let actual = compute_source_hash(source_paths, python_version, target_triple)?;

    if actual != expected_hash {
        return Err(FreezeError::StaleSnapshot {
            expected: expected_hash.to_string(),
            actual,
        });
    }

    Ok(())
}

/// Quick check: does the snapshot file itself look intact?
/// Reads the header magic bytes without deserializing the whole file.
pub fn quick_integrity_check(snapshot_path: &Path) -> Result<bool> {
    use crate::snapshot::format::MAGIC;

    let mut buf = [0u8; 8];
    let mut f = std::fs::File::open(snapshot_path)?;
    use std::io::Read;
    f.read_exact(&mut buf)?;

    Ok(&buf == MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn same_content_same_hash() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"print('hello')").unwrap();
        f2.write_all(b"print('hello')").unwrap();

        let h1 = compute_source_hash(
            &[f1.path().to_string_lossy().into()],
            "CPython 3.12.3",
            "x86_64-linux",
        ).unwrap();

        let h2 = compute_source_hash(
            &[f2.path().to_string_lossy().into()],
            "CPython 3.12.3",
            "x86_64-linux",
        ).unwrap();

        assert_eq!(h1, h2);
    }

    #[test]
    fn changed_content_different_hash() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"x = 1").unwrap();
        let path: String = f.path().to_string_lossy().into();

        let h1 = compute_source_hash(&[path.clone()], "CPython 3.12.3", "x86_64-linux").unwrap();

        // Overwrite the file.
        f.as_file().set_len(0).unwrap();
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(0)).unwrap();
        f.write_all(b"x = 2").unwrap();

        let h2 = compute_source_hash(&[path], "CPython 3.12.3", "x86_64-linux").unwrap();

        assert_ne!(h1, h2);
    }

    #[test]
    fn version_difference_produces_different_hash() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"pass").unwrap();
        let path: String = f.path().to_string_lossy().into();

        let h1 = compute_source_hash(&[path.clone()], "CPython 3.11.9", "x86_64-linux").unwrap();
        let h2 = compute_source_hash(&[path],          "CPython 3.12.3", "x86_64-linux").unwrap();

        assert_ne!(h1, h2);
    }
}

// src/capture/mod.rs
//
// Orchestrates the full capture sequence:
//   1. Record the start time.
//   2. Walk sys.modules (graph_walker).
//   3. Capture open file descriptors (fd_capture).
//   4. Build snapshot metadata.
//   5. Write everything to disk via SnapshotWriter.

pub mod fd_capture;
pub mod graph_walker;

use std::path::{Path, PathBuf};
use std::time::Instant;

use pyo3::prelude::*;
use tracing::info;

use crate::{
    error::Result,
    snapshot::{
        format::SnapshotHeader,
        hash::compute_source_hash,
        metadata::{MetadataBuilder, write_sidecar},
        SnapshotWriter,
    },
};

// ─── CaptureConfig ────────────────────────────────────────────────────────────

pub struct CaptureConfig {
    /// Where to write the snapshot file.
    pub snapshot_path: PathBuf,

    /// "django" | "flask" | "generic"
    pub framework: String,
}

impl CaptureConfig {
    pub fn new(snapshot_path: impl Into<PathBuf>, framework: impl Into<String>) -> Self {
        Self {
            snapshot_path: snapshot_path.into(),
            framework:     framework.into(),
        }
    }
}

// ─── capture() ───────────────────────────────────────────────────────────────

/// Capture a snapshot of the current Python interpreter state.
///
/// Must be called from within a `Python::with_gil` closure (or equivalent)
/// and should be called right after the import phase completes — i.e., after
/// `django.setup()` or `app = Flask(__name__)` has finished.
pub fn capture(py: Python<'_>, config: &CaptureConfig, start_time: Instant) -> Result<PathBuf> {
    let elapsed_ms = start_time.elapsed().as_millis() as u64;
    info!(elapsed_ms, "beginning capture");

    // 1. Walk the module graph.
    let (module_table, pickle_blobs) = graph_walker::walk_sys_modules(py)?;

    // 2. Capture file descriptors.
    let fd_table = fd_capture::capture_fd_table()?;

    // 3. Collect source paths for the global hash.
    let source_paths: Vec<String> = module_table
        .entries
        .iter()
        .filter_map(|e| e.source_path.clone())
        .collect();

    let (python_version, target_triple) = get_runtime_identity(py)?;

    let source_hash = compute_source_hash(&source_paths, &python_version, &target_triple)?;

    // 4. Build metadata.
    let metadata = MetadataBuilder::new()
        .source_hash(source_hash)
        .python_impl_version(&python_version)
        .framework(&config.framework)
        .import_phase_ms(elapsed_ms)
        .build();

    // 5. Assemble and write the snapshot.
    let header = SnapshotHeader {
        metadata:            metadata.clone(),
        // Offsets are patched by SnapshotWriter::commit().
        module_table_offset: 0,
        fd_table_offset:     0,
        pickle_blob_offset:  0,
    };

    let mut writer = SnapshotWriter::new(&config.snapshot_path, header);

    // Register pickle blobs; the writer returns offsets that we write back
    // into each entry's blob_offset / blob_len.
    let mut adjusted_module_table = module_table;
    for entry in &mut adjusted_module_table.entries {
        if let (Some(old_off), Some(len)) = (entry.blob_offset, entry.blob_len) {
            let (new_off, _) = writer.add_pickle_blob(
                &pickle_blobs[old_off as usize..(old_off + len) as usize],
            );
            entry.blob_offset = Some(new_off);
        }
    }

    writer.set_module_table(adjusted_module_table);
    writer.set_fd_table(fd_table);

    let written_path = writer.commit()?;
    write_sidecar(&written_path, &metadata)?;

    info!(
        path = %written_path.display(),
        import_ms = elapsed_ms,
        "snapshot committed"
    );

    Ok(written_path)
}

// ─── Runtime identity ─────────────────────────────────────────────────────────

pub fn get_runtime_identity(py: Python<'_>) -> Result<(String, String)> {
    let sys = pyo3::types::PyModule::import(py, "sys")?;

    let vi  = sys.getattr("version_info")?;
    let major: u32 = vi.getattr("major")?.extract()?;
    let minor: u32 = vi.getattr("minor")?.extract()?;
    let micro: u32 = vi.getattr("micro")?.extract()?;
    let impl_name: String = sys.getattr("implementation")?.getattr("name")?.extract()?;

    let version = format!("{} {}.{}.{}", impl_name, major, minor, micro);

    let platform: String = sys.getattr("platform")?.extract()?;
    let arch = std::env::consts::ARCH;
    let triple = format!("{}-{}", arch, platform);

    Ok((version, triple))
}

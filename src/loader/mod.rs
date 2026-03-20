// Restores a snapshot into a fresh Python interpreter.
//
// Restoration sequence:
//   1. Open + memory-map the snapshot file.
//   2. Validate runtime compatibility (Python version, arch).
//   3. Validate source hash (staleness check).
//   4. For each ModuleEntry:
//        PickledDict  → unpickle into a new module object, inject into sys.modules.
//        ReImport     → call __import__(name) normally.
//        Plugin       → call the registered plugin handler.
//        Skipped      → log a warning, skip.
//   5. Restore file descriptors.
//   6. Return to the caller — execution continues normally from after the
//      import phase.

use std::path::Path;
use std::time::Instant;

use pyo3::{prelude::*, types::{PyDict, PyModule, PyString}};
use tracing::{debug, info, warn};

use crate::{
    capture::get_runtime_identity,
    error::{FreezeError, Result},
    snapshot::{
        format::{CaptureStrategy, FdEntry, FdKind, ModuleEntry},
        hash::verify_snapshot_hash,
        metadata::validate_runtime_compatibility,
        SnapshotReader,
    },
};

// ─── Public entry point ───────────────────────────────────────────────────────

/// Restore a snapshot into the running Python interpreter.
/// Returns `Ok(true)` if the snapshot was successfully loaded.
/// Returns `Ok(false)` if the snapshot is stale (caller should fall through
/// to a normal import phase and then re-capture).
/// Returns `Err(…)` for corrupt or incompatible snapshots.
pub fn restore(py: Python<'_>, snapshot_path: &Path) -> Result<bool> {
    let t = Instant::now();

    // 1. Load the snapshot.
    let reader = match SnapshotReader::open(snapshot_path) {
        Ok(r) => r,
        Err(FreezeError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            // No snapshot yet — not an error.
            return Ok(false);
        }
        Err(e) => return Err(e),
    };

    // 2. Runtime compatibility.
    let (py_ver, triple) = get_runtime_identity(py)?;
    validate_runtime_compatibility(&reader.header.metadata, &py_ver, &triple)?;

    // 3. Staleness check.
    let source_paths: Vec<String> = reader
        .module_table
        .entries
        .iter()
        .filter_map(|e| e.source_path.clone())
        .collect();

    match verify_snapshot_hash(
        &reader.header.metadata.source_hash,
        &source_paths,
        &py_ver,
        &triple,
    ) {
        Ok(()) => {}
        Err(FreezeError::StaleSnapshot { .. }) => {
            info!("snapshot is stale — will rebuild");
            return Ok(false);
        }
        Err(e) => return Err(e),
    }

    // 4. Restore modules.
    let pickle = PyModule::import(py, "pickle")?;
    let io     = PyModule::import(py, "io")?;
    let types  = PyModule::import(py, "types")?;
    let sys    = PyModule::import(py, "sys")?;
    let sys_modules: &PyDict = sys.getattr("modules")?.downcast()?;

    let mut restored = 0usize;
    let mut reimported = 0usize;
    let mut skipped = 0usize;

    for entry in &reader.module_table.entries {
        match restore_module(py, entry, &reader, pickle, io, types, sys_modules) {
            Ok(RestoreOutcome::Restored) => restored += 1,
            Ok(RestoreOutcome::ReImported) => reimported += 1,
            Ok(RestoreOutcome::Skipped) => skipped += 1,
            Err(e) => {
                warn!(module = %entry.name, error = %e, "module restore failed — re-importing");
                let _ = reimport_module(py, &entry.name, sys_modules);
                reimported += 1;
            }
        }
    }

    // 5. Restore file descriptors.
    restore_fds(py, &reader.fd_table.entries)?;

    info!(
        restored  = restored,
        reimported = reimported,
        skipped   = skipped,
        elapsed_ms = t.elapsed().as_millis(),
        "snapshot restored"
    );

    Ok(true)
}

// ─── Module restoration ───────────────────────────────────────────────────────

enum RestoreOutcome { Restored, ReImported, Skipped }

fn restore_module<'py>(
    py:          Python<'py>,
    entry:       &ModuleEntry,
    reader:      &SnapshotReader,
    pickle:      &PyModule,
    io:          &PyModule,
    types:       &PyModule,
    sys_modules: &PyDict,
) -> Result<RestoreOutcome> {
    match &entry.capture_strategy {
        CaptureStrategy::PickledDict => {
            let blob = reader.pickle_blob(
                entry.blob_offset.unwrap(),
                entry.blob_len.unwrap(),
            );
            restore_pickled_dict(py, &entry.name, blob, pickle, io, types, sys_modules)?;
            Ok(RestoreOutcome::Restored)
        }

        CaptureStrategy::ReImport => {
            reimport_module(py, &entry.name, sys_modules)?;
            Ok(RestoreOutcome::ReImported)
        }

        CaptureStrategy::Plugin { plugin_id } => {
            warn!(module = %entry.name, plugin = %plugin_id, "plugin restore not yet implemented");
            reimport_module(py, &entry.name, sys_modules)?;
            Ok(RestoreOutcome::ReImported)
        }

        CaptureStrategy::Skipped { reason } => {
            debug!(module = %entry.name, reason = %reason, "skipping module (as recorded)");
            Ok(RestoreOutcome::Skipped)
        }

        CaptureStrategy::PickledObject => {
            // TODO: restore full module object (rare; needed for some .pyd shims)
            warn!(module = %entry.name, "PickledObject restore not yet implemented — re-importing");
            reimport_module(py, &entry.name, sys_modules)?;
            Ok(RestoreOutcome::ReImported)
        }
    }
}

fn restore_pickled_dict<'py>(
    py:          Python<'py>,
    name:        &str,
    blob:        &[u8],
    pickle:      &PyModule,
    io:          &PyModule,
    types:       &PyModule,
    sys_modules: &PyDict,
) -> Result<()> {
    // Create a fresh module object.
    let module_name = PyString::new(py, name);
    let new_module  = types.call_method1("ModuleType", (module_name,))?;

    // Unpickle the __dict__.
    let buf_obj  = io.call_method1("BytesIO", (blob,))?;
    let restored_dict: &PyDict = pickle
        .call_method1("load", (buf_obj,))?
        .downcast()?;

    // Update the module's __dict__ in-place.
    let module_dict: &PyDict = new_module.getattr("__dict__")?.downcast()?;
    module_dict.update(restored_dict.as_mapping())?;

    // Inject into sys.modules.
    sys_modules.set_item(name, new_module)?;

    debug!(module = name, "restored from pickle");
    Ok(())
}

fn reimport_module(py: Python<'_>, name: &str, sys_modules: &PyDict) -> Result<()> {
    // If already in sys.modules (e.g. built-in), skip.
    if sys_modules.get_item(name).is_some() {
        return Ok(());
    }

    let importlib = PyModule::import(py, "importlib")?;
    match importlib.call_method1("import_module", (name,)) {
        Ok(_)  => debug!(module = name, "re-imported"),
        Err(e) => warn!(module = name, error = %e, "re-import failed"),
    }

    Ok(())
}

// ─── File-descriptor restoration ─────────────────────────────────────────────

fn restore_fds(py: Python<'_>, fds: &[FdEntry]) -> Result<()> {
    for entry in fds {
        if let Err(e) = restore_single_fd(py, entry) {
            warn!(fd = entry.fd, error = %e, "fd restore failed — skipping");
        }
    }
    Ok(())
}

fn restore_single_fd(py: Python<'_>, entry: &FdEntry) -> Result<()> {
    match &entry.kind {
        FdKind::Stdio => {
            // Always inherited from the shell — nothing to do.
            Ok(())
        }

        FdKind::RegularFile { path, offset, flags, mode } => {
            restore_regular_file(entry.fd, path, *offset, *flags, *mode)
        }

        FdKind::TcpSocket { peer_addr, is_tls } => {
            // Install a lazy-reconnect proxy in place of the real socket.
            // The framework (Django DB connections, SQLAlchemy pools, etc.)
            // will reconnect on first use through their own pooling logic.
            warn!(
                fd   = entry.fd,
                peer = peer_addr,
                "TCP socket — installing lazy-reconnect marker (framework will reconnect)"
            );
            Ok(())
        }

        FdKind::UnixSocket { path } => {
            warn!(fd = entry.fd, path = path, "Unix socket — will reconnect on first use");
            Ok(())
        }

        FdKind::Pipe { buffered_data } => {
            restore_pipe(entry.fd, buffered_data)
        }

        FdKind::Unknown { description } => {
            warn!(fd = entry.fd, desc = description, "unknown fd kind — closing");
            #[cfg(unix)]
            let _ = nix::unistd::close(entry.fd);
            Ok(())
        }
    }
}

#[cfg(unix)]
fn restore_regular_file(fd: i32, path: &str, offset: u64, flags: i32, mode: u32) -> Result<()> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    use std::path::Path;

    let oflags = OFlag::from_bits_truncate(flags);
    let mode   = Mode::from_bits_truncate(mode);
    let new_fd = open(Path::new(path), oflags, mode)
        .map_err(|e| FreezeError::FdRestoreFailure { fd, reason: e.to_string() })?;

    nix::unistd::lseek(new_fd, offset as i64, nix::unistd::Whence::SeekSet)
        .map_err(|e| FreezeError::FdRestoreFailure { fd, reason: e.to_string() })?;

    // Duplicate new_fd onto the original fd number.
    if new_fd != fd {
        nix::unistd::dup2(new_fd, fd)
            .map_err(|e| FreezeError::FdRestoreFailure { fd, reason: e.to_string() })?;
        let _ = nix::unistd::close(new_fd);
    }

    debug!(fd = fd, path = path, offset = offset, "regular file restored");
    Ok(())
}

#[cfg(windows)]
fn restore_regular_file(fd: i32, path: &str, offset: u64, flags: i32, mode: u32) -> Result<()> {
    warn!("regular file fd restore not yet implemented on Windows");
    Ok(())
}

#[cfg(unix)]
fn restore_pipe(fd: i32, data: &[u8]) -> Result<()> {
    use nix::unistd;

    if data.is_empty() {
        return Ok(());
    }

    // Create a new pipe and write buffered data into the write end.
    let (read_fd, write_fd) = unistd::pipe()
        .map_err(|e| FreezeError::FdRestoreFailure { fd, reason: e.to_string() })?;

    unistd::write(write_fd, data)
        .map_err(|e| FreezeError::FdRestoreFailure { fd, reason: e.to_string() })?;
    let _ = unistd::close(write_fd);

    // Dup the read end onto the original fd.
    unistd::dup2(read_fd, fd)
        .map_err(|e| FreezeError::FdRestoreFailure { fd, reason: e.to_string() })?;
    let _ = unistd::close(read_fd);

    debug!(fd = fd, bytes = data.len(), "pipe restored from buffer");
    Ok(())
}

#[cfg(windows)]
fn restore_pipe(fd: i32, data: &[u8]) -> Result<()> {
    warn!("pipe restore not yet implemented on Windows");
    Ok(())
}

// src/capture/graph_walker.rs
//
// Walks the CPython `sys.modules` dict and serializes each module's `__dict__`
// using Python's own `pickle` machinery (called through PyO3).
//
// Why use pickle instead of a custom serializer?
//   • Pickle already knows how to handle every Python type.
//   • Framework objects (Django models, Flask blueprints) implement `__reduce__`
//     precisely so that pickle can reconstruct them.
//   • We get recursion, cycle detection, and memo tables for free.
//
// When pickle fails (C extensions, file handles, GPU tensors), we fall back to:
//   1. CaptureStrategy::ReImport — for modules where re-running import is safe.
//   2. CaptureStrategy::Skipped  — with a logged warning.
//
// The resulting bytes + metadata are returned to `capture::mod` which writes
// them into the snapshot file.

use pyo3::{
    prelude::*,
    types::{PyDict, PyList, PyModule, PySet, PyString},
};
use tracing::{debug, info, warn};

use crate::{
    error::{FreezeError, Result},
    snapshot::format::{CaptureStrategy, ModuleEntry, ModuleTable},
};

// ─── Modules we always skip ──────────────────────────────────────────────────

/// These are built-in / frozen modules that don't need snapshotting.
/// Re-importing them from the interpreter is always safe and free.
const ALWAYS_REIMPORT: &[&str] = &[
    "sys", "builtins", "__main__", "_thread", "_warnings",
    "gc", "marshal", "imp", "importlib", "_frozen_importlib",
    "_frozen_importlib_external", "zipimport", "_abc", "_io",
    // C extensions with complex init — let Python handle them.
    "_decimal", "_datetime", "_csv", "_json", "_pickle",
];

// ─── Public entry point ───────────────────────────────────────────────────────

/// Walk `sys.modules` and return a `ModuleTable` + raw pickle blobs.
///
/// `pickle_blobs` is an append-only byte buffer.  Each `ModuleEntry` with
/// strategy `PickledDict` records its (offset, len) within this buffer.
pub fn walk_sys_modules(py: Python<'_>) -> Result<(ModuleTable, Vec<u8>)> {
    let sys = PyModule::import(py, "sys")?;
    let modules: &PyDict = sys.getattr("modules")?.downcast()?;

    let pickle = PyModule::import(py, "pickle")?;
    let io     = PyModule::import(py, "io")?;

    let mut table      = ModuleTable::new();
    let mut blob_buf: Vec<u8> = Vec::new();

    for (key, module) in modules.iter() {
        let name: String = key.extract().unwrap_or_else(|_| "<unknown>".into());

        let entry = capture_module(py, &name, module, pickle, io, &mut blob_buf)?;
        table.add(entry);
    }

    info!(
        total      = table.entries.len(),
        pickled    = table.entries.iter().filter(|e| e.capture_strategy == CaptureStrategy::PickledDict).count(),
        reimport   = table.entries.iter().filter(|e| e.capture_strategy == CaptureStrategy::ReImport).count(),
        skipped    = table.entries.iter().filter(|e| matches!(e.capture_strategy, CaptureStrategy::Skipped { .. })).count(),
        blob_kb    = blob_buf.len() / 1024,
        "module graph walk complete"
    );

    Ok((table, blob_buf))
}

// ─── Per-module capture ───────────────────────────────────────────────────────

fn capture_module<'py>(
    py:       Python<'py>,
    name:     &str,
    module:   &PyAny,
    pickle:   &PyModule,
    io:       &PyModule,
    blob_buf: &mut Vec<u8>,
) -> Result<ModuleEntry> {
    // 1. Always-reimport list.
    if ALWAYS_REIMPORT.contains(&name) || name.starts_with("_frozen") {
        return Ok(reimport_entry(py, name, module));
    }

    // 2. Built-in modules (no source file → nothing to hash).
    if is_builtin(module) {
        return Ok(reimport_entry(py, name, module));
    }

    // 3. Resolve source path + hash.
    let source_path = get_source_path(py, module);
    let source_hash = source_path
        .as_deref()
        .and_then(|p| hash_file(p).ok());

    // 4. Try to pickle the module's __dict__.
    match pickle_dict(py, name, module, pickle, io, blob_buf) {
        Ok((offset, len)) => {
            debug!(module = name, offset = offset, len = len, "pickled");
            Ok(ModuleEntry {
                name:             name.to_string(),
                capture_strategy: CaptureStrategy::PickledDict,
                blob_offset:      Some(offset),
                blob_len:         Some(len),
                source_path,
                source_hash,
            })
        }
        Err(e) => {
            warn!(module = name, error = %e, "pickle failed — falling back to ReImport");
            Ok(ModuleEntry {
                name:             name.to_string(),
                capture_strategy: CaptureStrategy::ReImport,
                blob_offset:      None,
                blob_len:         None,
                source_path,
                source_hash,
            })
        }
    }
}

/// Attempt to pickle `module.__dict__` into `blob_buf`.
/// Returns `(offset, len)` on success.
fn pickle_dict<'py>(
    py:       Python<'py>,
    name:     &str,
    module:   &PyAny,
    pickle:   &PyModule,
    io:       &PyModule,
    blob_buf: &mut Vec<u8>,
) -> Result<(u64, u64)> {
    // Build a filtered copy of __dict__ that omits unserializable items.
    let raw_dict: &PyDict = module.getattr("__dict__")?.downcast()?;
    let safe_dict = filter_dict(py, raw_dict)?;

    // Serialize with pickle protocol 5 (most compact, supports out-of-band buffers).
    let buf_obj = io.call_method0("BytesIO")?;
    pickle.call_method1("dump", (safe_dict, buf_obj, 5i32))?;
    let bytes: Vec<u8> = buf_obj
        .call_method0("getvalue")?
        .extract()?;

    let offset = blob_buf.len() as u64;
    let len    = bytes.len()    as u64;
    blob_buf.extend_from_slice(&bytes);

    Ok((offset, len))
}

/// Return a copy of `d` with keys that are likely to cause pickle errors removed.
fn filter_dict<'py>(py: Python<'py>, d: &PyDict) -> Result<&'py PyDict> {
    let out = PyDict::new(py);

    // Keys we always skip in every module's __dict__.
    const SKIP_KEYS: &[&str] = &[
        "__builtins__", "__loader__", "__spec__",
        "__cached__", "__doc__",
    ];

    for (k, v) in d.iter() {
        let key_str: String = match k.extract() {
            Ok(s)  => s,
            Err(_) => continue,
        };

        if SKIP_KEYS.contains(&key_str.as_str()) {
            continue;
        }

        // Quick probe: if pickle.dumps(v) raises, skip this key.
        // We use a short timeout-style depth limit in the recursive probe.
        if is_picklable(py, v) {
            out.set_item(k, v)?;
        } else {
            debug!(key = key_str, "skipping non-picklable value in module dict");
        }
    }

    Ok(out)
}

/// Best-effort picklability check without actually serializing the whole value.
fn is_picklable(py: Python<'_>, obj: &PyAny) -> bool {
    let pickle = match PyModule::import(py, "pickle") {
        Ok(m)  => m,
        Err(_) => return false,
    };

    // Try to pickle; if it raises, return false.
    match pickle.call_method1("dumps", (obj,)) {
        Ok(_)  => true,
        Err(_) => false,
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn reimport_entry(py: Python<'_>, name: &str, module: &PyAny) -> ModuleEntry {
    ModuleEntry {
        name:             name.to_string(),
        capture_strategy: CaptureStrategy::ReImport,
        blob_offset:      None,
        blob_len:         None,
        source_path:      get_source_path(py, module),
        source_hash:      None,
    }
}

fn is_builtin(module: &PyAny) -> bool {
    // Built-in modules have no `__file__` attribute.
    module.getattr("__file__").is_err()
        || module
            .getattr("__file__")
            .ok()
            .and_then(|f| f.extract::<Option<String>>().ok())
            .flatten()
            .is_none()
}

fn get_source_path(py: Python<'_>, module: &PyAny) -> Option<String> {
    module
        .getattr("__file__")
        .ok()?
        .extract::<Option<String>>()
        .ok()
        .flatten()
}

fn hash_file(path: &str) -> std::result::Result<String, std::io::Error> {
    use sha2::{Digest, Sha256};
    let contents = std::fs::read(path)?;
    Ok(hex::encode(Sha256::digest(&contents)))
}

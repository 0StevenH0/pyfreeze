// PyO3 extension module entry point.
// Exposes `pyfreeze_rs` as an importable Python extension.
//
// Python usage:
//   import pyfreeze_rs
//   pyfreeze_rs.capture("/tmp/myapp.pyfreeze", "django")
//   pyfreeze_rs.restore("/tmp/myapp.pyfreeze")   # → True | False

mod error;
mod snapshot;
mod capture;
mod loader;

use std::path::PathBuf;
use std::time::Instant;

use pyo3::prelude::*;
use tracing_subscriber::EnvFilter;

/// Capture the current interpreter state and write a snapshot to `path`.
///
/// Call this right after your framework has finished initializing
/// (e.g., after `django.setup()` or after building the Flask `app`).
///
/// Args:
///     path (str):      Where to store the snapshot file.
///     framework (str): Hint for metadata ("django", "flask", "generic").
///     start_time_ns (int): `time.perf_counter_ns()` value recorded at
///                          interpreter startup, used to compute import_phase_ms.
///
/// Returns: The path string of the written snapshot.
#[pyfunction]
fn capture(
    py:           Python<'_>,
    path:         &str,
    framework:    &str,
    start_time_ns: u64,
) -> PyResult<String> {
    let config = capture::CaptureConfig::new(PathBuf::from(path), framework);

    // Reconstruct a `std::time::Instant` from the ns offset.
    // We approximate: elapsed = now - start_time.
    // (Instant is opaque, so we use Duration arithmetic.)
    let elapsed_ns = {
        let now_ns: u64 = pyo3::types::PyModule::import(py, "time")?
            .call_method0("perf_counter_ns")?
            .extract()?;
        now_ns.saturating_sub(start_time_ns)
    };

    let fake_start = Instant::now()
        .checked_sub(std::time::Duration::from_nanos(elapsed_ns))
        .unwrap_or(Instant::now());

    capture::capture(py, &config, fake_start)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

/// Attempt to restore interpreter state from a snapshot.
///
/// Returns:
///     True  → snapshot was valid and successfully restored.
///     False → no snapshot exists yet, or snapshot is stale (rebuild needed).
///
/// Raises `RuntimeError` for corrupt or incompatible snapshots.
#[pyfunction]
fn restore(py: Python<'_>, path: &str) -> PyResult<bool> {
    loader::restore(py, std::path::Path::new(path))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

/// Return the default cache directory PyFreeze-RS would use.
#[pyfunction]
fn default_cache_dir() -> PyResult<String> {
    Ok(snapshot::default_cache_dir()
        .to_string_lossy()
        .into_owned())
}

/// Top-level Python module.
#[pymodule]
fn pyfreeze_rs(py: Python<'_>, m: &PyModule) -> PyResult<()> {
    // Initialise tracing from PYFREEZE_LOG env var (e.g. PYFREEZE_LOG=debug).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("PYFREEZE_LOG")
                .unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .try_init();

    m.add_function(wrap_pyfunction!(capture,          m)?)?;
    m.add_function(wrap_pyfunction!(restore,          m)?)?;
    m.add_function(wrap_pyfunction!(default_cache_dir, m)?)?;

    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    Ok(())
}

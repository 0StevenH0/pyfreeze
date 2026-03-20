# tests/python/test_api.py
#
# Tests for the public pyfreeze Python API.
# These exercise the pure-Python paths (no Rust extension required).

import sys
import time
import types
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent.parent / "python"))

import pyfreeze


# ─── Helpers ─────────────────────────────────────────────────────────────────

def _make_mock_rs(restore_returns: bool = False):
    """Return a mock pyfreeze_rs extension module."""
    mock = MagicMock()
    mock.restore.return_value = restore_returns
    mock.capture.return_value = "/tmp/test.pyfreeze"
    mock.default_cache_dir.return_value = "/tmp/pyfreeze_cache"
    return mock


# ─── bootstrap() ─────────────────────────────────────────────────────────────

def test_bootstrap_no_rust_is_noop(tmp_path, monkeypatch):
    """bootstrap() must be a no-op when the Rust extension is absent."""
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", False)
    monkeypatch.setattr(pyfreeze, "_rs", None)
    # Should not raise.
    pyfreeze.bootstrap(snapshot_path=str(tmp_path / "snap.pyfreeze"))


def test_bootstrap_calls_restore_when_snapshot_exists(tmp_path, monkeypatch):
    snap = tmp_path / "snap.pyfreeze"
    snap.touch()   # file exists

    mock_rs = _make_mock_rs(restore_returns=True)
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    monkeypatch.setattr(pyfreeze, "_rs", mock_rs)

    pyfreeze.bootstrap(snapshot_path=str(snap))
    mock_rs.restore.assert_called_once_with(str(snap))


def test_bootstrap_installs_hook_when_no_snapshot(tmp_path, monkeypatch):
    snap = tmp_path / "missing.pyfreeze"
    # File does NOT exist.

    mock_rs = _make_mock_rs(restore_returns=False)
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    monkeypatch.setattr(pyfreeze, "_rs", mock_rs)

    original_meta_path_len = len(sys.meta_path)
    pyfreeze.bootstrap(snapshot_path=str(snap), framework="generic")

    # A hook must have been inserted into sys.meta_path.
    assert len(sys.meta_path) == original_meta_path_len + 1

    # Clean up.
    sys.meta_path.pop(0)


def test_bootstrap_rebuild_flag_skips_restore(tmp_path, monkeypatch):
    snap = tmp_path / "snap.pyfreeze"
    snap.touch()

    mock_rs = _make_mock_rs(restore_returns=True)
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    monkeypatch.setattr(pyfreeze, "_rs", mock_rs)

    pyfreeze.bootstrap(snapshot_path=str(snap), rebuild=True)

    # restore() must NOT have been called — rebuild=True forces a fresh capture.
    mock_rs.restore.assert_not_called()


# ─── CaptureContext ───────────────────────────────────────────────────────────

def test_capture_context_commit_calls_rs_capture(tmp_path, monkeypatch):
    snap    = tmp_path / "ctx.pyfreeze"
    mock_rs = _make_mock_rs()
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    monkeypatch.setattr(pyfreeze, "_rs", mock_rs)

    ctx = pyfreeze.CaptureContext(snapshot_path=str(snap), framework="flask")
    ctx.start()
    result = ctx.commit()

    mock_rs.capture.assert_called_once()
    call_args = mock_rs.capture.call_args
    assert call_args[0][0] == str(snap)
    assert call_args[0][1] == "flask"
    assert isinstance(call_args[0][2], int)   # start_ns


def test_capture_context_requires_start_before_commit(tmp_path, monkeypatch):
    snap    = tmp_path / "ctx2.pyfreeze"
    mock_rs = _make_mock_rs()
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    monkeypatch.setattr(pyfreeze, "_rs", mock_rs)

    ctx = pyfreeze.CaptureContext(snapshot_path=str(snap))
    with pytest.raises(RuntimeError, match="start()"):
        ctx.commit()


def test_capture_context_as_context_manager(tmp_path, monkeypatch):
    snap    = tmp_path / "ctx3.pyfreeze"
    mock_rs = _make_mock_rs()
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    monkeypatch.setattr(pyfreeze, "_rs", mock_rs)

    with pyfreeze.CaptureContext(snapshot_path=str(snap)):
        pass   # __exit__ calls commit()

    mock_rs.capture.assert_called_once()


def test_capture_context_noop_without_rust(tmp_path, monkeypatch):
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", False)
    monkeypatch.setattr(pyfreeze, "_rs", None)

    ctx = pyfreeze.CaptureContext(snapshot_path=str(tmp_path / "x.pyfreeze"))
    ctx.start()
    result = ctx.commit()   # must not raise
    assert result is None


# ─── enabled() / snapshot_path() ─────────────────────────────────────────────

def test_enabled_reflects_has_rust(monkeypatch):
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    assert pyfreeze.enabled() is True

    monkeypatch.setattr(pyfreeze, "_HAS_RUST", False)
    assert pyfreeze.enabled() is False


def test_snapshot_path_from_env(monkeypatch):
    monkeypatch.setenv("PYFREEZE_SNAPSHOT", "/custom/path.pyfreeze")
    assert pyfreeze.snapshot_path() == Path("/custom/path.pyfreeze")


def test_snapshot_path_none_when_env_unset(monkeypatch):
    monkeypatch.delenv("PYFREEZE_SNAPSHOT", raising=False)
    assert pyfreeze.snapshot_path() is None


# ─── _PostImportCapture hook ──────────────────────────────────────────────────

def test_hook_fires_on_trigger_module(tmp_path, monkeypatch):
    from pyfreeze import _PostImportCapture, _atexit_capture

    snap     = tmp_path / "hook.pyfreeze"
    fired    = []
    mock_rs  = _make_mock_rs()
    monkeypatch.setattr(pyfreeze, "_HAS_RUST", True)
    monkeypatch.setattr(pyfreeze, "_rs", mock_rs)

    # Patch atexit.register so we can inspect what gets registered.
    import atexit as _atexit
    registered = []
    monkeypatch.setattr(_atexit, "register", lambda fn, *a, **kw: registered.append((fn, a)))
    monkeypatch.setattr(_atexit, "unregister", lambda fn: None)

    hook = _PostImportCapture(snap, "flask", time.perf_counter_ns())
    sys.meta_path.insert(0, hook)
    try:
        hook.find_spec("flask.app", None)   # simulate importing the trigger module
    finally:
        if hook in sys.meta_path:
            sys.meta_path.remove(hook)

    assert hook._fired is True
    # An atexit callback must have been registered.
    assert any(fn == _atexit_capture for fn, _ in registered)


def test_hook_ignores_non_trigger_modules(tmp_path):
    from pyfreeze import _PostImportCapture

    snap = tmp_path / "notfired.pyfreeze"
    hook = _PostImportCapture(snap, "flask", time.perf_counter_ns())

    hook.find_spec("os", None)
    hook.find_spec("sys", None)
    hook.find_spec("collections", None)

    assert hook._fired is False

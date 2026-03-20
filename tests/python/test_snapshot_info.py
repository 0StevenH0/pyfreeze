# Tests for the pure-Python sidecar reader.
# These run without the Rust extension — pytest tests/python/

import json
import sys
from pathlib import Path

import pytest

# Add the Python package to the path regardless of whether maturin has
# installed the Rust extension.
sys.path.insert(0, str(Path(__file__).parent.parent.parent / "python"))

from pyfreeze._snapshot_info import format_info, is_stale, read_info


# ─── Fixtures ────────────────────────────────────────────────────────────────

@pytest.fixture
def sidecar(tmp_path: Path):
    """Write a synthetic sidecar next to a fake snapshot and return its path."""
    snap = tmp_path / "test.pyfreeze"
    snap.touch()

    import platform, sys as _sys
    vi = _sys.version_info
    py_ver    = f"{_sys.implementation.name} {vi.major}.{vi.minor}.{vi.micro}"
    triple    = f"{platform.machine()}-{_sys.platform}"

    meta = {
        "source_hash":         "aabbccddeeff00112233445566778899",
        "python_impl_version": py_ver,
        "target_triple":       triple,
        "captured_at":         "2025-01-01T00:00:00+00:00",
        "framework":           "django",
        "import_phase_ms":     350,
    }

    sidecar_path = tmp_path / "test.pyfreeze.meta.json"
    sidecar_path.write_text(json.dumps(meta))

    return snap


# ─── read_info ────────────────────────────────────────────────────────────────

def test_read_info_returns_dict(sidecar):
    info = read_info(sidecar)
    assert isinstance(info, dict)
    assert info["framework"] == "django"
    assert info["import_phase_ms"] == 350


def test_read_info_missing_sidecar_raises(tmp_path):
    snap = tmp_path / "nonexistent.pyfreeze"
    with pytest.raises(FileNotFoundError):
        read_info(snap)


def test_read_info_malformed_json_raises(tmp_path):
    snap = tmp_path / "bad.pyfreeze"
    snap.touch()
    (tmp_path / "bad.pyfreeze.meta.json").write_text("{ not json }")
    with pytest.raises(ValueError, match="Malformed sidecar"):
        read_info(snap)


# ─── is_stale ─────────────────────────────────────────────────────────────────

def test_current_runtime_is_not_stale(sidecar):
    # Sidecar was written with current Python version → should not be stale.
    assert is_stale(sidecar) is False


def test_different_python_version_is_stale(tmp_path):
    snap = tmp_path / "old.pyfreeze"
    snap.touch()

    meta = {
        "source_hash":         "deadbeef",
        "python_impl_version": "cpython 2.7.18",   # ancient
        "target_triple":       "x86_64-linux",
        "captured_at":         "2020-01-01T00:00:00+00:00",
        "framework":           "flask",
        "import_phase_ms":     100,
    }
    (tmp_path / "old.pyfreeze.meta.json").write_text(json.dumps(meta))

    assert is_stale(snap) is True


def test_missing_sidecar_not_stale(tmp_path):
    snap = tmp_path / "no_sidecar.pyfreeze"
    snap.touch()
    # No sidecar → can't determine staleness → assume valid.
    assert is_stale(snap) is False


# ─── format_info ─────────────────────────────────────────────────────────────

def test_format_info_contains_key_fields(sidecar):
    text = format_info(sidecar)
    assert "django" in text
    assert "350ms" in text
    assert "PyFreeze Snapshot" in text


def test_format_info_missing_snapshot(tmp_path):
    result = format_info(tmp_path / "ghost.pyfreeze")
    assert "No snapshot found" in result

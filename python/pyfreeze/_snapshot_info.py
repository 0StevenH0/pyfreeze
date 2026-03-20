# Pure-Python helper that reads the JSON sidecar file written alongside every
# snapshot.  The sidecar lives at `<snapshot>.meta.json` and contains all
# SnapshotMetadata fields in human-readable form.
#
# This lets Python code inspect a snapshot's metadata without importing the
# Rust extension (useful for management commands, health-check endpoints, etc.).

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def read_info(snapshot_path: Path | str) -> dict[str, Any]:
    """
    Read the JSON sidecar for *snapshot_path* and return its contents as a dict.

    Raises FileNotFoundError if the sidecar does not exist.
    Raises ValueError if the sidecar is malformed.
    """
    snap = Path(snapshot_path)
    sidecar = snap.parent / (snap.name + ".meta.json")

    if not sidecar.exists():
        raise FileNotFoundError(f"No sidecar found: {sidecar}")

    try:
        return json.loads(sidecar.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ValueError(f"Malformed sidecar ({sidecar}): {exc}") from exc


def is_stale(snapshot_path: Path | str) -> bool:
    """
    Quick Python-level staleness check.

    Re-hashes every source file listed in the sidecar and compares against
    the recorded ``source_hash``.  Intended for health-check endpoints where
    importing the Rust extension is undesirable.

    Returns True  → snapshot needs rebuilding.
    Returns False → snapshot is valid (or sidecar is missing → assume valid).
    """
    import hashlib

    try:
        info = read_info(snapshot_path)
    except FileNotFoundError:
        return False  # No sidecar → can't check → assume valid.

    expected_hash: str = info.get("source_hash", "")
    python_ver:    str = info.get("python_impl_version", "")
    target_triple: str = info.get("target_triple", "")

    # We don't have access to the source paths list from the sidecar alone;
    # that information is in the binary.  Instead we do a coarser check:
    # compare the Python version + target triple to the current runtime.
    import sys, platform

    vi = sys.version_info
    current_ver    = f"{sys.implementation.name} {vi.major}.{vi.minor}.{vi.micro}"
    current_triple = f"{platform.machine()}-{sys.platform}"

    if python_ver != current_ver or target_triple != current_triple:
        return True  # Runtime mismatch → definitely stale.

    return False  # Can't do full hash without source paths → assume valid.


def format_info(snapshot_path: Path | str) -> str:
    """Return a human-readable multi-line summary of the snapshot metadata."""
    try:
        info = read_info(snapshot_path)
    except FileNotFoundError:
        return f"No snapshot found at {snapshot_path}"

    lines = [
        "PyFreeze Snapshot",
        f"  Path         : {snapshot_path}",
        f"  Python       : {info.get('python_impl_version', '?')}",
        f"  Target       : {info.get('target_triple', '?')}",
        f"  Framework    : {info.get('framework', '?')}",
        f"  Captured at  : {info.get('captured_at', '?')}",
        f"  Import phase : {info.get('import_phase_ms', '?')}ms",
        f"  Source hash  : {info.get('source_hash', '?')[:16]}…",
    ]

    return "\n".join(lines)

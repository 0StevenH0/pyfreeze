# PyFreeze public Python API.
#
# Two usage patterns:
#
# ① Automatic (recommended) — call bootstrap() at the very top of your
#   entry point (manage.py / wsgi.py / app.py) BEFORE any framework imports:
#
#     import pyfreeze
#     pyfreeze.bootstrap()         # framework auto-detected
#     # ... rest of your file unchanged
#
# ② Manual — wrap your import phase explicitly if you need fine control:
#
#     import pyfreeze
#     ctx = pyfreeze.CaptureContext()
#     ctx.start()
#     import django; django.setup()   # ← expensive imports go here
#     ctx.commit()

from __future__ import annotations

import os
import sys
import time
import logging
from pathlib import Path
from typing import Optional

log = logging.getLogger("pyfreeze")

# Lazily import the Rust extension so that the pure-Python fallback still works
# when the extension hasn't been compiled yet.
try:
    import pyfreeze_rs as _rs
    _HAS_RUST = True
except ImportError:
    _rs = None          # type: ignore[assignment]
    _HAS_RUST = False
    log.warning(
        "pyfreeze_rs native extension not found — "
        "running without snapshot acceleration. "
        "Build with: maturin develop"
    )

__version__ = "0.1.0"
__all__     = ["bootstrap", "CaptureContext", "enabled", "snapshot_path"]

# ─── Configuration from environment ──────────────────────────────────────────

def _env(key: str, default: str = "") -> str:
    return os.environ.get(key, default)

def _snapshot_path_from_env() -> Optional[Path]:
    raw = _env("PYFREEZE_SNAPSHOT")
    return Path(raw) if raw else None

def _framework_from_env() -> str:
    return _env("PYFREEZE_FRAMEWORK", "generic")

# ─── Public helpers ───────────────────────────────────────────────────────────

def enabled() -> bool:
    """Return True if the Rust extension is available and PyFreeze is active."""
    return _HAS_RUST

def snapshot_path() -> Optional[Path]:
    """Return the snapshot path that will be used for the current process."""
    return _snapshot_path_from_env()

# ─── bootstrap() — the one-liner API ─────────────────────────────────────────

def bootstrap(
    snapshot_path: Optional[str | Path] = None,
    framework:     Optional[str]        = None,
    rebuild:       bool                 = False,
    argv:          Optional[list]       = None,
) -> None:
    """
    Bootstrap PyFreeze at the very start of your process.

    If a valid snapshot exists → restore it and return immediately.
    If no snapshot (or stale)  → install a post-import hook that captures
                                  state once the framework finishes loading.

    Place this call BEFORE any framework imports:

        import pyfreeze; pyfreeze.bootstrap()
        import django   # ← captured
        django.setup()  # ← captured
    """
    if not _HAS_RUST:
        return

    snap = Path(snapshot_path) if snapshot_path else _snapshot_path_from_env()
    if snap is None:
        snap = _default_snapshot_path()

    fw = framework or _framework_from_env() or _detect_framework()

    # Record the true process-start time ASAP.
    start_ns = time.perf_counter_ns()

    if not rebuild and snap.exists():
        log.debug("attempting restore from %s", snap)
        try:
            restored = _rs.restore(str(snap))
        except RuntimeError as e:
            log.warning("snapshot invalid (%s) — will rebuild", e)
            restored = False

        if restored:
            log.info("restored from snapshot in %.1fms", (time.perf_counter_ns() - start_ns) / 1e6)
            return

    # Install the post-import hook.
    log.debug("no valid snapshot — installing capture hook")
    _install_capture_hook(snap, fw, start_ns)


# ─── CaptureContext — manual API ─────────────────────────────────────────────

class CaptureContext:
    """
    Manual wrapper for fine-grained control over the capture boundary.

    Example::

        ctx = pyfreeze.CaptureContext(framework="django")
        ctx.start()

        import django
        django.setup()

        ctx.commit()
    """

    def __init__(
        self,
        snapshot_path: Optional[str | Path] = None,
        framework:     str                  = "generic",
    ) -> None:
        self._snap  = Path(snapshot_path) if snapshot_path else _default_snapshot_path()
        self._fw    = framework
        self._start: Optional[int] = None

    def start(self) -> "CaptureContext":
        self._start = time.perf_counter_ns()
        return self

    def commit(self) -> Optional[Path]:
        """Serialize current interpreter state and write the snapshot."""
        if not _HAS_RUST:
            log.warning("pyfreeze_rs not available — commit() is a no-op")
            return None

        if self._start is None:
            raise RuntimeError("CaptureContext.start() must be called before commit()")

        snap_str = _rs.capture(str(self._snap), self._fw, self._start)
        return Path(snap_str)

    def __enter__(self) -> "CaptureContext":
        return self.start()

    def __exit__(self, *_: object) -> None:
        self.commit()


# ─── Import hook ─────────────────────────────────────────────────────────────

class _PostImportCapture:
    """
    sys.meta_path hook that watches for a "trigger" module and fires the
    capture once that module has been imported.

    For Django the trigger is `django.apps.registry` (set after django.setup()).
    For Flask the trigger is `flask.app` (set after Flask(__name__) returns).
    """

    _TRIGGERS = {
        "django":  "django.apps.registry",
        "flask":   "flask.app",
        "generic": None,   # generic: capture after first 3-second idle window
    }

    def __init__(self, snap: Path, fw: str, start_ns: int) -> None:
        self._snap     = snap
        self._fw       = fw
        self._start_ns = start_ns
        self._trigger  = self._TRIGGERS.get(fw)
        self._fired    = False

    # ── sys.meta_path interface ──────────────────────────────────────────────

    def find_module(self, name: str, path=None):
        return None  # never intercept; we only observe

    def find_spec(self, name: str, path, target=None):
        if not self._fired:
            self._maybe_fire(name)
        return None  # never intercept

    # ── Internal ─────────────────────────────────────────────────────────────

    def _maybe_fire(self, just_imported: str) -> None:
        if self._trigger and just_imported != self._trigger:
            return

        self._fired = True
        sys.meta_path.remove(self)

        # Give the framework a tick to finish module-level setup.
        # (We schedule the capture via atexit so that it runs after
        # the current import chain unwinds completely.)
        import atexit
        atexit.unregister(_atexit_capture)  # avoid duplicate registration
        atexit.register(_atexit_capture, self._snap, self._fw, self._start_ns)

        log.debug("capture scheduled (trigger: %s)", just_imported or "atexit")


def _atexit_capture(snap: Path, fw: str, start_ns: int) -> None:
    """Called once — either via atexit or explicitly after framework setup."""
    if not _HAS_RUST:
        return
    log.info("capturing snapshot → %s", snap)
    try:
        _rs.capture(str(snap), fw, start_ns)
        log.info("snapshot written")
    except Exception as e:
        log.error("capture failed: %s", e)


def _install_capture_hook(snap: Path, fw: str, start_ns: int) -> None:
    hook = _PostImportCapture(snap, fw, start_ns)
    sys.meta_path.insert(0, hook)


# ─── Helpers ─────────────────────────────────────────────────────────────────

def _default_snapshot_path() -> Path:
    """Derive a snapshot path from the main script's name."""
    main = getattr(sys.modules.get("__main__"), "__file__", None) or "generic"
    import hashlib
    key  = hashlib.sha256(main.encode()).hexdigest()[:16]
    if _HAS_RUST:
        base = Path(_rs.default_cache_dir())
    else:
        base = Path.home() / ".cache" / "pyfreeze"
    base.mkdir(parents=True, exist_ok=True)
    return base / f"{key}.pyfreeze"


def _detect_framework() -> str:
    """Guess the framework from sys.argv or installed packages."""
    argv0 = Path(sys.argv[0]).name if sys.argv else ""
    if "manage.py" in argv0 or "django" in argv0:
        return "django"
    if "flask" in argv0 or any("flask" in a for a in sys.argv[1:]):
        return "flask"
    return "generic"

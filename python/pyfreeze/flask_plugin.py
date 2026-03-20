# python/pyfreeze/flask_plugin.py
#
# Drop-in Flask integration.
#
# Usage — factory pattern (recommended):
#
#   from pyfreeze.flask_plugin import freeze_app
#
#   def create_app():
#       app = Flask(__name__)
#       app.config.from_object("config.ProductionConfig")
#       db.init_app(app)
#       # ... register blueprints ...
#       freeze_app(app)   # ← one line
#       return app
#
# Usage — simple script:
#
#   import pyfreeze.flask_plugin as pf
#   pf.patch_flask()      # call before importing Flask
#   from flask import Flask
#   app = Flask(__name__)
#   # snapshot is captured automatically when app object is created

from __future__ import annotations

import os
import sys
import time
import logging
from pathlib import Path
from typing import Optional, TYPE_CHECKING

if TYPE_CHECKING:
    from flask import Flask

log = logging.getLogger("pyfreeze.flask")

try:
    import pyfreeze_rs as _rs
    _HAS_RUST = True
except ImportError:
    _rs = None  # type: ignore[assignment]
    _HAS_RUST = False


# ─── freeze_app() — factory-pattern API ──────────────────────────────────────

def freeze_app(
    app:           "Flask",
    snapshot_path: Optional[str | Path] = None,
    rebuild:       bool                 = False,
    start_ns:      Optional[int]        = None,
) -> "Flask":
    """
    Capture a snapshot of the current interpreter state after the Flask
    application object has been fully configured.

    Call this at the END of your `create_app()` factory function.
    Returns `app` unchanged so it can be used inline.

    Example::

        def create_app():
            app = Flask(__name__)
            # configure, register blueprints, etc.
            return freeze_app(app)
    """
    if not _HAS_RUST:
        return app

    snap = _resolve_snapshot_path(snapshot_path, app.name)
    _start_ns = start_ns or time.perf_counter_ns()

    if not rebuild and snap.exists():
        try:
            if _rs.restore(str(snap)):
                elapsed = (time.perf_counter_ns() - _start_ns) / 1e6
                log.info("⚡ warm start — restored in %.1fms", elapsed)
                return app
        except RuntimeError as e:
            log.warning("snapshot invalid (%s) — rebuilding", e)

    # Capture now — the app is fully constructed.
    log.info("capturing Flask snapshot → %s", snap)
    try:
        _rs.capture(str(snap), "flask", _start_ns)
        elapsed = (time.perf_counter_ns() - _start_ns) / 1e6
        log.info("snapshot written in %.1fms", elapsed)
    except Exception as e:
        log.error("capture failed: %s", e)

    return app


# ─── patch_flask() — pre-import hook API ─────────────────────────────────────

_PATCH_START_NS: Optional[int] = None

def patch_flask(
    snapshot_path: Optional[str | Path] = None,
    rebuild:       bool                 = False,
) -> None:
    """
    Call BEFORE importing Flask.  Patches Flask.__init__ so that every new
    Flask application object triggers a capture automatically.

    This is the "one-liner" approach for simple scripts:

        import pyfreeze.flask_plugin as pf; pf.patch_flask()
        from flask import Flask
        app = Flask(__name__)   # ← snapshot captured here
    """
    global _PATCH_START_NS
    _PATCH_START_NS = time.perf_counter_ns()

    if not _HAS_RUST:
        return

    env_path = os.environ.get("PYFREEZE_SNAPSHOT")
    snap = Path(env_path) if env_path else None

    # If a snapshot already exists, try to restore before Flask even loads.
    if snap and snap.exists() and not rebuild:
        try:
            if _rs.restore(str(snap)):
                elapsed = (time.perf_counter_ns() - _PATCH_START_NS) / 1e6
                log.info("⚡ warm start — restored in %.1fms", elapsed)
                return
        except RuntimeError:
            pass

    # Install the Flask.__init__ patch.
    _install_flask_init_patch(snap, _PATCH_START_NS, rebuild)


def _install_flask_init_patch(
    snap:     Optional[Path],
    start_ns: int,
    rebuild:  bool,
) -> None:
    """
    Replaces Flask.__init__ with a version that fires the capture after the
    application object is fully constructed.
    """
    import importlib

    try:
        flask_module = importlib.import_module("flask")
    except ImportError:
        log.warning("Flask is not installed — patch_flask() is a no-op")
        return

    OriginalFlask = flask_module.Flask
    _snap = snap  # close over

    class _PatchedFlask(OriginalFlask):  # type: ignore[misc]
        def __init__(self, import_name: str, *args, **kwargs):
            super().__init__(import_name, *args, **kwargs)
            # Remove patch — only capture the first app object.
            flask_module.Flask = OriginalFlask

            resolved_snap = _snap or _resolve_snapshot_path(None, import_name)

            if not rebuild and resolved_snap.exists():
                try:
                    if _rs.restore(str(resolved_snap)):
                        elapsed = (time.perf_counter_ns() - start_ns) / 1e6
                        log.info("⚡ warm start — restored in %.1fms", elapsed)
                        return
                except RuntimeError as e:
                    log.warning("snapshot invalid (%s) — rebuilding", e)

            log.info("capturing Flask snapshot → %s", resolved_snap)
            try:
                _rs.capture(str(resolved_snap), "flask", start_ns)
            except Exception as e:
                log.error("capture failed: %s", e)

    flask_module.Flask = _PatchedFlask
    log.debug("Flask.__init__ patched for snapshot capture")


# ─── Flask extension — init_app() style ───────────────────────────────────────

class PyFreeze:
    """
    Flask extension object for applications that use the application factory
    pattern with `init_app()`.

    app/extensions.py:
        from pyfreeze.flask_plugin import PyFreeze
        pyfreeze = PyFreeze()

    app/__init__.py:
        from app.extensions import pyfreeze
        def create_app():
            app = Flask(__name__)
            pyfreeze.init_app(app)
            return app
    """

    def __init__(self) -> None:
        self._snap:     Optional[Path] = None
        self._start_ns: Optional[int]  = None

    def init_app(
        self,
        app:           "Flask",
        snapshot_path: Optional[str | Path] = None,
        rebuild:       bool                 = False,
    ) -> None:
        self._start_ns = time.perf_counter_ns()
        self._snap = _resolve_snapshot_path(snapshot_path, app.name)

        app.extensions["pyfreeze"] = self

        # Register a hook on first_request (fires after all blueprints are
        # registered and before any requests are served).
        @app.before_request
        def _first_request_capture():
            # Remove ourselves so this only fires once.
            app.before_request_funcs[None].remove(_first_request_capture)  # type: ignore

            if not _HAS_RUST:
                return

            if not rebuild and self._snap.exists():
                try:
                    if _rs.restore(str(self._snap)):
                        return
                except RuntimeError:
                    pass

            try:
                _rs.capture(str(self._snap), "flask", self._start_ns)
                log.info("snapshot written → %s", self._snap)
            except Exception as e:
                log.error("capture failed: %s", e)


# ─── Helpers ─────────────────────────────────────────────────────────────────

def _resolve_snapshot_path(
    path:     Optional[str | Path],
    app_name: str,
) -> Path:
    if path:
        return Path(path)

    env_path = os.environ.get("PYFREEZE_SNAPSHOT")
    if env_path:
        return Path(env_path)

    import hashlib
    key  = hashlib.sha256(f"flask:{app_name}".encode()).hexdigest()[:16]

    if _HAS_RUST:
        base = Path(_rs.default_cache_dir())
    else:
        base = Path.home() / ".cache" / "pyfreeze"

    base.mkdir(parents=True, exist_ok=True)
    return base / f"flask-{key}.pyfreeze"

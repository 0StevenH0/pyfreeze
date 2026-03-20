# python/pyfreeze/django_plugin.py
#
# Drop-in Django integration.
#
# Option A — manage.py (development):
#
#   #!/usr/bin/env python
#   import pyfreeze.django_plugin as _pf; _pf.patch_manage()
#   # rest of standard manage.py unchanged
#
# Option B — WSGI/ASGI (production):
#
#   # wsgi.py
#   import pyfreeze.django_plugin as _pf; _pf.patch_wsgi()
#   from django.core.wsgi import get_wsgi_application
#   application = get_wsgi_application()
#
# Both variants capture the snapshot AFTER django.setup() has run, so that
# the full ORM, signal handlers, and middleware configuration are all baked in.

from __future__ import annotations

import os
import sys
import time
import logging
from pathlib import Path
from typing import Callable, Optional

log = logging.getLogger("pyfreeze.django")

try:
    import pyfreeze_rs as _rs
    _HAS_RUST = True
except ImportError:
    _rs = None  # type: ignore[assignment]
    _HAS_RUST = False

# ─── patch_manage() ───────────────────────────────────────────────────────────

def patch_manage(
    snapshot_path: Optional[str | Path] = None,
    rebuild:       bool                 = False,
) -> None:
    """
    Call at the very top of manage.py, before any imports.

    If a valid snapshot exists it is restored and execution continues normally.
    If not, a hook is installed to capture state after django.setup() finishes.

    The snapshot is keyed on DJANGO_SETTINGS_MODULE + CPython version so that
    changing settings automatically invalidates the cache.
    """
    if not _HAS_RUST:
        log.debug("pyfreeze_rs not built — skipping")
        return

    snap = _resolve_snapshot_path(snapshot_path, "django")
    start_ns = time.perf_counter_ns()

    if not rebuild and snap.exists():
        try:
            if _rs.restore(str(snap)):
                _log_warm_start(start_ns)
                return
        except RuntimeError as e:
            log.warning("snapshot invalid (%s) — rebuilding", e)

    # Monkey-patch django.setup() to fire the capture after it returns.
    _hook_django_setup(snap, start_ns)
    log.debug("Django capture hook installed → %s", snap)


def _hook_django_setup(snap: Path, start_ns: int) -> None:
    """Replace django.setup() with a wrapper that captures state after it."""

    def _setup_wrapper(*args, **kwargs):
        # Call the real setup().
        _original_setup(*args, **kwargs)
        # Now Django's AppRegistry is fully populated — safe to capture.
        log.info("django.setup() complete — capturing snapshot")
        _capture(snap, "django", start_ns)
        # Restore the original so subsequent calls work normally.
        import django
        django.setup = _original_setup

    import django
    _original_setup = django.setup
    django.setup = _setup_wrapper


# ─── patch_wsgi() ─────────────────────────────────────────────────────────────

def patch_wsgi(
    snapshot_path: Optional[str | Path] = None,
    rebuild:       bool                 = False,
) -> None:
    """
    Call at the top of wsgi.py / asgi.py.

    Identical to patch_manage() but names the snapshot differently so that
    manage.py and gunicorn don't share the same snapshot (they may differ in
    which signals / middlewares are registered).
    """
    if not _HAS_RUST:
        return

    snap = _resolve_snapshot_path(snapshot_path, "django-wsgi")
    start_ns = time.perf_counter_ns()

    if not rebuild and snap.exists():
        try:
            if _rs.restore(str(snap)):
                _log_warm_start(start_ns)
                return
        except RuntimeError as e:
            log.warning("snapshot invalid (%s) — rebuilding", e)

    _hook_django_setup(snap, start_ns)


# ─── Gunicorn / uWSGI application factory hook ───────────────────────────────

class PyFreezeGunicornWorkerMixin:
    """
    Mix this into your Gunicorn worker class to get per-worker snapshots.

    gunicorn.conf.py:
        from pyfreeze.django_plugin import PyFreezeGunicornWorkerMixin
        from gunicorn.workers.sync import SyncWorker

        class Worker(PyFreezeGunicornWorkerMixin, SyncWorker):
            pass
        worker_class = "gunicorn.conf:Worker"
    """

    def init_process(self):  # type: ignore[override]
        patch_wsgi()
        super().init_process()  # type: ignore[misc]


# ─── Custom management command mixin ─────────────────────────────────────────

class PyFreezeCommandMixin:
    """
    Mixin for Django management commands that want explicit snapshot control.

    class Command(PyFreezeCommandMixin, BaseCommand):
        def handle(self, *args, **options):
            if options.get("rebuild_snapshot"):
                self.invalidate_snapshot()
            super().handle(*args, **options)

        def add_arguments(self, parser):
            super().add_arguments(parser)
            parser.add_argument("--rebuild-snapshot", action="store_true")
    """

    def invalidate_snapshot(self) -> None:
        snap = _resolve_snapshot_path(None, "django")
        if snap.exists():
            snap.unlink()
            self.stdout.write(self.style.SUCCESS(f"Snapshot invalidated: {snap}"))
        else:
            self.stdout.write("No snapshot found.")

    def snapshot_info(self) -> dict:
        snap = _resolve_snapshot_path(None, "django")
        if not snap.exists():
            return {"exists": False, "path": str(snap)}

        try:
            from pyfreeze._snapshot_info import read_info
            return {"exists": True, "path": str(snap), **read_info(snap)}
        except Exception:
            return {"exists": True, "path": str(snap)}


# ─── Helpers ─────────────────────────────────────────────────────────────────

def _capture(snap: Path, framework: str, start_ns: int) -> None:
    if not _HAS_RUST:
        return
    try:
        _rs.capture(str(snap), framework, start_ns)
        elapsed = (time.perf_counter_ns() - start_ns) / 1e6
        log.info("snapshot written in %.1fms → %s", elapsed, snap)
    except Exception as e:
        log.error("capture failed: %s", e)


def _resolve_snapshot_path(
    path:      Optional[str | Path],
    qualifier: str,
) -> Path:
    if path:
        return Path(path)

    env_path = os.environ.get("PYFREEZE_SNAPSHOT")
    if env_path:
        return Path(env_path)

    # Key on settings module + qualifier so different configs get different snaps.
    settings_module = os.environ.get("DJANGO_SETTINGS_MODULE", "default")

    import hashlib
    key  = hashlib.sha256(f"{settings_module}:{qualifier}".encode()).hexdigest()[:16]

    if _HAS_RUST:
        base = Path(_rs.default_cache_dir())
    else:
        base = Path.home() / ".cache" / "pyfreeze"

    base.mkdir(parents=True, exist_ok=True)
    return base / f"django-{key}.pyfreeze"


def _log_warm_start(start_ns: int) -> None:
    elapsed_ms = (time.perf_counter_ns() - start_ns) / 1e6
    log.info("⚡ warm start — restored in %.1fms", elapsed_ms)

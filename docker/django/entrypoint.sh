#!/bin/bash
# docker/django/entrypoint.sh
#
# Container entrypoint for the PyFreeze-RS / Django image.
#
# Responsibilities:
#   1. Print snapshot status for both manage + wsgi snapshots.
#   2. Prune stale snapshots (version mismatch).
#   3. Collect static files on first boot (skipped if SKIP_COLLECTSTATIC=1).
#   4. Exec the server command.

set -euo pipefail

# ── Helpers ───────────────────────────────────────────────────────────────────
check_snapshot() {
    local label="$1"
    local snap="$2"
    local sidecar="${snap}.meta.json"

    echo "  ${label}:"
    if [ -f "${snap}" ]; then
        local size; size=$(du -sh "${snap}" | cut -f1)

        if [ ! -f "${sidecar}" ]; then
            echo "    ⚠️  snapshot present but no sidecar (${size}) — will rebuild"
            rm -f "${snap}"
            return
        fi

        local captured_at import_ms snap_py
        captured_at=$(python3 -c "import json; d=json.load(open('${sidecar}')); print(d.get('captured_at','?'))" 2>/dev/null || echo "?")
        import_ms=$(python3   -c "import json; d=json.load(open('${sidecar}')); print(d.get('import_phase_ms','?'))" 2>/dev/null || echo "?")
        snap_py=$(python3     -c "import json; d=json.load(open('${sidecar}')); print(d.get('python_impl_version','').lower())" 2>/dev/null || echo "")

        local current_py
        current_py=$(python3 -c "import sys; v=sys.version_info; print(f'cpython {v.major}.{v.minor}.{v.micro}')")

        if [ -n "${snap_py}" ] && [ "${snap_py}" != "${current_py}" ]; then
            echo "    ⚠️  stale (snapshot: ${snap_py}, current: ${current_py}) — deleting"
            rm -f "${snap}" "${sidecar}"
        else
            echo "    ✅ valid  (${size}, built ${captured_at}, saved ${import_ms}ms)"
        fi
    else
        echo "    🔵 not yet captured — will build on first boot"
    fi
}

# ── Banner ────────────────────────────────────────────────────────────────────
echo "──────────────────────────────────────────"
echo "  PyFreeze-RS  │  Django Container"
echo "──────────────────────────────────────────"
echo "  Python      : $(python3 --version)"
echo "  Settings    : ${DJANGO_SETTINGS_MODULE:-<not set>}"
echo ""
echo "  Snapshots:"
check_snapshot "manage.py" "/snapshots/django-manage.pyfreeze"
check_snapshot "wsgi     " "/snapshots/django-wsgi.pyfreeze"
echo "──────────────────────────────────────────"

# ── Static files ──────────────────────────────────────────────────────────────
if [ "${SKIP_COLLECTSTATIC:-0}" != "1" ]; then
    echo "Collecting static files…"
    python manage.py collectstatic --noinput --clear -v 0 2>&1 | tail -3
fi

# ── Exec the server ───────────────────────────────────────────────────────────
exec "$@"

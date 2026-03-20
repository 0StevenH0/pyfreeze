#!/bin/bash
# docker/flask/entrypoint.sh
#
# Container entrypoint for the PyFreeze-RS / Flask image.
#
# Responsibilities:
#   1. Print snapshot status at startup (useful for container logs).
#   2. Validate the snapshot isn't stale before gunicorn starts.
#   3. If stale or missing, delete it so PyFreeze rebuilds it on first request.
#   4. Exec the actual server command (gunicorn / flask run / etc.).

set -euo pipefail

SNAP="${PYFREEZE_SNAPSHOT:-/snapshots/flask.pyfreeze}"
SIDECAR="${SNAP}.meta.json"

echo "──────────────────────────────────────────"
echo "  PyFreeze-RS  │  Flask Container"
echo "──────────────────────────────────────────"
echo "  Python  : $(python3 --version)"
echo "  Snapshot: ${SNAP}"

# ── Snapshot status ───────────────────────────────────────────────────────────
if [ -f "${SNAP}" ]; then
    SIZE=$(du -sh "${SNAP}" | cut -f1)

    if [ -f "${SIDECAR}" ]; then
        CAPTURED_AT=$(python3 -c "import json; d=json.load(open('${SIDECAR}')); print(d.get('captured_at','?'))" 2>/dev/null || echo "?")
        IMPORT_MS=$(python3   -c "import json; d=json.load(open('${SIDECAR}')); print(d.get('import_phase_ms','?'))" 2>/dev/null || echo "?")
        PY_VER=$(python3      -c "import json; d=json.load(open('${SIDECAR}')); print(d.get('python_impl_version','?'))" 2>/dev/null || echo "?")
        echo "  Status  : ✅ snapshot found (${SIZE})"
        echo "  Built   : ${CAPTURED_AT}"
        echo "  Saved   : ${IMPORT_MS}ms of import time"
        echo "  Py ver  : ${PY_VER}"
    else
        echo "  Status  : ⚠️  snapshot found but no sidecar (${SIZE}) — will rebuild"
        rm -f "${SNAP}"
    fi

    # Staleness check: compare Python version recorded in sidecar vs current.
    CURRENT_PY=$(python3 -c "import sys; v=sys.version_info; print(f'cpython {v.major}.{v.minor}.{v.micro}')")
    SNAP_PY=$(python3 -c "import json; d=json.load(open('${SIDECAR}')); print(d.get('python_impl_version','').lower())" 2>/dev/null || echo "")

    if [ -n "${SNAP_PY}" ] && [ "${SNAP_PY}" != "${CURRENT_PY}" ]; then
        echo "  ⚠️  Python version mismatch (snapshot: ${SNAP_PY}, current: ${CURRENT_PY})"
        echo "  Deleting stale snapshot — will rebuild on next start."
        rm -f "${SNAP}" "${SIDECAR}"
    fi
else
    echo "  Status  : 🔵 no snapshot yet — will capture on first request"
fi

echo "──────────────────────────────────────────"

# ── Exec the server ───────────────────────────────────────────────────────────
# Pass all arguments through unchanged so CMD in the Dockerfile is respected.
exec "$@"

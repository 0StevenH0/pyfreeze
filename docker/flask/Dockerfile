# docker/flask/Dockerfile
#
# Multi-stage build for a PyFreeze-RS accelerated Flask application.
#
# Stages:
#   1. rust-builder   — compiles the Rust extension (.so) + CLI binary
#   2. python-base    — installs Python deps, copies compiled artifacts
#   3. runtime        — final lean image, non-root user, snapshot volume
#
# Build:
#   docker build -f docker/flask/Dockerfile -t myapp-flask .
#
# Run (ephemeral snapshot — rebuilt every container boot):
#   docker run -p 5000:5000 myapp-flask
#
# Run (persistent snapshot across restarts — recommended):
#   docker run -p 5000:5000 -v pyfreeze_snapshots:/snapshots myapp-flask

# ── Stage 1: Rust builder ─────────────────────────────────────────────────────
FROM rust:1.78-slim-bookworm AS rust-builder

# Build-time dependencies.
RUN apt-get update && apt-get install -y --no-install-recommends \
        python3-dev \
        python3-pip \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Install maturin (builds PyO3 extensions).
RUN pip3 install maturin --break-system-packages

WORKDIR /build

# Copy only the files maturin needs to resolve dependencies first,
# so Docker can cache this layer and skip re-downloading crates on code changes.
COPY Cargo.toml pyproject.toml ./
COPY src/ ./src/
COPY python/ ./python/

# Compile the Rust extension in release mode.
# Output: target/release/pyfreeze (CLI binary)
#         target/release/libpyfreeze_rs.so  (Python extension, renamed by maturin)
RUN maturin build --release --out /dist

# ── Stage 2: Python dependency installer ─────────────────────────────────────
FROM python:3.12-slim-bookworm AS python-base

WORKDIR /app

# Install the compiled wheel produced by maturin.
COPY --from=rust-builder /dist/*.whl /tmp/
RUN pip install --no-cache-dir /tmp/*.whl

# Copy and install your Flask application's requirements.
# Replace this file with your actual requirements.
COPY examples/flask_example/requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

# Copy the compiled pyfreeze CLI binary into the PATH.
COPY --from=rust-builder /build/target/release/pyfreeze /usr/local/bin/pyfreeze
RUN chmod +x /usr/local/bin/pyfreeze

# ── Stage 3: Runtime ──────────────────────────────────────────────────────────
FROM python:3.12-slim-bookworm AS runtime

# Bring in only what's needed at runtime (no compiler, no Rust toolchain).
COPY --from=python-base /usr/local/lib/python3.12/site-packages \
                        /usr/local/lib/python3.12/site-packages
COPY --from=python-base /usr/local/bin/pyfreeze /usr/local/bin/pyfreeze

# ── Non-root user ─────────────────────────────────────────────────────────────
RUN groupadd --gid 1001 appuser \
 && useradd  --uid 1001 --gid appuser --shell /bin/bash --create-home appuser

WORKDIR /app
COPY examples/flask_example/ ./

# ── Snapshot directory ────────────────────────────────────────────────────────
# /snapshots is the canonical location for PyFreeze snapshot files in containers.
# Mount a named volume here to persist snapshots across container restarts:
#   -v pyfreeze_snapshots:/snapshots
#
# Without a volume mount the snapshot is rebuilt every cold start (still works,
# just loses the warm-start benefit on container boot).
RUN mkdir -p /snapshots && chown appuser:appuser /snapshots
VOLUME ["/snapshots"]

# Tell PyFreeze to store snapshots in /snapshots instead of ~/.cache/pyfreeze.
ENV PYFREEZE_SNAPSHOT=/snapshots/flask.pyfreeze \
    PYFREEZE_FRAMEWORK=flask \
    PYFREEZE_LOG=info \
    # Standard Flask config.
    FLASK_APP=app:create_app \
    FLASK_ENV=production \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

USER appuser

EXPOSE 5000

# Health-check: a lightweight HTTP ping that doesn't go through your full stack.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD python3 -c "import urllib.request; urllib.request.urlopen('http://localhost:5000/health')"

# ── Entrypoint ────────────────────────────────────────────────────────────────
# Use the pyfreeze CLI as the runner so that warm-start logic is applied
# before gunicorn forks workers.
COPY docker/flask/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
CMD ["gunicorn", "--bind", "0.0.0.0:5000", "--workers", "2", "app:create_app()"]

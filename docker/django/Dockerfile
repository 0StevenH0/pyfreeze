# docker/django/Dockerfile
#
# Multi-stage build for a PyFreeze-RS accelerated Django application.
#
# Stages:
#   1. rust-builder   — compiles Rust extension + CLI binary
#   2. python-base    — installs Python deps, copies compiled artifacts
#   3. runtime        — final lean image, non-root user, snapshot volume
#
# Build:
#   docker build -f docker/django/Dockerfile -t myapp-django .
#
# Run (web server):
#   docker run -p 8000:8000 \
#     -v pyfreeze_snapshots:/snapshots \
#     -e DJANGO_SETTINGS_MODULE=myproject.settings.production \
#     myapp-django
#
# Run management commands:
#   docker run --rm \
#     -v pyfreeze_snapshots:/snapshots \
#     -e DJANGO_SETTINGS_MODULE=myproject.settings.production \
#     myapp-django python manage.py migrate

# ── Stage 1: Rust builder ─────────────────────────────────────────────────────
FROM rust:1.78-slim-bookworm AS rust-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        python3-dev \
        python3-pip \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN pip3 install maturin --break-system-packages

WORKDIR /build

COPY Cargo.toml pyproject.toml ./
COPY src/ ./src/
COPY python/ ./python/

# Compile in release mode. The wheel ends up in /dist/.
RUN maturin build --release --out /dist

# ── Stage 2: Python dependency installer ─────────────────────────────────────
FROM python:3.12-slim-bookworm AS python-base

WORKDIR /app

# System libs Django may need (psycopg2-binary, Pillow, etc.)
RUN apt-get update && apt-get install -y --no-install-recommends \
        libpq5 \
        libpq-dev \
        gcc \
    && rm -rf /var/lib/apt/lists/*

# Install the compiled pyfreeze wheel.
COPY --from=rust-builder /dist/*.whl /tmp/
RUN pip install --no-cache-dir /tmp/*.whl

# Install the Django project's requirements.
# Replace this with your actual requirements file.
COPY examples/django_example/requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

# Copy the CLI binary.
COPY --from=rust-builder /build/target/release/pyfreeze /usr/local/bin/pyfreeze
RUN chmod +x /usr/local/bin/pyfreeze

# ── Stage 3: Runtime ──────────────────────────────────────────────────────────
FROM python:3.12-slim-bookworm AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        libpq5 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=python-base /usr/local/lib/python3.12/site-packages \
                        /usr/local/lib/python3.12/site-packages
COPY --from=python-base /usr/local/bin/pyfreeze /usr/local/bin/pyfreeze

# ── Non-root user ─────────────────────────────────────────────────────────────
RUN groupadd --gid 1001 appuser \
 && useradd  --uid 1001 --gid appuser --shell /bin/bash --create-home appuser

WORKDIR /app
COPY examples/django_example/ ./

# ── Snapshot directory ────────────────────────────────────────────────────────
# Mount a named volume at /snapshots for persistence across restarts.
#
# Django uses TWO snapshots:
#   /snapshots/django-manage.pyfreeze  — for manage.py commands (runserver etc.)
#   /snapshots/django-wsgi.pyfreeze    — for WSGI workers (gunicorn)
#
# They are separate because manage.py and WSGI may register different signals
# and middleware depending on the entry point.
RUN mkdir -p /snapshots && chown appuser:appuser /snapshots
VOLUME ["/snapshots"]

ENV PYFREEZE_FRAMEWORK=django \
    PYFREEZE_LOG=info \
    # Django-specific envs — override at runtime.
    DJANGO_SETTINGS_MODULE=myproject.settings \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    # Static files directory.
    DJANGO_STATIC_ROOT=/app/staticfiles

USER appuser

EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD python3 -c "import urllib.request; urllib.request.urlopen('http://localhost:8000/health/')"

COPY docker/django/entrypoint.sh /entrypoint.sh
COPY docker/django/manage-wrapper.sh /usr/local/bin/manage
RUN chmod +x /entrypoint.sh /usr/local/bin/manage

ENTRYPOINT ["/entrypoint.sh"]
# Default: run the WSGI server.
# Override with "python manage.py <command>" for management tasks.
CMD ["gunicorn", \
     "--bind", "0.0.0.0:8000", \
     "--workers", "2", \
     "--timeout", "120", \
     "myproject.wsgi:application"]

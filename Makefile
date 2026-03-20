.PHONY: help build-flask build-django build-all \
        up down logs shell-flask shell-django \
        migrate collectstatic \
        snapshot-info-flask snapshot-info-django \
        rebuild-snapshot-flask rebuild-snapshot-django \
        test-python test-rust test-all

# ── Defaults ──────────────────────────────────────────────────────────────────
COMPOSE = docker compose
FLASK_SERVICE  = flask
DJANGO_SERVICE = django

# ── Help ─────────────────────────────────────────────────────────────────────
help:
	@echo ""
	@echo "  PyFreeze-RS  —  Docker targets"
	@echo "  ──────────────────────────────────────────────────────────────"
	@echo "  Build"
	@echo "    build-flask           Build the Flask image"
	@echo "    build-django          Build the Django image"
	@echo "    build-all             Build both images"
	@echo ""
	@echo "  Run"
	@echo "    up                    Start all services (detached)"
	@echo "    down                  Stop and remove containers"
	@echo "    logs                  Tail logs for all services"
	@echo "    logs-flask            Tail Flask logs only"
	@echo "    logs-django           Tail Django logs only"
	@echo ""
	@echo "  Shells"
	@echo "    shell-flask           Bash shell inside the Flask container"
	@echo "    shell-django          Bash shell inside the Django container"
	@echo ""
	@echo "  Django management"
	@echo "    migrate               Run Django migrations"
	@echo "    collectstatic         Collect static files"
	@echo ""
	@echo "  Snapshots"
	@echo "    snapshot-info-flask   Show Flask snapshot metadata"
	@echo "    snapshot-info-django  Show Django snapshot metadata"
	@echo "    rebuild-snapshot-flask   Force Flask snapshot rebuild"
	@echo "    rebuild-snapshot-django  Force Django snapshot rebuild"
	@echo ""
	@echo "  Tests"
	@echo "    test-python           Run Python test suite (no Docker needed)"
	@echo "    test-rust             Run Rust test suite"
	@echo "    test-all              Run both test suites"
	@echo ""

# ── Build ─────────────────────────────────────────────────────────────────────
build-flask:
	$(COMPOSE) build $(FLASK_SERVICE)

build-django:
	$(COMPOSE) build $(DJANGO_SERVICE)

build-all:
	$(COMPOSE) build

# ── Run ───────────────────────────────────────────────────────────────────────
up:
	$(COMPOSE) up -d

down:
	$(COMPOSE) down

logs:
	$(COMPOSE) logs -f

logs-flask:
	$(COMPOSE) logs -f $(FLASK_SERVICE)

logs-django:
	$(COMPOSE) logs -f $(DJANGO_SERVICE)

# ── Shells ────────────────────────────────────────────────────────────────────
shell-flask:
	$(COMPOSE) exec $(FLASK_SERVICE) bash

shell-django:
	$(COMPOSE) exec $(DJANGO_SERVICE) bash

# ── Django management commands ────────────────────────────────────────────────
migrate:
	$(COMPOSE) run --rm $(DJANGO_SERVICE) python manage.py migrate

collectstatic:
	$(COMPOSE) run --rm \
	    -e SKIP_COLLECTSTATIC=0 \
	    $(DJANGO_SERVICE) python manage.py collectstatic --noinput

# ── Snapshot management ───────────────────────────────────────────────────────
snapshot-info-flask:
	@$(COMPOSE) exec $(FLASK_SERVICE) \
	    python3 -c "from pyfreeze._snapshot_info import format_info; print(format_info('/snapshots/flask.pyfreeze'))"

snapshot-info-django:
	@$(COMPOSE) exec $(DJANGO_SERVICE) \
	    python3 -c "from pyfreeze._snapshot_info import format_info; \
	               print(format_info('/snapshots/django-wsgi.pyfreeze'))"

rebuild-snapshot-flask:
	@echo "Invalidating Flask snapshot…"
	$(COMPOSE) exec $(FLASK_SERVICE) \
	    pyfreeze invalidate /snapshots/flask.pyfreeze 2>/dev/null || true
	@echo "Restarting Flask container to trigger rebuild…"
	$(COMPOSE) restart $(FLASK_SERVICE)

rebuild-snapshot-django:
	@echo "Invalidating Django snapshots…"
	$(COMPOSE) exec $(DJANGO_SERVICE) \
	    sh -c 'rm -f /snapshots/django-*.pyfreeze /snapshots/django-*.pyfreeze.meta.json' 2>/dev/null || true
	@echo "Restarting Django container to trigger rebuild…"
	$(COMPOSE) restart $(DJANGO_SERVICE)

# ── Tests ─────────────────────────────────────────────────────────────────────
test-python:
	python -m pytest tests/python/ -v

test-rust:
	cargo test

test-all: test-python test-rust

# Standard Flask application factory pattern with PyFreeze baked in.
# The only additions are the lines marked ← PyFreeze.

# ── Option A: one-liner before Flask imports (patch_flask) ──────────────────
# import pyfreeze.flask_plugin as _pf; _pf.patch_flask()   # ← PyFreeze

# ── Option B: explicit freeze_app() inside factory (recommended) ─────────────
from pyfreeze.flask_plugin import freeze_app   # ← PyFreeze

from flask import Flask
from flask_sqlalchemy import SQLAlchemy

db = SQLAlchemy()


def create_app(config_object: str = "config.DevelopmentConfig") -> Flask:
    """Application factory."""
    app = Flask(__name__)
    app.config.from_object(config_object)

    # ── Extensions ──────────────────────────────────────────────────────────
    db.init_app(app)

    # ── Blueprints ──────────────────────────────────────────────────────────
    from .blueprints.api import api_bp
    from .blueprints.auth import auth_bp
    app.register_blueprint(api_bp,  url_prefix="/api")
    app.register_blueprint(auth_bp, url_prefix="/auth")

    # ── PyFreeze: capture after everything is registered ────────────────────
    freeze_app(app)   # ← PyFreeze (one line)
    # ────────────────────────────────────────────────────────────────────────

    return app


# ── WSGI entry point for gunicorn / uWSGI ────────────────────────────────────
# gunicorn "app:application"
application = create_app()

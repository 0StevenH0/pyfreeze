#!/usr/bin/env python
# examples/django_example/manage.py
#
# Drop-in replacement for the standard Django manage.py.
# The only addition is the two lines marked ← PyFreeze.
#
# Cold start  (first run):   builds snapshot, runs normally.
# Warm start  (later runs):  loads snapshot, skips import phase.
#
# Environment variables:
#   DJANGO_SETTINGS_MODULE — as usual
#   PYFREEZE_LOG=info      — see capture/restore timings
#   PYFREEZE_SNAPSHOT=…    — override snapshot path

"""Django's command-line utility for administrative tasks."""

# ── PyFreeze: must be the very first import ─────────────────────────────────
import pyfreeze.django_plugin as _pf   # ← PyFreeze (1/2)
_pf.patch_manage()                     # ← PyFreeze (2/2)
# ─────────────────────────────────────────────────────────────────────────────

import os
import sys


def main():
    """Run administrative tasks."""
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "myproject.settings")
    try:
        from django.core.management import execute_from_command_line
    except ImportError as exc:
        raise ImportError(
            "Couldn't import Django. Are you sure it's installed and "
            "available on your PYTHONPATH environment variable? Did you "
            "forget to activate a virtual environment?"
        ) from exc
    execute_from_command_line(sys.argv)


if __name__ == "__main__":
    main()

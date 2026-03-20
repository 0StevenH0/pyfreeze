#!/bin/bash
# docker/django/manage-wrapper.sh
#
# Drop-in replacement for `python manage.py` inside the container.
# Sets the correct snapshot path for management commands (separate from WSGI).
#
# Usage inside container:
#   manage migrate
#   manage createsuperuser
#   manage shell
#
# Or via docker run:
#   docker run --rm myapp-django manage migrate

exec python manage.py \
    --pyfreeze-snapshot /snapshots/django-manage.pyfreeze \
    "$@"

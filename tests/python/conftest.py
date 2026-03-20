# Shared pytest fixtures and configuration.

import sys
from pathlib import Path

import pytest

# Ensure the package root is on sys.path so tests can import pyfreeze
# without a full pip install.
ROOT = Path(__file__).parent.parent.parent
sys.path.insert(0, str(ROOT / "python"))


@pytest.fixture(autouse=True)
def clean_env(monkeypatch):
    """
    Remove PyFreeze env vars before each test so they don't bleed across tests.
    """
    monkeypatch.delenv("PYFREEZE_SNAPSHOT",  raising=False)
    monkeypatch.delenv("PYFREEZE_FRAMEWORK", raising=False)
    monkeypatch.delenv("PYFREEZE_LOG",       raising=False)


@pytest.fixture(autouse=True)
def clean_meta_path():
    """
    Remove any PyFreeze hooks that a test may have left in sys.meta_path.
    """
    from pyfreeze import _PostImportCapture

    original = sys.meta_path[:]
    yield
    # Remove any hook objects left behind.
    sys.meta_path[:] = [
        h for h in sys.meta_path
        if not isinstance(h, _PostImportCapture)
    ]

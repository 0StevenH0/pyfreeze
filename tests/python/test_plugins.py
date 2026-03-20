# tests/python/test_plugins.py
#
# Tests for the plugin registry — runs without the Rust extension.

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent.parent / "python"))

from pyfreeze.plugins import Plugin, PluginRegistry, NumpyPlugin, PillowPlugin


# ─── Registry basics ─────────────────────────────────────────────────────────

def test_register_and_find():
    reg = PluginRegistry()

    class DummyPlugin:
        plugin_id = "dummy"
        def can_handle(self, name): return name == "dummy_module"
        def capture(self, m): return {}
        def restore(self, m, s): pass

    reg.register(DummyPlugin())
    assert reg.find("dummy_module") is not None
    assert reg.find("other_module") is None


def test_get_by_id():
    reg = PluginRegistry()

    class FooPlugin:
        plugin_id = "foo"
        def can_handle(self, name): return False
        def capture(self, m): return {}
        def restore(self, m, s): pass

    reg.register(FooPlugin())
    assert reg.get("foo") is not None
    assert reg.get("bar") is None


def test_plugin_protocol_conformance():
    """NumpyPlugin and PillowPlugin must conform to the Plugin Protocol."""
    assert isinstance(NumpyPlugin(), Plugin)
    assert isinstance(PillowPlugin(), Plugin)


# ─── NumpyPlugin ─────────────────────────────────────────────────────────────

numpy = pytest.importorskip("numpy")

def test_numpy_plugin_handles_correct_modules():
    p = NumpyPlugin()
    assert p.can_handle("numpy")
    assert p.can_handle("numpy.core")
    assert not p.can_handle("pandas")


def test_numpy_capture_restore_roundtrip():
    import types, numpy as np

    p = NumpyPlugin()

    # Fake module with an ndarray attribute.
    mod = types.SimpleNamespace(
        MY_ARRAY = np.array([1.0, 2.0, 3.0]),
        NOT_ARRAY = "just a string",
    )

    state = p.capture(mod)
    assert "MY_ARRAY" in state["arrays"]
    assert "NOT_ARRAY" not in state["arrays"]

    # Restore into a fresh namespace.
    restored = types.SimpleNamespace()
    p.restore(restored, state)

    np.testing.assert_array_equal(restored.MY_ARRAY, mod.MY_ARRAY)


def test_numpy_2d_array():
    import types, numpy as np

    p = NumpyPlugin()
    mod = types.SimpleNamespace(matrix=np.arange(12, dtype=np.float32).reshape(3, 4))
    state = p.capture(mod)
    out   = types.SimpleNamespace()
    p.restore(out, state)
    np.testing.assert_array_equal(out.matrix, mod.matrix)


# ─── PillowPlugin ────────────────────────────────────────────────────────────

PIL = pytest.importorskip("PIL")

def test_pillow_plugin_handles_correct_modules():
    p = PillowPlugin()
    assert p.can_handle("PIL")
    assert p.can_handle("PIL.Image")
    assert not p.can_handle("cv2")


def test_pillow_capture_restore_roundtrip():
    import types
    from PIL import Image
    import numpy as np

    p = PillowPlugin()

    original_img = Image.fromarray(
        np.zeros((4, 4, 3), dtype=np.uint8), mode="RGB"
    )
    mod   = types.SimpleNamespace(LOGO=original_img, NAME="not an image")
    state = p.capture(mod)

    assert "LOGO"  in state["images"]
    assert "NAME" not in state["images"]

    out = types.SimpleNamespace()
    p.restore(out, state)

    assert out.LOGO.size  == original_img.size
    assert out.LOGO.mode  == original_img.mode


# ─── Edge cases ───────────────────────────────────────────────────────────────

def test_numpy_empty_module_produces_empty_state():
    import types, numpy as np

    p   = NumpyPlugin()
    mod = types.SimpleNamespace(x=42, s="hello")   # no arrays
    state = p.capture(mod)
    assert state == {"arrays": {}}


def test_restore_with_empty_state_is_noop():
    import types
    p   = NumpyPlugin()
    mod = types.SimpleNamespace()
    p.restore(mod, {"arrays": {}})   # must not raise

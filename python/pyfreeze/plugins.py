# Plugin registry for libraries that need custom capture/restore logic.
#
# A plugin is any object that implements the Protocol below.  Plugins are
# discovered automatically (via entry_points) or registered manually.
#
# Built-in plugins:
#   numpy    — captures ndarray buffers via __array_interface__
#   PIL      — captures image data via tobytes() / frombytes()
#
# Third-party plugins register themselves via the entry point group
# "pyfreeze.plugins", e.g. in their own setup.cfg:
#
#   [options.entry_points]
#   pyfreeze.plugins =
#       torch = pyfreeze_torch:TorchPlugin
#
# Plugin selection happens inside graph_walker (Rust) when it encounters a
# module whose type is registered here.  The plugin ID string is written
# into the snapshot's ModuleEntry.capture_strategy.plugin_id field.

from __future__ import annotations

import importlib
import logging
from typing import Any, Protocol, runtime_checkable

log = logging.getLogger("pyfreeze.plugins")


# ─── Plugin Protocol ─────────────────────────────────────────────────────────

@runtime_checkable
class Plugin(Protocol):
    """
    Interface every PyFreeze plugin must implement.

    ``plugin_id``  — short unique string written into the snapshot.
    ``capture()``  — called during freeze; must return a JSON-serializable dict.
    ``restore()``  — called during thaw; receives the dict from capture().
    """

    plugin_id: str

    def can_handle(self, module_name: str) -> bool:
        """Return True if this plugin knows how to handle *module_name*."""
        ...

    def capture(self, module: Any) -> dict[str, Any]:
        """Serialize the module's state to a JSON-safe dict."""
        ...

    def restore(self, module: Any, state: dict[str, Any]) -> None:
        """Restore the module's state from the dict returned by capture()."""
        ...


# ─── Registry ────────────────────────────────────────────────────────────────

class PluginRegistry:
    def __init__(self) -> None:
        self._plugins: dict[str, Plugin] = {}

    def register(self, plugin: Plugin) -> None:
        self._plugins[plugin.plugin_id] = plugin
        log.debug("registered plugin '%s'", plugin.plugin_id)

    def find(self, module_name: str) -> Plugin | None:
        for plugin in self._plugins.values():
            if plugin.can_handle(module_name):
                return plugin
        return None

    def get(self, plugin_id: str) -> Plugin | None:
        return self._plugins.get(plugin_id)

    def load_entry_points(self) -> None:
        """Discover and load plugins declared via setuptools entry_points."""
        try:
            from importlib.metadata import entry_points
            eps = entry_points(group="pyfreeze.plugins")
        except Exception:
            return

        for ep in eps:
            try:
                cls = ep.load()
                self.register(cls())
                log.info("loaded plugin '%s' from entry_point '%s'", cls.plugin_id, ep.name)
            except Exception as exc:
                log.warning("failed to load plugin '%s': %s", ep.name, exc)

    def __repr__(self) -> str:
        return f"PluginRegistry({list(self._plugins)})"


# ─── Global registry instance ────────────────────────────────────────────────

registry = PluginRegistry()


# ─── Built-in: NumPy ─────────────────────────────────────────────────────────

class NumpyPlugin:
    """
    Captures ndarray objects by serializing their raw data buffers using
    NumPy's own .npy format (via numpy.save / numpy.load).

    This avoids pickle entirely for array data, which is both faster and
    more robust across NumPy versions.
    """

    plugin_id = "numpy"

    def can_handle(self, module_name: str) -> bool:
        return module_name in ("numpy", "numpy.core", "numpy.core.multiarray")

    def capture(self, module: Any) -> dict[str, Any]:
        import io, numpy as np

        arrays: dict[str, bytes] = {}
        for name, obj in vars(module).items():
            if isinstance(obj, np.ndarray):
                buf = io.BytesIO()
                np.save(buf, obj, allow_pickle=False)
                arrays[name] = buf.getvalue().hex()  # hex for JSON safety

        return {"arrays": arrays}

    def restore(self, module: Any, state: dict[str, Any]) -> None:
        import io, numpy as np

        for name, hex_data in state.get("arrays", {}).items():
            buf = io.BytesIO(bytes.fromhex(hex_data))
            arr = np.load(buf, allow_pickle=False)
            setattr(module, name, arr)
            log.debug("numpy: restored array '%s' shape=%s dtype=%s", name, arr.shape, arr.dtype)


# ─── Built-in: Pillow ────────────────────────────────────────────────────────

class PillowPlugin:
    """Captures PIL.Image objects as PNG byte blobs."""

    plugin_id = "pillow"

    def can_handle(self, module_name: str) -> bool:
        return module_name in ("PIL", "PIL.Image")

    def capture(self, module: Any) -> dict[str, Any]:
        import io

        try:
            Image = importlib.import_module("PIL.Image")
        except ImportError:
            return {}

        images: dict[str, str] = {}
        for name, obj in vars(module).items():
            if isinstance(obj, Image.Image):
                buf = io.BytesIO()
                obj.save(buf, format="PNG")
                images[name] = buf.getvalue().hex()

        return {"images": images}

    def restore(self, module: Any, state: dict[str, Any]) -> None:
        import io

        try:
            Image = importlib.import_module("PIL.Image")
        except ImportError:
            return

        for name, hex_data in state.get("images", {}).items():
            buf = io.BytesIO(bytes.fromhex(hex_data))
            img = Image.open(buf)
            img.load()  # force decode
            setattr(module, name, img)
            log.debug("pillow: restored image '%s' size=%s", name, img.size)


# ─── Register built-ins ───────────────────────────────────────────────────────

def _register_builtins() -> None:
    registry.register(NumpyPlugin())
    registry.register(PillowPlugin())
    registry.load_entry_points()


_register_builtins()

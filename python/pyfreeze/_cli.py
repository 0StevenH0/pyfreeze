# Pure-Python CLI entry point installed via pyproject.toml `[project.scripts]`.
# This delegates to the compiled `pyfreeze` Rust binary when it exists,
# and falls back to a helpful error message when it doesn't.
#
# This module is kept intentionally thin — all real logic lives in the Rust
# binary (src/main.rs).  Having a Python shim means `pip install pyfreeze-rs`
# gives the user a working `pyfreeze` command on PATH even before they've
# compiled the binary themselves.

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path


def _find_rust_binary() -> Path | None:
    """
    Search for the compiled `pyfreeze` binary in order of preference:

    1. Next to this file (editable / maturin develop installs).
    2. Standard script directories on PATH.
    3. A `target/release/pyfreeze` relative to the repo root (cargo build).
    """
    candidates = [
        # Editable install: binary sits next to the Python package.
        Path(__file__).parent.parent / "pyfreeze",
        Path(__file__).parent.parent / "pyfreeze.exe",  # Windows
        # Cargo release build.
        Path(__file__).parent.parent.parent / "target" / "release" / "pyfreeze",
        Path(__file__).parent.parent.parent / "target" / "release" / "pyfreeze.exe",
    ]

    for path in candidates:
        if path.is_file() and os.access(path, os.X_OK):
            return path

    # Fall back to PATH.
    found = shutil.which("pyfreeze")
    return Path(found) if found else None


def main() -> None:
    binary = _find_rust_binary()

    if binary is None:
        _print_build_instructions()
        sys.exit(1)

    # Replace the current process with the Rust binary.
    # On Unix this is a true exec(); on Windows it's a subprocess wait.
    if sys.platform == "win32":
        result = subprocess.run([str(binary)] + sys.argv[1:])
        sys.exit(result.returncode)
    else:
        os.execv(str(binary), [str(binary)] + sys.argv[1:])


def _print_build_instructions() -> None:
    print(
        "PyFreeze-RS: the native Rust binary is not compiled yet.\n"
        "\n"
        "Build it with one of:\n"
        "\n"
        "  # Recommended (installs both the Rust extension + Python package)\n"
        "  maturin develop --release\n"
        "\n"
        "  # Or just the binary\n"
        "  cargo build --release\n"
        "\n"
        "See README.md for full setup instructions.",
        file=sys.stderr,
    )

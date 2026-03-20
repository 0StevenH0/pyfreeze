# PyFreeze-RS

**Dormant Process Resumption for Python** — eliminates import-phase startup latency for Django and Flask by serializing interpreter state to disk after the first run.

```
Cold start  (first run):  380ms   ← runs normally, captures snapshot
Warm start  (later runs):  22ms   ← loads snapshot, skips imports entirely
```

---

## How it works

Instead of repeating the same expensive imports every process boot:

```
Disk → interpreter → import django → import sqlalchemy → import … → main()
```

PyFreeze-RS captures the interpreter state *after* imports complete and stores it as a binary snapshot. Subsequent runs memory-map the snapshot and jump straight to `main()`.

The core engine is written in Rust (via PyO3) and uses **semantic graph-walking** rather than raw memory dumps, making it immune to ASLR and portable across Python patch versions.

---

## Quick start

### Prerequisites

- Python 3.10+
- Rust toolchain (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- `maturin` (`pip install maturin`)

### Build

```bash
git clone https://github.com/you/pyfreeze-rs
cd pyfreeze-rs
maturin develop --release   # compiles Rust + installs Python package
```

### Django

```python
# manage.py  (top of file, before everything else)
import pyfreeze.django_plugin as _pf
_pf.patch_manage()

# ... rest of manage.py unchanged
```

```python
# wsgi.py
import pyfreeze.django_plugin as _pf
_pf.patch_wsgi()

from django.core.wsgi import get_wsgi_application
application = get_wsgi_application()
```

### Flask

```python
# app.py
from pyfreeze.flask_plugin import freeze_app

def create_app():
    app = Flask(__name__)
    # ... configure, register blueprints ...
    freeze_app(app)   # ← one line at the end
    return app
```

### CLI

```bash
# Run with snapshot acceleration (auto-detected framework)
pyfreeze run manage.py runserver

# Inspect a snapshot
pyfreeze info ~/.cache/pyfreeze/abc123.pyfreeze

# Force rebuild
pyfreeze run --rebuild manage.py runserver

# Benchmark cold vs warm
pyfreeze benchmark manage.py check
```

---

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `PYFREEZE_SNAPSHOT` | auto-derived | Override snapshot file path |
| `PYFREEZE_FRAMEWORK` | auto-detected | `django` \| `flask` \| `generic` |
| `PYFREEZE_LOG` | `warn` | Log level (`debug` \| `info` \| `warn` \| `error`) |

---

## Architecture

```
pyfreeze/
├── src/
│   ├── capture/
│   │   ├── graph_walker.rs   # walks sys.modules, pickles __dict__s
│   │   └── fd_capture.rs     # snapshots open file descriptors
│   ├── snapshot/
│   │   ├── format.rs         # binary on-disk format
│   │   ├── hash.rs           # staleness detection (SHA-256 of .py files)
│   │   └── metadata.rs       # version + compatibility checks
│   ├── loader/mod.rs         # restores snapshot into a fresh interpreter
│   ├── lib.rs                # PyO3 extension module (pyfreeze_rs)
│   └── main.rs               # pyfreeze CLI binary
└── python/pyfreeze/
    ├── __init__.py           # bootstrap() and CaptureContext APIs
    ├── django_plugin.py      # patch_manage() / patch_wsgi()
    └── flask_plugin.py       # freeze_app() / patch_flask() / PyFreeze extension
```

### Snapshot format

```
┌──────────────────────────────────────────────────────────┐
│  MAGIC (8 B)  │  FORMAT_VER (4 B)  │  HEADER_LEN (4 B)  │
├──────────────────────────────────────────────────────────┤
│  SnapshotHeader  (bincode)                               │
│    └─ SnapshotMetadata  (source_hash, python_ver, …)    │
├──────────────────────────────────────────────────────────┤
│  ModuleTable  (bincode)                                  │
│    ├─ ModuleEntry { name, strategy, blob_offset, … }    │
│    └─ …                                                  │
├──────────────────────────────────────────────────────────┤
│  FdTable  (bincode)                                      │
│    ├─ FdEntry { fd, kind: RegularFile | TcpSocket | … } │
│    └─ …                                                  │
├──────────────────────────────────────────────────────────┤
│  Pickle blobs  (raw bytes, indexed by ModuleEntry)       │
└──────────────────────────────────────────────────────────┘
```

---

## Limitations (v0.1)

- **Import-phase only** — mid-execution snapshots (stack frame capture) are Phase 2.
- **C extensions with opaque internal state** — falls back to re-import; no data loss.
- **GPU tensors** — PyTorch plugin planned for v0.2.
- **Windows** — FD capture stubs present; not yet fully tested.

---

## License

MIT

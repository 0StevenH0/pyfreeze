// src/snapshot/format.rs
//
// On-disk layout of a PyFreeze snapshot.
//
// ┌───────────────────────────────────────────────────────────┐
// │  MAGIC (8 bytes)  │  VERSION (4 bytes)  │  HEADER_LEN (4)│
// ├───────────────────────────────────────────────────────────┤
// │  SnapshotHeader  (bincode, fixed after HEADER_LEN)        │
// ├───────────────────────────────────────────────────────────┤
// │  ModuleTable     (bincode)                                │
// ├───────────────────────────────────────────────────────────┤
// │  FdTable         (bincode)                                │
// ├───────────────────────────────────────────────────────────┤
// │  PickleBlobs     (raw bytes, indexed by ModuleEntry)      │
// └───────────────────────────────────────────────────────────┘

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Magic bytes written at the start of every snapshot file.
pub const MAGIC: &[u8; 8] = b"PYFRZ\x00\x01\x00";

/// Bump this whenever the on-disk format changes in a breaking way.
pub const FORMAT_VERSION: u32 = 1;

// ─── Top-level header ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub metadata:   SnapshotMetadata,
    /// Byte offset of `ModuleTable` from the start of the file.
    pub module_table_offset: u64,
    /// Byte offset of `FdTable` from the start of the file.
    pub fd_table_offset:     u64,
    /// Byte offset of the raw pickle blob section.
    pub pickle_blob_offset:  u64,
}

// ─── Metadata ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    /// SHA-256 hex digest of every `.py` / `.pyc` file that was imported.
    /// If any file on disk changes, this won't match → snapshot is stale.
    pub source_hash: String,

    /// e.g. "CPython 3.12.3"
    pub python_impl_version: String,

    /// e.g. "x86_64-linux" | "aarch64-linux" | "x86_64-windows"
    pub target_triple: String,

    /// When the snapshot was captured (ISO-8601 UTC).
    pub captured_at: String,

    /// Which framework triggered the capture ("django" | "flask" | "generic").
    pub framework: String,

    /// Total wall-clock time of the import phase that was snapshotted, in ms.
    pub import_phase_ms: u64,
}

// ─── Module table ─────────────────────────────────────────────────────────────

/// Describes the serialized state of one Python module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleEntry {
    /// Fully-qualified module name, e.g. "django.db.models".
    pub name: String,

    /// How the module's `__dict__` was captured.
    pub capture_strategy: CaptureStrategy,

    /// Byte offset inside the pickle blob section (only set when
    /// strategy == PickledDict or PickledObject).
    pub blob_offset: Option<u64>,

    /// Length of the pickle blob in bytes.
    pub blob_len: Option<u64>,

    /// Absolute path on disk at capture time (used for staleness check).
    pub source_path: Option<String>,

    /// SHA-256 of the module's source file at capture time.
    pub source_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CaptureStrategy {
    /// Module `__dict__` was successfully serialized with pickle.
    PickledDict,

    /// The entire module object was pickled (e.g. for `.pyd` / `.so` shims).
    PickledObject,

    /// Module has no serializable state — simply re-import it from disk.
    /// This is fine for C extensions whose `import` side-effects are idempotent.
    ReImport,

    /// Module contains GPU tensors or other opaque buffers; use plugin to handle.
    Plugin { plugin_id: String },

    /// We couldn't capture this module safely; log a warning and skip it.
    Skipped { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleTable {
    /// Ordered list (preserving import order, which matters for some frameworks).
    pub entries: Vec<ModuleEntry>,

    /// Maps module name → index in `entries` for O(1) lookup.
    pub name_index: HashMap<String, usize>,
}

impl ModuleTable {
    pub fn new() -> Self {
        Self { entries: Vec::new(), name_index: HashMap::new() }
    }

    pub fn add(&mut self, entry: ModuleEntry) {
        let idx = self.entries.len();
        self.name_index.insert(entry.name.clone(), idx);
        self.entries.push(entry);
    }

    pub fn get(&self, name: &str) -> Option<&ModuleEntry> {
        self.name_index.get(name).map(|&i| &self.entries[i])
    }
}

// ─── File-descriptor table ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FdTable {
    pub entries: Vec<FdEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FdEntry {
    /// The file-descriptor integer (e.g. 3, 4, 5 …).
    pub fd: i32,

    /// How to reconstruct this FD after restoring.
    pub kind: FdKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FdKind {
    /// A regular file that can be re-opened at a known offset.
    RegularFile {
        path:   String,
        offset: u64,
        flags:  i32,   // O_RDONLY | O_WRONLY | O_RDWR etc.
        mode:   u32,
    },

    /// A lazy-reconnect proxy.  On first use after restore, PyFreeze
    /// will call into the framework's connection factory.
    TcpSocket {
        peer_addr: String,   // "host:port"
        is_tls:    bool,
    },

    /// Unix-domain socket (e.g. Postgres via /var/run/postgresql).
    UnixSocket { path: String },

    /// We drained the pipe and buffered the bytes.
    Pipe { buffered_data: Vec<u8> },

    /// stdin / stdout / stderr — always skip, they're inherited from the shell.
    Stdio,

    /// Anything else we can't handle; log and close.
    Unknown { description: String },
}

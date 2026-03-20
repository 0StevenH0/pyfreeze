// src/snapshot/mod.rs
//
// High-level API for reading and writing snapshot files.
// Lower-level details live in format.rs, hash.rs, and metadata.rs.

pub mod format;
pub mod hash;
pub mod metadata;

use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use memmap2::MmapOptions;
use tracing::{debug, info};

use crate::error::{FreezeError, Result};
use format::{FdTable, ModuleTable, SnapshotHeader, FORMAT_VERSION, MAGIC};

// ─── Default snapshot directory ───────────────────────────────────────────────

/// Returns `$XDG_CACHE_HOME/pyfreeze` (Linux/macOS) or `%LOCALAPPDATA%\pyfreeze` (Windows).
pub fn default_cache_dir() -> PathBuf {
    #[cfg(unix)]
    {
        let base = std::env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                PathBuf::from(home).join(".cache")
            });
        base.join("pyfreeze")
    }
    #[cfg(windows)]
    {
        let base = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:\\Temp"));
        base.join("pyfreeze")
    }
}

// ─── SnapshotWriter ────────────────────────────────────────────────────────────

/// Writes a snapshot to disk atomically (write to `.tmp`, then rename).
pub struct SnapshotWriter {
    path:         PathBuf,
    module_table: ModuleTable,
    fd_table:     FdTable,
    pickle_blobs: Vec<u8>,   // raw bytes; ModuleEntry offsets point into here
    header:       SnapshotHeader,
}

impl SnapshotWriter {
    pub fn new(path: impl Into<PathBuf>, header: SnapshotHeader) -> Self {
        Self {
            path: path.into(),
            module_table: ModuleTable::new(),
            fd_table: FdTable { entries: Vec::new() },
            pickle_blobs: Vec::new(),
            header,
        }
    }

    /// Append a raw pickle blob and return its (offset, len) within the blob section.
    pub fn add_pickle_blob(&mut self, data: &[u8]) -> (u64, u64) {
        let offset = self.pickle_blobs.len() as u64;
        self.pickle_blobs.extend_from_slice(data);
        (offset, data.len() as u64)
    }

    pub fn set_module_table(&mut self, table: ModuleTable) {
        self.module_table = table;
    }

    pub fn set_fd_table(&mut self, table: FdTable) {
        self.fd_table = table;
    }

    /// Flush everything to disk.  Writes to a `.tmp` file first, then renames
    /// for atomicity — no half-written snapshots.
    pub fn commit(mut self) -> Result<PathBuf> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let tmp_path = self.path.with_extension("pyfreeze.tmp");
        let mut file = std::fs::File::create(&tmp_path)?;

        // 1. Magic + format version
        file.write_all(MAGIC)?;
        file.write_all(&FORMAT_VERSION.to_le_bytes())?;

        // 2. Serialise tables so we know their sizes.
        let module_bytes = bincode::serialize(&self.module_table)?;
        let fd_bytes     = bincode::serialize(&self.fd_table)?;
        let header_bytes = bincode::serialize(&self.header)?;

        // 3. Write header-length prefix then header.
        let header_len = header_bytes.len() as u32;
        file.write_all(&header_len.to_le_bytes())?;
        file.write_all(&header_bytes)?;

        // 4. Module table.
        let module_table_offset = MAGIC.len() as u64 + 4 + 4 + header_bytes.len() as u64;
        file.write_all(&module_bytes)?;

        // 5. Fd table.
        let fd_table_offset = module_table_offset + module_bytes.len() as u64;
        file.write_all(&fd_bytes)?;

        // 6. Pickle blobs.
        let pickle_blob_offset = fd_table_offset + fd_bytes.len() as u64;
        file.write_all(&self.pickle_blobs)?;

        // 7. Patch offsets back into the header and re-write it.
        //    (Simple approach: seek back and overwrite header.)
        self.header.module_table_offset = module_table_offset;
        self.header.fd_table_offset     = fd_table_offset;
        self.header.pickle_blob_offset  = pickle_blob_offset;
        let header_bytes2 = bincode::serialize(&self.header)?;
        file.seek(SeekFrom::Start(MAGIC.len() as u64 + 4 + 4))?;
        file.write_all(&header_bytes2)?;

        drop(file);

        // 8. Atomic rename.
        std::fs::rename(&tmp_path, &self.path)?;

        info!(
            path = %self.path.display(),
            modules = self.module_table.entries.len(),
            blobs_kb = self.pickle_blobs.len() / 1024,
            "snapshot written"
        );

        Ok(self.path)
    }
}

// ─── SnapshotReader ────────────────────────────────────────────────────────────

/// Memory-maps a snapshot file and provides access to its sections.
pub struct SnapshotReader {
    pub header:       SnapshotHeader,
    pub module_table: ModuleTable,
    pub fd_table:     FdTable,
    mmap:             memmap2::Mmap,
}

impl SnapshotReader {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;

        // Safety: the file is only read through shared references; we never mutate it.
        let mmap = unsafe { MmapOptions::new().map(&file)? };

        // Verify magic.
        if mmap.len() < MAGIC.len() + 8 {
            return Err(FreezeError::CorruptSnapshot {
                reason: "file too small".into(),
            });
        }
        if &mmap[..MAGIC.len()] != MAGIC {
            return Err(FreezeError::CorruptSnapshot {
                reason: "magic bytes mismatch".into(),
            });
        }

        let mut cursor = MAGIC.len();

        // Format version.
        let fmt_ver = u32::from_le_bytes(mmap[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        if fmt_ver != FORMAT_VERSION {
            return Err(FreezeError::CorruptSnapshot {
                reason: format!("format version {fmt_ver} != expected {FORMAT_VERSION}"),
            });
        }

        // Header length.
        let header_len = u32::from_le_bytes(mmap[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;

        // Deserialise header.
        let header: SnapshotHeader = bincode::deserialize(&mmap[cursor..cursor + header_len])
            .map_err(|e| FreezeError::CorruptSnapshot { reason: e.to_string() })?;
        cursor += header_len;
        let _ = cursor; // further access goes through offset fields in header.

        // Module table.
        let mt_start = header.module_table_offset as usize;
        let ft_start = header.fd_table_offset     as usize;
        let pb_start = header.pickle_blob_offset  as usize;

        let module_table: ModuleTable = bincode::deserialize(&mmap[mt_start..ft_start])
            .map_err(|e| FreezeError::CorruptSnapshot { reason: e.to_string() })?;

        // Fd table.
        let fd_table: FdTable = bincode::deserialize(&mmap[ft_start..pb_start])
            .map_err(|e| FreezeError::CorruptSnapshot { reason: e.to_string() })?;

        debug!(
            modules = module_table.entries.len(),
            fds     = fd_table.entries.len(),
            "snapshot loaded"
        );

        Ok(Self { header, module_table, fd_table, mmap })
    }

    /// Borrow the raw pickle blob for a module entry.
    pub fn pickle_blob(&self, offset: u64, len: u64) -> &[u8] {
        let base  = self.header.pickle_blob_offset as usize;
        let start = base + offset as usize;
        let end   = start + len as usize;
        &self.mmap[start..end]
    }
}

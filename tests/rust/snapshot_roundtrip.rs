// Integration tests for the snapshot write → read round-trip.
// Run with: cargo test

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use tempfile::TempDir;

    use pyfreeze_rs::{
        snapshot::{
            format::{
                CaptureStrategy, FdEntry, FdKind, FdTable, ModuleEntry,
                ModuleTable, SnapshotHeader,
            },
            hash::compute_source_hash,
            metadata::{validate_runtime_compatibility, MetadataBuilder},
            SnapshotReader, SnapshotWriter,
        },
    };

    fn make_test_header(source_hash: &str) -> SnapshotHeader {
        let meta = MetadataBuilder::new()
            .source_hash(source_hash)
            .python_impl_version("CPython 3.12.3")
            .framework("test")
            .import_phase_ms(42)
            .build();

        SnapshotHeader {
            metadata:            meta,
            module_table_offset: 0,
            fd_table_offset:     0,
            pickle_blob_offset:  0,
        }
    }

    // ── Round-trip: empty snapshot ──────────────────────────────────────────

    #[test]
    fn empty_snapshot_roundtrip() {
        let dir  = TempDir::new().unwrap();
        let path = dir.path().join("empty.pyfreeze");

        let header = make_test_header("abc123");
        let writer = SnapshotWriter::new(&path, header);
        writer.commit().unwrap();

        let reader = SnapshotReader::open(&path).unwrap();
        assert_eq!(reader.header.metadata.source_hash, "abc123");
        assert_eq!(reader.header.metadata.framework,   "test");
        assert_eq!(reader.module_table.entries.len(),  0);
        assert_eq!(reader.fd_table.entries.len(),      0);
    }

    // ── Round-trip: modules + pickle blobs ──────────────────────────────────

    #[test]
    fn modules_and_blobs_roundtrip() {
        let dir  = TempDir::new().unwrap();
        let path = dir.path().join("modules.pyfreeze");

        let blob_a = b"pickle_data_for_module_a";
        let blob_b = b"longer_pickle_data_for_module_b_which_is_larger";

        let header = make_test_header("deadbeef");
        let mut writer = SnapshotWriter::new(&path, header);

        let (off_a, len_a) = writer.add_pickle_blob(blob_a);
        let (off_b, len_b) = writer.add_pickle_blob(blob_b);

        let mut table = ModuleTable::new();
        table.add(ModuleEntry {
            name:             "module_a".into(),
            capture_strategy: CaptureStrategy::PickledDict,
            blob_offset:      Some(off_a),
            blob_len:         Some(len_a),
            source_path:      Some("/app/module_a.py".into()),
            source_hash:      Some("hash_a".into()),
        });
        table.add(ModuleEntry {
            name:             "module_b".into(),
            capture_strategy: CaptureStrategy::PickledDict,
            blob_offset:      Some(off_b),
            blob_len:         Some(len_b),
            source_path:      None,
            source_hash:      None,
        });
        table.add(ModuleEntry {
            name:             "builtins".into(),
            capture_strategy: CaptureStrategy::ReImport,
            blob_offset:      None,
            blob_len:         None,
            source_path:      None,
            source_hash:      None,
        });

        writer.set_module_table(table);
        writer.commit().unwrap();

        // ── Read back ────────────────────────────────────────────────────────
        let reader = SnapshotReader::open(&path).unwrap();

        assert_eq!(reader.module_table.entries.len(), 3);

        let entry_a = reader.module_table.get("module_a").unwrap();
        assert_eq!(entry_a.capture_strategy, CaptureStrategy::PickledDict);
        let data_a = reader.pickle_blob(entry_a.blob_offset.unwrap(), entry_a.blob_len.unwrap());
        assert_eq!(data_a, blob_a);

        let entry_b = reader.module_table.get("module_b").unwrap();
        let data_b = reader.pickle_blob(entry_b.blob_offset.unwrap(), entry_b.blob_len.unwrap());
        assert_eq!(data_b, blob_b);

        let entry_builtin = reader.module_table.get("builtins").unwrap();
        assert_eq!(entry_builtin.capture_strategy, CaptureStrategy::ReImport);
    }

    // ── Round-trip: file descriptor table ───────────────────────────────────

    #[test]
    fn fd_table_roundtrip() {
        let dir  = TempDir::new().unwrap();
        let path = dir.path().join("fds.pyfreeze");

        let header = make_test_header("fd_test");
        let mut writer = SnapshotWriter::new(&path, header);

        let fd_table = FdTable {
            entries: vec![
                FdEntry { fd: 0, kind: FdKind::Stdio },
                FdEntry {
                    fd:   3,
                    kind: FdKind::RegularFile {
                        path:   "/app/logfile.log".into(),
                        offset: 1024,
                        flags:  2,   // O_RDWR
                        mode:   0o644,
                    },
                },
                FdEntry {
                    fd:   4,
                    kind: FdKind::TcpSocket {
                        peer_addr: "127.0.0.1:5432".into(),
                        is_tls:    false,
                    },
                },
                FdEntry {
                    fd:   5,
                    kind: FdKind::Pipe {
                        buffered_data: b"hello from pipe".to_vec(),
                    },
                },
            ],
        };

        writer.set_fd_table(fd_table);
        writer.commit().unwrap();

        let reader = SnapshotReader::open(&path).unwrap();
        assert_eq!(reader.fd_table.entries.len(), 4);

        // Verify the TCP socket entry.
        let tcp = &reader.fd_table.entries[2];
        assert_eq!(tcp.fd, 4);
        if let FdKind::TcpSocket { peer_addr, is_tls } = &tcp.kind {
            assert_eq!(peer_addr, "127.0.0.1:5432");
            assert!(!is_tls);
        } else {
            panic!("expected TcpSocket, got {:?}", tcp.kind);
        }

        // Verify buffered pipe data.
        let pipe = &reader.fd_table.entries[3];
        if let FdKind::Pipe { buffered_data } = &pipe.kind {
            assert_eq!(buffered_data, b"hello from pipe");
        } else {
            panic!("expected Pipe, got {:?}", pipe.kind);
        }
    }

    // ── Corrupt file detection ───────────────────────────────────────────────

    #[test]
    fn corrupt_magic_rejected() {
        let dir  = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.pyfreeze");
        std::fs::write(&path, b"NOTMAGIC not a valid snapshot file at all").unwrap();

        let result = SnapshotReader::open(&path);
        assert!(
            matches!(result, Err(pyfreeze_rs::error::FreezeError::CorruptSnapshot { .. })),
            "expected CorruptSnapshot, got {:?}",
            result
        );
    }

    // ── Staleness: version mismatch ──────────────────────────────────────────

    #[test]
    fn version_mismatch_is_detected() {
        let meta = MetadataBuilder::new()
            .python_impl_version("CPython 3.11.9")
            .source_hash("x")
            .framework("test")
            .build();

        let result = validate_runtime_compatibility(&meta, "CPython 3.12.3", "x86_64-linux");
        assert!(
            matches!(result, Err(pyfreeze_rs::error::FreezeError::VersionMismatch { .. })),
        );
    }

    // ── Source hash: content change detected ─────────────────────────────────

    #[test]
    fn source_hash_detects_file_change() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"import os").unwrap();
        let path: String = f.path().to_string_lossy().into();

        let h1 = compute_source_hash(&[path.clone()], "CPython 3.12.3", "x86_64-linux").unwrap();

        // Change the file content.
        f.as_file().set_len(0).unwrap();
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(0)).unwrap();
        f.write_all(b"import sys").unwrap();

        let h2 = compute_source_hash(&[path], "CPython 3.12.3", "x86_64-linux").unwrap();
        assert_ne!(h1, h2, "hash must change when file content changes");
    }

    // ── Atomic write: no partial file on error ───────────────────────────────

    #[test]
    fn tmp_file_cleaned_up_on_successful_commit() {
        let dir  = TempDir::new().unwrap();
        let path = dir.path().join("atomic.pyfreeze");
        let tmp  = path.with_extension("pyfreeze.tmp");

        let writer = SnapshotWriter::new(&path, make_test_header("abc"));
        writer.commit().unwrap();

        // After commit the .tmp file must not exist.
        assert!(!tmp.exists(), ".tmp file should be removed after commit");
        assert!(path.exists(), "final snapshot file must exist");
    }
}

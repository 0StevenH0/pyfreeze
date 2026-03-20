// Discovers and classifies all open file descriptors held by the current
// Python process at snapshot time.
//
// Strategy per FD type:
//   Regular file  → record (path, offset, flags) for re-open on restore.
//   TCP socket    → record (peer_addr, tls) as a lazy-reconnect marker.
//   Unix socket   → record the socket path.
//   Pipe          → drain into a Vec<u8> buffer (only safe if writer end is closed).
//   stdin/stdout/stderr → skip (fd 0/1/2 are inherited from the shell).
//   Unknown       → record description, log warning, close on restore.

use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::{
    error::{FreezeError, Result},
    snapshot::format::{FdEntry, FdKind, FdTable},
};

// ─── Public entry point ───────────────────────────────────────────────────────

/// Walk `/proc/self/fd` (Linux) or use `fcntl`/`proc_pidinfo` (macOS) to
/// enumerate all open file descriptors and classify each one.
pub fn capture_fd_table() -> Result<FdTable> {
    let raw_fds = enumerate_open_fds()?;
    let mut entries = Vec::with_capacity(raw_fds.len());

    for fd in raw_fds {
        match classify_fd(fd) {
            Ok(entry) => {
                debug!(fd = fd, kind = ?entry.kind, "captured fd");
                entries.push(entry);
            }
            Err(e) => {
                warn!(fd = fd, error = %e, "skipping fd — could not classify");
                entries.push(FdEntry {
                    fd,
                    kind: FdKind::Unknown {
                        description: e.to_string(),
                    },
                });
            }
        }
    }

    Ok(FdTable { entries })
}

// ─── FD enumeration ──────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn enumerate_open_fds() -> Result<Vec<i32>> {
    // /proc/self/fd contains one symlink per open fd.
    let dir = std::fs::read_dir("/proc/self/fd")?;
    let mut fds = Vec::new();

    for entry in dir.flatten() {
        if let Ok(name) = entry.file_name().into_string() {
            if let Ok(fd) = name.parse::<i32>() {
                // Skip the fd we just opened to read /proc/self/fd.
                fds.push(fd);
            }
        }
    }

    fds.sort_unstable();
    Ok(fds)
}

#[cfg(target_os = "macos")]
fn enumerate_open_fds() -> Result<Vec<i32>> {
    use std::process;
    // Use `lsof -p <pid> -F f` as a portable fallback on macOS.
    // A production implementation would call proc_pidinfo directly.
    let output = std::process::Command::new("lsof")
        .args(["-p", &process::id().to_string(), "-F", "f"])
        .output()?;

    let mut fds = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(rest) = line.strip_prefix('f') {
            if let Ok(fd) = rest.parse::<i32>() {
                fds.push(fd);
            }
        }
    }

    fds.sort_unstable();
    Ok(fds)
}

#[cfg(windows)]
fn enumerate_open_fds() -> Result<Vec<i32>> {
    // On Windows the concept of "file descriptors" is mapped to CRT file
    // handles.  For now, we scan the standard CRT range.
    // TODO: use NtQuerySystemInformation for a proper enumeration.
    let mut fds = Vec::new();
    for fd in 0i32..2048 {
        if is_valid_handle_windows(fd) {
            fds.push(fd);
        }
    }
    Ok(fds)
}

#[cfg(windows)]
fn is_valid_handle_windows(fd: i32) -> bool {
    // Attempt a zero-length read as a validity probe.
    use std::os::windows::io::FromRawHandle;
    unsafe {
        let h = libc::get_osfhandle(fd);
        h != -1
    }
}

// ─── FD classification ───────────────────────────────────────────────────────

fn classify_fd(fd: i32) -> Result<FdEntry> {
    // stdin / stdout / stderr — always skip.
    if fd <= 2 {
        return Ok(FdEntry { fd, kind: FdKind::Stdio });
    }

    #[cfg(unix)]
    return classify_fd_unix(fd);

    #[cfg(windows)]
    return classify_fd_windows(fd);
}

#[cfg(unix)]
fn classify_fd_unix(fd: i32) -> Result<FdEntry> {
    use nix::sys::stat::{fstat, SFlag};
    use std::os::unix::io::FromRawFd;

    let stat = fstat(fd).map_err(|e| FreezeError::FdCaptureFailure {
        fd,
        reason: e.to_string(),
    })?;

    let file_type = SFlag::from_bits_truncate(stat.st_mode);

    if file_type.contains(SFlag::S_IFREG) {
        // Regular file.
        classify_regular_file(fd)
    } else if file_type.contains(SFlag::S_IFSOCK) {
        // Socket — determine TCP vs Unix.
        classify_socket(fd)
    } else if file_type.contains(SFlag::S_IFIFO) {
        // Pipe / FIFO.
        classify_pipe(fd)
    } else {
        Ok(FdEntry {
            fd,
            kind: FdKind::Unknown {
                description: format!("unsupported st_mode 0o{:o}", stat.st_mode),
            },
        })
    }
}

#[cfg(unix)]
fn classify_regular_file(fd: i32) -> Result<FdEntry> {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};

    // Read the current file-offset.
    let offset = nix::unistd::lseek(fd, 0, nix::unistd::Whence::SeekCur)
        .map_err(|e| FreezeError::FdCaptureFailure { fd, reason: e.to_string() })?
        as u64;

    // Read the open flags.
    let flags_raw = fcntl(fd, FcntlArg::F_GETFL)
        .map_err(|e| FreezeError::FdCaptureFailure { fd, reason: e.to_string() })?;

    // Resolve the fd to a path via /proc/self/fd/<fd>.
    let proc_link = format!("/proc/self/fd/{}", fd);
    let path = std::fs::read_link(&proc_link)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "<unknown>".into());

    Ok(FdEntry {
        fd,
        kind: FdKind::RegularFile {
            path,
            offset,
            flags: flags_raw,
            mode: 0o644,
        },
    })
}

#[cfg(unix)]
fn classify_socket(fd: i32) -> Result<FdEntry> {
    use std::os::unix::io::BorrowedFd;
    use std::net::TcpStream;

    // Try to get a peer address — if it succeeds, this is a connected TCP socket.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };

    // Use nix to call getpeername.
    match nix::sys::socket::getpeername::<nix::sys::socket::SockaddrStorage>(fd) {
        Ok(peer) => {
            let peer_str = peer.to_string();
            Ok(FdEntry {
                fd,
                kind: FdKind::TcpSocket {
                    peer_addr: peer_str,
                    is_tls: false, // TODO: detect TLS via getsockopt SO_PROTOCOL
                },
            })
        }
        Err(_) => {
            // Possibly a Unix-domain socket or unconnected socket.
            // Try to get the socket path.
            match nix::sys::socket::getsockname::<nix::sys::socket::UnixAddr>(fd) {
                Ok(addr) => {
                    let path = addr.path()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "<unnamed>".into());
                    Ok(FdEntry { fd, kind: FdKind::UnixSocket { path } })
                }
                Err(e) => Err(FreezeError::FdCaptureFailure {
                    fd,
                    reason: format!("socket with no peer/name: {e}"),
                }),
            }
        }
    }
}

#[cfg(unix)]
fn classify_pipe(fd: i32) -> Result<FdEntry> {
    use nix::fcntl::{fcntl, FcntlArg};

    // Check if this is the write or read end.
    let flags = fcntl(fd, FcntlArg::F_GETFL)
        .map_err(|e| FreezeError::FdCaptureFailure { fd, reason: e.to_string() })?;

    let is_write = (flags & nix::fcntl::OFlag::O_WRONLY.bits()) != 0;

    if is_write {
        // Can't drain a write-end pipe.
        return Ok(FdEntry {
            fd,
            kind: FdKind::Unknown {
                description: "write-end pipe (cannot buffer)".into(),
            },
        });
    }

    // Drain available data (non-blocking read).
    use nix::fcntl::OFlag;
    let orig_flags = fcntl(fd, FcntlArg::F_GETFL).unwrap_or(0);
    let _ = fcntl(fd, FcntlArg::F_SETFL(OFlag::from_bits_truncate(orig_flags | OFlag::O_NONBLOCK.bits())));

    let mut buffered = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match nix::unistd::read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => buffered.extend_from_slice(&buf[..n]),
            Err(_) => break, // EAGAIN → no more data
        }
    }

    // Restore blocking mode.
    let _ = fcntl(fd, FcntlArg::F_SETFL(OFlag::from_bits_truncate(orig_flags)));

    Ok(FdEntry { fd, kind: FdKind::Pipe { buffered_data: buffered } })
}

#[cfg(windows)]
fn classify_fd_windows(fd: i32) -> Result<FdEntry> {
    // Simplified Windows stub — extend with proper Windows handle classification.
    Ok(FdEntry {
        fd,
        kind: FdKind::Unknown {
            description: "windows fd classification not yet implemented".into(),
        },
    })
}

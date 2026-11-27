//! Crash-safe idle auto-backup persistence (`I-61` ~ `I-66`).

use crate::core::{BufferId, Doc, Eol, EolStreamDecoder};
use crate::shell::{home_dir, registore, xdg_path};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

const REC_MAGIC: &[u8; 8] = b"NEONREC1";

/// Snapshot of a single modified buffer sent to the background recovery worker (`I-64`).
#[derive(Debug, Clone)]
pub struct RecoverySnapshot {
    pub buffer_id: BufferId,
    pub path: Option<String>,
    pub name: String,
    pub doc: Doc,
    pub eol: Eol,
}

#[derive(Debug, Clone)]
enum BufferBackupOp {
    Write(RecoverySnapshot),
    Remove {
        buffer_id: BufferId,
        path: Option<String>,
    },
}

/// Parsed metadata from a `.rec` recovery file header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryMeta {
    pub pid: u32,
    pub buffer_id: BufferId,
    pub eol: Eol,
    pub timestamp_secs: u64,
    pub name: String,
    pub path: Option<String>,
}

#[derive(Default)]
struct QueueState {
    pending: BTreeMap<(u32, u64), BufferBackupOp>,
    shutdown: bool,
}

/// Single background worker that coalesces per-buffer backup operations (`I-64`, `I-66`).
pub struct RecoveryWorker {
    dir: PathBuf,
    pid: u32,
    shared: Arc<(Mutex<QueueState>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl RecoveryWorker {
    /// Creates `dir` if needed and spawns the background recovery worker.
    ///
    /// # Errors
    /// Returns `io::Error` if creating `dir` or spawning the thread fails.
    pub fn spawn(dir: PathBuf, pid: u32) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        let shared = Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        let worker_shared = Arc::clone(&shared);
        let worker_dir = dir.clone();
        let handle = thread::Builder::new()
            .name("neonemacs-recovery".to_owned())
            .spawn(move || worker_loop(&worker_dir, pid, &worker_shared))?;
        Ok(Self {
            dir,
            pid,
            shared,
            handle: Some(handle),
        })
    }

    /// Returns the recovery directory path.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the process ID associated with this worker's recovery files.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Enqueues a non-blocking snapshot write, replacing any older pending op for `(buffer_id, path)`.
    pub fn enqueue_write(&self, snapshot: RecoverySnapshot) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            if state.shutdown {
                return;
            }
            let key = (snapshot.buffer_id.0, fnv1a_64(snapshot.path.as_deref()));
            state.pending.insert(key, BufferBackupOp::Write(snapshot));
            cvar.notify_one();
        }
    }

    /// Enqueues a non-blocking removal of the recovery artifact for `(buffer_id, path)` (`I-65`).
    pub fn enqueue_remove(&self, buffer_id: BufferId, path: Option<String>) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            if state.shutdown {
                return;
            }
            let key = (buffer_id.0, fnv1a_64(path.as_deref()));
            state
                .pending
                .insert(key, BufferBackupOp::Remove { buffer_id, path });
            cvar.notify_one();
        }
    }

    /// Drains pending operations, stops the worker, and removes all recovery files for `self.pid` (`I-65`).
    pub fn drain_and_stop(mut self) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            state.shutdown = true;
            cvar.notify_one();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        cleanup_pid_files(&self.dir, self.pid);
    }
}

impl Drop for RecoveryWorker {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            state.shutdown = true;
            cvar.notify_one();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(dir: &Path, pid: u32, shared: &(Mutex<QueueState>, Condvar)) {
    let (lock, cvar) = shared;
    loop {
        let op = {
            let Ok(mut state) = lock.lock() else {
                return;
            };
            loop {
                if let Some((_, op)) = state.pending.pop_first() {
                    break op;
                }
                if state.shutdown {
                    return;
                }
                state = match cvar.wait(state) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
        };

        match op {
            BufferBackupOp::Write(snap) => {
                let _ = write_recovery_file(dir, pid, &snap);
            }
            BufferBackupOp::Remove { buffer_id, path } => {
                let target = dir.join(recovery_file_name(pid, buffer_id, path.as_deref()));
                let _ = fs::remove_file(target);
            }
        }
    }
}

/// Resolves `$XDG_STATE_HOME/neonemacs/recovery` (or `~/.local/state/neonemacs/recovery`).
#[must_use]
pub fn recovery_dir() -> Option<PathBuf> {
    let xdg = env::var_os("XDG_STATE_HOME");
    let home = home_dir();
    xdg_path(
        xdg.as_deref(),
        home.as_deref(),
        &[".local", "state"],
        "recovery",
    )
}

/// Computes the per-instance, per-buffer recovery file name (`<pid>-<id>-<hash>.rec`).
#[must_use]
pub fn recovery_file_name(pid: u32, buffer_id: BufferId, path: Option<&str>) -> String {
    format!("{pid}-{}-{:016x}.rec", buffer_id.0, fnv1a_64(path))
}

const fn fnv1a_64(path: Option<&str>) -> u64 {
    let Some(s) = path else {
        return 0;
    };
    let bytes = s.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    hash
}

fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_field(s: &str) -> io::Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid escape in recovery header",
                ));
            }
        }
    }
    Ok(out)
}

fn encode_header(pid: u32, snap: &RecoverySnapshot, timestamp_secs: u64) -> Vec<u8> {
    let eol = match snap.eol {
        Eol::Lf => "lf",
        Eol::Crlf => "crlf",
    };
    let mut meta = format!(
        "pid={pid}\nbuffer_id={}\neol={eol}\ntimestamp={timestamp_secs}\nname={}\n",
        snap.buffer_id.0,
        escape_field(&snap.name)
    );
    if let Some(path) = &snap.path {
        meta.push_str("path=");
        meta.push_str(&escape_field(path));
        meta.push('\n');
    }

    let meta_bytes = meta.as_bytes();
    let len = u32::try_from(meta_bytes.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(REC_MAGIC.len() + 4 + meta_bytes.len());
    out.extend_from_slice(REC_MAGIC);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&meta_bytes[..len as usize]);
    out
}

/// Parses a `.rec` recovery file into its metadata header and raw encoded body slice.
///
/// # Errors
/// Returns `io::ErrorKind::InvalidData` if the magic, header length, or fields are malformed.
pub fn parse_recovery_file(bytes: &[u8]) -> io::Result<(RecoveryMeta, &[u8])> {
    if bytes.len() < REC_MAGIC.len() + 4 || &bytes[..REC_MAGIC.len()] != REC_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid recovery file magic",
        ));
    }
    let len_bytes: [u8; 4] = bytes[REC_MAGIC.len()..REC_MAGIC.len() + 4]
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "missing header length"))?;
    let header_len = u32::from_le_bytes(len_bytes) as usize;
    let header_start = REC_MAGIC.len() + 4;
    let header_end = header_start
        .checked_add(header_len)
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated recovery header"))?;
    let header_str = std::str::from_utf8(&bytes[header_start..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 recovery header"))?;

    let mut pid = None;
    let mut buffer_id = None;
    let mut eol = None;
    let mut timestamp_secs = None;
    let mut name = None;
    let mut path = None;

    for line in header_str.lines() {
        let Some((k, v)) = line.split_once('=') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed recovery header line",
            ));
        };
        match k {
            "pid" => pid = v.parse::<u32>().ok(),
            "buffer_id" => buffer_id = v.parse::<u32>().ok().map(BufferId),
            "eol" => {
                eol = match v {
                    "lf" => Some(Eol::Lf),
                    "crlf" => Some(Eol::Crlf),
                    _ => None,
                };
            }
            "timestamp" => timestamp_secs = v.parse::<u64>().ok(),
            "name" => name = Some(unescape_field(v)?),
            "path" => path = Some(unescape_field(v)?),
            _ => {}
        }
    }

    let meta = RecoveryMeta {
        pid: pid.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing pid"))?,
        buffer_id: buffer_id
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing buffer_id"))?,
        eol: eol.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing eol"))?,
        timestamp_secs: timestamp_secs
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing timestamp"))?,
        name: name.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing name"))?,
        path,
    };
    Ok((meta, &bytes[header_end..]))
}

/// Writes one recovery snapshot atomically to `dir` via `registore` (`I-62`, `I-66`).
///
/// # Errors
/// Returns `io::Error` if creating the file or atomic rename fails.
pub fn write_recovery_file(dir: &Path, pid: u32, snap: &RecoverySnapshot) -> io::Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let header = encode_header(pid, snap, ts);
    let target = dir.join(recovery_file_name(
        pid,
        snap.buffer_id,
        snap.path.as_deref(),
    ));
    registore::persist_with_header(&target, &header, &snap.doc, snap.eol, None)?;
    Ok(target)
}

/// Removes all `.rec` files belonging to `pid` in `dir` (`I-65`).
pub fn cleanup_pid_files(dir: &Path, pid: u32) {
    let prefix = format!("{pid}-");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let lossy = name.to_string_lossy();
        if lossy.starts_with(&prefix) && lossy.ends_with(".rec") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Returns whether `pid` currently refers to a live OS process.
#[must_use]
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let Ok(raw) = i32::try_from(pid) else {
            return false;
        };
        matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None),
            Ok(()) | Err(nix::errno::Errno::EPERM)
        )
    }
    #[cfg(windows)]
    {
        let Ok(out) = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
        else {
            return false;
        };
        if !out.status.success() {
            return false;
        }
        let needle = format!(",\"{pid}\",");
        String::from_utf8_lossy(&out.stdout).contains(&needle)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Finds the newest orphan `.rec` file in `dir` matching `path` (excluding `self_pid` and live PIDs),
/// decoding its body back to `\n`-normalized `Doc` while preserving `meta.eol`,
/// and returns all matching orphan `.rec` paths for cleanup.
#[must_use]
pub fn find_latest_recovery(
    dir: &Path,
    path: &str,
    self_pid: u32,
) -> Option<(Vec<PathBuf>, RecoveryMeta, Doc, Eol)> {
    find_latest_recovery_with(dir, path, self_pid, is_pid_alive)
}

fn find_latest_recovery_with(
    dir: &Path,
    path: &str,
    self_pid: u32,
    is_alive: impl Fn(u32) -> bool,
) -> Option<(Vec<PathBuf>, RecoveryMeta, Doc, Eol)> {
    let entries = fs::read_dir(dir).ok()?;
    let mut matching_paths = Vec::new();
    let mut best: Option<(RecoveryMeta, Doc, Eol)> = None;
    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry_path.extension().and_then(|e| e.to_str()) != Some("rec") {
            continue;
        }
        let Ok(bytes) = fs::read(&entry_path) else {
            continue;
        };
        let Ok((meta, body)) = parse_recovery_file(&bytes) else {
            continue;
        };
        if meta.pid == self_pid || meta.path.as_deref() != Some(path) || is_alive(meta.pid) {
            continue;
        }
        matching_paths.push(entry_path);
        let replace = best
            .as_ref()
            .is_none_or(|(prev, _, _)| meta.timestamp_secs >= prev.timestamp_secs);
        if replace {
            let (doc, _) = EolStreamDecoder::new().push_chunk(body).finish();
            let eol = meta.eol;
            best = Some((meta, doc, eol));
        }
    }
    best.map(|(meta, doc, eol)| (matching_paths, meta, doc, eol))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_file_roundtrip_preserves_metadata_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let snap = RecoverySnapshot {
            buffer_id: BufferId(7),
            path: Some("/tmp/dir\nname\\file.txt".to_owned()),
            name: "file.txt".to_owned(),
            doc: Doc::from_bytes(b"hello\nworld"),
            eol: Eol::Crlf,
        };

        let file = write_recovery_file(dir.path(), 4242, &snap).unwrap();
        let raw = fs::read(&file).unwrap();
        let (meta, body) = parse_recovery_file(&raw).unwrap();

        assert_eq!(meta.pid, 4242);
        assert_eq!(meta.buffer_id, BufferId(7));
        assert_eq!(meta.eol, Eol::Crlf);
        assert_eq!(meta.name, "file.txt");
        assert_eq!(meta.path.as_deref(), Some("/tmp/dir\nname\\file.txt"));
        assert_eq!(body, b"hello\r\nworld");
    }

    #[test]
    fn worker_coalesces_writes_and_cleans_on_drain() {
        let dir = tempfile::tempdir().unwrap();
        let worker = RecoveryWorker::spawn(dir.path().to_path_buf(), 9001).unwrap();

        worker.enqueue_write(RecoverySnapshot {
            buffer_id: BufferId(1),
            path: None,
            name: "*scratch*".to_owned(),
            doc: Doc::from_bytes(b"first"),
            eol: Eol::Lf,
        });
        worker.enqueue_write(RecoverySnapshot {
            buffer_id: BufferId(1),
            path: None,
            name: "*scratch*".to_owned(),
            doc: Doc::from_bytes(b"latest"),
            eol: Eol::Lf,
        });

        let expected = dir.path().join(recovery_file_name(9001, BufferId(1), None));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Ok(bytes) = fs::read(&expected)
                && let Ok((_, body)) = parse_recovery_file(&bytes)
                && body == b"latest"
            {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        let raw = fs::read(&expected).unwrap();
        let (_, body) = parse_recovery_file(&raw).unwrap();
        assert_eq!(body, b"latest");

        worker.drain_and_stop();
        assert!(!expected.exists());
    }

    #[test]
    fn worker_keeps_remove_for_old_path_when_write_for_new_path_is_pending() {
        let dir = tempfile::tempdir().unwrap();
        let worker = RecoveryWorker::spawn(dir.path().to_path_buf(), 9002).unwrap();

        let old_file = write_recovery_file(
            dir.path(),
            9002,
            &RecoverySnapshot {
                buffer_id: BufferId(1),
                path: Some("a.txt".to_owned()),
                name: "a.txt".to_owned(),
                doc: Doc::from_bytes(b"old"),
                eol: Eol::Lf,
            },
        )
        .unwrap();
        assert!(old_file.exists());

        // Hold the queue lock while enqueuing both ops so they coalesce in `pending` before the worker pops.
        {
            let (lock, cvar) = &*worker.shared;
            let mut state = lock.lock().unwrap();
            state.pending.insert(
                (1, fnv1a_64(Some("a.txt"))),
                BufferBackupOp::Remove {
                    buffer_id: BufferId(1),
                    path: Some("a.txt".to_owned()),
                },
            );
            state.pending.insert(
                (1, fnv1a_64(Some("b.txt"))),
                BufferBackupOp::Write(RecoverySnapshot {
                    buffer_id: BufferId(1),
                    path: Some("b.txt".to_owned()),
                    name: "b.txt".to_owned(),
                    doc: Doc::from_bytes(b"new"),
                    eol: Eol::Lf,
                }),
            );
            cvar.notify_one();
        }

        let new_file = dir
            .path()
            .join(recovery_file_name(9002, BufferId(1), Some("b.txt")));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if !old_file.exists() && new_file.exists() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(!old_file.exists(), "old .rec file should be removed");
        assert!(new_file.exists(), "new .rec file should be written");
        worker.drain_and_stop();
    }

    #[test]
    fn find_latest_recovery_filters_self_pid_scratch_and_picks_newest_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let target_path = "/work/note.txt";

        // Same PID -> ignored.
        write_recovery_file(
            dir.path(),
            500,
            &RecoverySnapshot {
                buffer_id: BufferId(1),
                path: Some(target_path.to_owned()),
                name: "note.txt".to_owned(),
                doc: Doc::from_bytes(b"same-pid"),
                eol: Eol::Lf,
            },
        )
        .unwrap();

        // No path (*scratch*) -> ignored.
        write_recovery_file(
            dir.path(),
            100,
            &RecoverySnapshot {
                buffer_id: BufferId(1),
                path: None,
                name: "*scratch*".to_owned(),
                doc: Doc::from_bytes(b"scratch"),
                eol: Eol::Lf,
            },
        )
        .unwrap();

        // Corrupt .rec -> skipped.
        fs::write(dir.path().join("99-1-deadbeef.rec"), b"BAD").unwrap();

        // Older foreign PID with explicit timestamp 10.
        let older_snap = RecoverySnapshot {
            buffer_id: BufferId(2),
            path: Some(target_path.to_owned()),
            name: "note.txt".to_owned(),
            doc: Doc::from_bytes(b"old"),
            eol: Eol::Lf,
        };
        let older_file = dir
            .path()
            .join(recovery_file_name(101, BufferId(2), Some(target_path)));
        registore::persist_with_header(
            &older_file,
            &encode_header(101, &older_snap, 10),
            &older_snap.doc,
            older_snap.eol,
            None,
        )
        .unwrap();

        // Newer foreign PID with CRLF and explicit timestamp 20.
        let newer_snap = RecoverySnapshot {
            buffer_id: BufferId(3),
            path: Some(target_path.to_owned()),
            name: "note.txt".to_owned(),
            doc: Doc::from_bytes(b"line1\nline2"),
            eol: Eol::Crlf,
        };
        let newer_file = dir
            .path()
            .join(recovery_file_name(102, BufferId(3), Some(target_path)));
        registore::persist_with_header(
            &newer_file,
            &encode_header(102, &newer_snap, 20),
            &newer_snap.doc,
            newer_snap.eol,
            None,
        )
        .unwrap();

        let found = find_latest_recovery_with(dir.path(), target_path, 500, |_| false).unwrap();
        assert_eq!(found.0.len(), 2);
        assert!(found.0.contains(&older_file));
        assert!(found.0.contains(&newer_file));
        assert_eq!(found.1.pid, 102);
        assert_eq!(found.2, Doc::from_bytes(b"line1\nline2"));
        assert_eq!(found.3, Eol::Crlf);
    }

    #[test]
    fn find_latest_recovery_skips_live_foreign_pid_and_checks_real_process_liveness() {
        assert!(!is_pid_alive(0));
        assert!(is_pid_alive(std::process::id()));

        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit", "0"])
                .spawn()
                .unwrap()
        } else {
            std::process::Command::new("true").spawn().unwrap()
        };
        let dead_pid = child.id();
        let _ = child.wait().unwrap();
        assert!(!is_pid_alive(dead_pid));

        let dir = tempfile::tempdir().unwrap();
        let target_path = "/work/shared.txt";

        // Live foreign PID -> must be ignored even though pid != self_pid.
        write_recovery_file(
            dir.path(),
            200,
            &RecoverySnapshot {
                buffer_id: BufferId(1),
                path: Some(target_path.to_owned()),
                name: "shared.txt".to_owned(),
                doc: Doc::from_bytes(b"live-instance"),
                eol: Eol::Lf,
            },
        )
        .unwrap();

        assert!(
            find_latest_recovery_with(dir.path(), target_path, 500, |pid| pid == 200).is_none()
        );

        // Dead foreign PID -> recovered.
        write_recovery_file(
            dir.path(),
            dead_pid,
            &RecoverySnapshot {
                buffer_id: BufferId(2),
                path: Some(target_path.to_owned()),
                name: "shared.txt".to_owned(),
                doc: Doc::from_bytes(b"crashed-instance"),
                eol: Eol::Lf,
            },
        )
        .unwrap();

        let found =
            find_latest_recovery_with(dir.path(), target_path, 500, |pid| pid == 200).unwrap();
        assert_eq!(found.0.len(), 1);
        assert_eq!(found.1.pid, dead_pid);
        assert_eq!(found.2, Doc::from_bytes(b"crashed-instance"));
    }

    #[test]
    fn fnv1a_64_and_escape_cr_and_short_header_and_cleanup_pid() {
        // Distinct paths produce exact, distinct FNV-1a 64-bit hashes (catches |= / &= mutations).
        assert_eq!(fnv1a_64(None), 0);
        assert_eq!(fnv1a_64(Some("a")), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv1a_64(Some("ab")), fnv1a_64(Some("ba")));

        // `\r` in name and path round-trips through escape_field / unescape_field.
        let dir = tempfile::tempdir().unwrap();
        let snap = RecoverySnapshot {
            buffer_id: BufferId(9),
            path: Some("dir\rname\\file.txt".to_owned()),
            name: "my\rname".to_owned(),
            doc: Doc::from_bytes(b"body"),
            eol: Eol::Lf,
        };
        let rec_path = write_recovery_file(dir.path(), 777, &snap).unwrap();
        let raw = fs::read(&rec_path).unwrap();
        let (meta, body) = parse_recovery_file(&raw).unwrap();
        assert_eq!(meta.name, "my\rname");
        assert_eq!(meta.path.as_deref(), Some("dir\rname\\file.txt"));
        assert_eq!(body, b"body");

        // Short buffers (< REC_MAGIC.len() + 4) return InvalidData rather than panicking.
        for short in [&b"NEONREC1"[..], &b"NEONREC1\x01\x00\x00"[..]] {
            let err = parse_recovery_file(short).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }

        // cleanup_pid_files removes only `<pid>-*.rec` and preserves other extensions or PIDs.
        let keep_non_rec = dir.path().join("777-1-00.txt");
        let keep_other_pid = dir.path().join("778-1-00.rec");
        fs::write(&keep_non_rec, b"keep").unwrap();
        fs::write(&keep_other_pid, b"keep").unwrap();
        cleanup_pid_files(dir.path(), 777);
        assert!(!rec_path.exists());
        assert!(keep_non_rec.exists());
        assert!(keep_other_pid.exists());
    }

    #[test]
    fn worker_pid_and_drop_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let rec_file = write_recovery_file(
            dir.path(),
            4321,
            &RecoverySnapshot {
                buffer_id: BufferId(1),
                path: None,
                name: "*scratch*".to_owned(),
                doc: Doc::from_bytes(b"x"),
                eol: Eol::Lf,
            },
        )
        .unwrap();
        assert!(rec_file.exists());

        let worker = RecoveryWorker::spawn(dir.path().to_path_buf(), 4321).unwrap();
        assert_eq!(worker.pid(), 4321);
        worker.drain_and_stop();
        assert!(!rec_file.exists());
    }
}

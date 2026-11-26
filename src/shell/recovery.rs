//! Crash-safe idle auto-backup and emergency dump persistence (`I-61` ~ `I-66`).

use crate::core::{BufferId, Doc, Editor, Eol};
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

/// Synchronously dumps all `modified` buffers in `ed` to `dir` (`I-62`, `I-66`).
/// Returns the number of buffers successfully dumped; I/O errors on individual buffers are skipped.
pub fn emergency_dump(ed: &Editor, dir: &Path, pid: u32) -> usize {
    if fs::create_dir_all(dir).is_err() {
        return 0;
    }
    let mut dumped = 0;
    for buf in ed.buffers().iter() {
        if !buf.modified() {
            continue;
        }
        let snap = RecoverySnapshot {
            buffer_id: buf.id(),
            path: buf.file_path().map(str::to_owned),
            name: buf.name().to_owned(),
            doc: buf.text().clone(),
            eol: buf.eol(),
        };
        if write_recovery_file(dir, pid, &snap).is_ok() {
            dumped += 1;
        }
    }
    dumped
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Backing, Config, FilePath};

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
    fn emergency_dump_writes_only_modified_buffers_and_never_touches_originals() {
        let rec_dir = tempfile::tempdir().unwrap();
        let orig_dir = tempfile::tempdir().unwrap();
        let orig_path = orig_dir.path().join("orig.txt");
        fs::write(&orig_path, b"original-on-disk").unwrap();

        let ed = Editor::new(Config::default());
        let (ed, b1) = ed.add_buffer(
            "orig.txt",
            Doc::from_bytes(b"modified-unsaved"),
            Backing::file(
                FilePath::resolve("", orig_path.to_str().unwrap()),
                crate::core::DiskState::Unknown,
            ),
        );
        let ed = ed.with_buffer(b1, |b| b.mark_modified());
        let (ed, _b2) = ed.add_buffer("clean.txt", Doc::from_bytes(b"clean"), Backing::None);

        let count = emergency_dump(&ed, rec_dir.path(), 777);
        assert_eq!(count, 1);
        assert_eq!(fs::read(&orig_path).unwrap(), b"original-on-disk");

        let rec_file =
            rec_dir
                .path()
                .join(recovery_file_name(777, b1, ed.buffer_by_id(b1).file_path()));
        let raw = fs::read(&rec_file).unwrap();
        let (meta, body) = parse_recovery_file(&raw).unwrap();
        assert_eq!(meta.buffer_id, b1);
        assert_eq!(body, b"modified-unsaved");
    }

    #[test]
    fn write_recovery_failure_does_not_panic_in_emergency_dump() {
        let dir = tempfile::tempdir().unwrap();
        let file_as_dir = dir.path().join("not_a_dir");
        fs::write(&file_as_dir, b"blocker").unwrap();

        let ed = Editor::new(Config::default())
            .with_current_buffer(|b| b.with_text(Doc::from_bytes(b"unsaved")).mark_modified());
        assert_eq!(emergency_dump(&ed, &file_as_dir, 123), 0);
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
}

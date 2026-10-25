//! What the core asks the shell to do (`Effect`) and what the shell reports back (`Event`).
//! The core performs no I/O. Each `Effect` is answered by exactly one `Event` (I-15).

use crate::core::buffer::Buffer;
use crate::core::editor::BufferId;
use crate::core::file::DiskState;
use crate::core::keymap::Key;
use std::path::PathBuf;

pub type Effects = Vec<Effect>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Reads the disk state of `path` (I-04).
    StatDisk {
        id: BufferId,
        path: PathBuf,
    },
    /// Writes `bytes` to `path` atomically. Only after I-04 or a confirmation (I-13).
    WriteFile {
        id: BufferId,
        path: PathBuf,
        bytes: Vec<u8>,
    },
    /// Reads `path`. A file longer than `max_size` is not read: the result carries only its length.
    ReadFile {
        target: ReadTarget,
        path: PathBuf,
        max_size: u64,
    },
    RemoveWal {
        path: PathBuf,
    },
    RunShell {
        command: String,
    },
    RunLua {
        name: String,
    },
}

impl Effect {
    /// Returns a `RemoveWal` effect if `buf` is backed by a disk file.
    pub fn remove_wal_if_file(buf: &Buffer) -> Option<Self> {
        buf.file_path().map(|path| Self::RemoveWal {
            path: path.to_path_buf(),
        })
    }
}

/// Why a file was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTarget {
    /// A new buffer for the file. `show` selects it in the window.
    Open { line: Option<usize>, show: bool },
    /// Reload the buffer `id` from its file.
    Revert(BufferId),
}

/// What a file read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Missing,
    Found(RawFile),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    /// Length on disk, which can exceed `bytes.len()`.
    pub len: u64,
    pub disk: DiskState,
    /// The whole file, or empty when `len` exceeds `max_size`.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    /// Run a command by name. The Lua `run` function sends this.
    Run(String),
    Disk {
        id: BufferId,
        state: DiskState,
    },
    Written {
        id: BufferId,
        result: Result<DiskState, String>,
    },
    FileRead {
        target: ReadTarget,
        path: PathBuf,
        result: Result<Read, String>,
    },
    WalRemoved,
    /// Raw stdout and stderr of a shell command, or the reason it did not start.
    ShellDone(Result<String, String>),
    /// Error text when the Lua command failed. The core shows its first line.
    LuaDone(Result<(), String>),
}

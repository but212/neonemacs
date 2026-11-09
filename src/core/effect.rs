//! What the core asks the shell to do (`Effect`) and what the shell reports back (`Event`).
//! The core performs no I/O. Each `Effect` is answered by exactly one `Event` (I-15).

use crate::core::buffer::Buffer;
use crate::core::config::ShellSpec;
use crate::core::editor::BufferId;
use crate::core::eol::Eol;
use crate::core::file::DiskState;
use crate::core::keymap::Key;
use std::path::PathBuf;

pub type Effects = Vec<Effect>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Reads the disk state of `path` (I-04).
    StatDisk {
        target: StatTarget,
        path: PathBuf,
    },
    /// Writes `bytes` to `path` atomically. Only after the I-04 check or a confirmation.
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
    /// Looks for the WAL of the file at `path` just opened with `disk` (I-44, I-45). An unusable
    /// WAL is moved aside, never overwritten. `line` is the `+n` of a start-up file.
    ProbeWal {
        path: PathBuf,
        disk: DiskState,
        line: Option<usize>,
    },
    RunShell {
        command: String,
        shell: ShellSpec,
    },
    RunLua {
        name: String,
    },
}

impl Effect {
    /// Returns a `RemoveWal` effect if `buf` is backed by a disk file.
    #[must_use]
    pub fn remove_wal_if_file(buf: &Buffer) -> Option<Self> {
        buf.file_path().map(|path| Self::RemoveWal {
            path: path.to_path_buf(),
        })
    }
}

/// Why a disk state was read (I-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatTarget {
    /// Save buffer `id` to its own file. A change since the read asks first.
    Save(BufferId),
    /// Write buffer `id` to the stat path (`write-file`). An existing file asks first.
    SaveAs(BufferId),
}

impl StatTarget {
    #[must_use]
    pub const fn id(self) -> BufferId {
        match self {
            Self::Save(id) | Self::SaveAs(id) => id,
        }
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

/// Unsaved text found in a WAL whose disk state matched: LF-normalized bytes and the line-ending mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub eol: Eol,
    pub bytes: Vec<u8>,
}

/// What the shell found when it looked for a WAL (I-44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalProbe {
    Absent,
    Valid(Recovered),
    /// The WAL does not match the file. It was moved to the given path, or the error says why not.
    Stale(Result<PathBuf, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    /// Run a command by name. The Lua `run` function sends this.
    Run(String),
    Disk {
        target: StatTarget,
        path: PathBuf,
        state: DiskState,
    },
    Written {
        id: BufferId,
        path: PathBuf,
        result: Result<DiskState, String>,
    },
    FileRead {
        target: ReadTarget,
        path: PathBuf,
        result: Result<Read, String>,
    },
    WalRemoved,
    WalProbed {
        path: PathBuf,
        line: Option<usize>,
        probe: WalProbe,
    },
    /// Raw stdout and stderr of a shell command, or the reason it did not start.
    ShellDone(Result<String, String>),
    /// Error text when the Lua command failed. The core shows its first line.
    LuaDone(Result<(), String>),
}

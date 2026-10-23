//! What the core asks the shell to do (`Effect`) and what the shell reports back (`Event`).
//! The core performs no I/O. Each `Effect` is answered by exactly one `Event` (I-15).

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
    /// Writes the undo file beside `path`. The bytes are encoded from the state the write produced (I-16).
    WriteUndo {
        id: BufferId,
        path: PathBuf,
        bytes: Vec<u8>,
    },
    /// Reads at most `max_size` bytes of `path`.
    ReadFile {
        target: ReadTarget,
        path: PathBuf,
        max_size: u64,
    },
    /// Reads the undo file beside `path`. The core verifies it against the disk state (I-10).
    ReadUndo {
        id: BufferId,
        path: PathBuf,
    },
    RunShell {
        command: String,
    },
    RunLua {
        name: String,
    },
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
    /// The first `min(len, max_size)` bytes.
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
    UndoWritten {
        id: BufferId,
        result: Result<(), String>,
    },
    FileRead {
        target: ReadTarget,
        path: PathBuf,
        result: Result<Read, String>,
    },
    UndoRead {
        id: BufferId,
        bytes: Option<Vec<u8>>,
    },
    /// Raw stdout and stderr of a shell command, or the reason it did not start.
    ShellDone(Result<String, String>),
    /// Error text when the Lua command failed. The core shows its first line.
    LuaDone(Result<(), String>),
}

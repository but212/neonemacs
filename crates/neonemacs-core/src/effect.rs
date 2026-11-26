//! What the core asks the shell to do (`Effect`) and what the shell reports back (`Event`).
//! The core performs no I/O. Each `Effect` is answered by at most one `Event` (I-15): `DeleteRecovery` yields no `Event`,
//! and an interactive session answers `RunShell`, `ReloadInit`, and `Suspend` itself (a refused `RunShell` gets no `Event`).

use crate::Doc;
use crate::buffer::BufferId;
use crate::config::ShellSpec;
use crate::eol::Eol;
use crate::file::{DiskState, RawFile};
use crate::keymap::Key;
use alloc::string::String;
use alloc::vec::Vec;

pub type Effects = Vec<Effect>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Reads the disk state of `path` (I-04).
    StatDisk {
        target: StatTarget,
        path: String,
    },
    /// Writes buffer `id` to `path` atomically. Only after the I-04 check or a confirmation.
    WriteFile {
        id: BufferId,
        path: String,
    },
    /// Reads `path`. A file longer than `max_size` is not read: the result carries only its length.
    ReadFile {
        target: ReadTarget,
        path: String,
        max_size: u64,
    },
    /// Probes for an orphan `.rec` crash recovery file matching `path` for buffer `id`.
    ProbeRecovery {
        id: BufferId,
        path: String,
    },
    /// Removes `.rec` crash recovery files after user confirmation or explicit decline.
    DeleteRecovery {
        rec_paths: Vec<String>,
    },
    RunShell {
        command: String,
        shell: ShellSpec,
    },
    RunLua {
        name: String,
    },
    /// Reads `init.lua` again and replaces the settings and the Lua state with the result (I-58).
    ReloadInit,
    /// Suspends the interactive terminal session (`SIGTSTP` on Unix).
    Suspend,
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

/// How `init.lua` was applied. The settings never hold part of it (I-54).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitLoad {
    /// Reload was requested from a Lua command, where the Lua state cannot be replaced.
    Ignored,
    /// There is no `init.lua`. The settings are the bundled ones.
    Absent,
    /// The settings are the bundled ones plus all of `init.lua`.
    Applied,
    /// `init.lua` was not applied. The text says why: the read error or the whole Lua error.
    Rejected(String),
}

/// Decoded crash recovery snapshot returned by `Event::RecoveryProbed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredFile {
    pub doc: Doc,
    pub eol: Eol,
    pub rec_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    /// Run a command by name. The Lua `run` function sends this.
    Run(String),
    Disk {
        target: StatTarget,
        path: String,
        state: DiskState,
    },
    Written {
        id: BufferId,
        path: String,
        result: Result<DiskState, String>,
    },
    FileRead {
        target: ReadTarget,
        path: String,
        result: Result<Option<RawFile>, String>,
    },
    RecoveryProbed {
        id: BufferId,
        rec: Option<RecoveredFile>,
    },
    /// Raw stdout and stderr of a shell command, or the reason it did not start.
    ShellDone(Result<String, String>),
    /// Error text when the Lua command failed. The core shows its first line.
    LuaDone(Result<(), String>),
    /// Answers `ReloadInit`.
    InitLoaded(InitLoad),
}

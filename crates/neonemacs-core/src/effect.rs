//! What the core asks the shell to do (`Effect`) and what the shell reports back (`Event`).
//! The core performs no I/O. Each `Effect` is answered by exactly one `Event` (I-15), except that an interactive session answers
//! `RunShell` and `ReloadInit` itself: a `RunShell` refused while another runs gets no `Event`.

use crate::Doc;
use crate::buffer::BufferId;
pub use crate::buffer::RecoveredDelta;
use crate::config::ShellSpec;
use crate::eol::Eol;
use crate::file::{DiskState, Read};
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
    /// Removes a WAL. A missing one is not an error.
    RemoveWal {
        target: WalTarget,
    },
    /// Writes the unsaved text of every entry to its WAL, in order. Answered by one `WalFlushed`
    /// with one result per entry. Never empty.
    FlushWal {
        entries: Vec<WalEntry>,
    },
    /// Looks for the WAL of the file at `path` just opened with `disk` (I-44, I-45). An unusable
    /// WAL is moved aside, never overwritten. `line` is the `+n` of a start-up file.
    ProbeWal {
        path: String,
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
    /// Reads `init.lua` again and replaces the settings and the Lua state with the result (I-58).
    ReloadInit,
}

/// Where a WAL lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalTarget {
    /// The WAL beside the file at this path (`.<name>.wal`).
    Sibling(String),
    /// This path is the WAL itself: the scratch slot (I-43).
    Slot(String),
}

impl WalTarget {
    /// The file path or the slot path: what a message names.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Sibling(path) | Self::Slot(path) => path,
        }
    }
}

/// Metadata for the unsaved text of buffer `id` recorded in its WAL header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalEntry {
    pub id: BufferId,
    pub target: WalTarget,
    pub disk: DiskState,
    pub eol: Eol,
}

/// The outcome of one `WalEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalWrite {
    pub id: BufferId,
    pub target: WalTarget,
    pub result: Result<(), String>,
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

/// Unsaved document and its line-ending mode, recovered from a matching WAL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub eol: Eol,
    pub doc: Doc,
}

/// What the shell found when it looked for a WAL (I-44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalProbe {
    Absent,
    Valid(Recovered),
    ValidDelta(RecoveredDelta),
    /// The WAL does not match the file. It was moved to the given path, or the error says why not.
    Stale(Result<String, String>),
}

/// How `init.lua` was applied. The settings never hold part of it (I-54).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitLoad {
    /// There is no `init.lua`. The settings are the bundled ones.
    Absent,
    /// The settings are the bundled ones plus all of `init.lua`.
    Applied,
    /// `init.lua` was not applied. The text says why: the read error or the whole Lua error.
    Rejected(String),
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
        result: Result<Read, String>,
    },
    WalRemoved,
    /// The shell saw no input for one second after an edit (I-23).
    Idle,
    WalFlushed(Vec<WalWrite>),
    WalProbed {
        path: String,
        line: Option<usize>,
        probe: WalProbe,
    },
    /// Raw stdout and stderr of a shell command, or the reason it did not start.
    ShellDone(Result<String, String>),
    /// Error text when the Lua command failed. The core shows its first line.
    LuaDone(Result<(), String>),
    /// Answers `ReloadInit`. `None` when the reload was asked for where the Lua state cannot be
    /// replaced: from a Lua command.
    InitLoaded(Option<InitLoad>),
}

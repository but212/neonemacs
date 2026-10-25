//! Editor modes, prompts, and modal state machines (§4, §8, §9).

use crate::core::buffer::BufferId;
use crate::core::search::Pattern;

/// Kind of a command, for commands that chain (§6, §7, I-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastCommand {
    Other,
    Insert {
        count: usize,
    },
    Undo {
        next: usize,
    },
    Kill,
    /// Goal display column for consecutive next-line and previous-line.
    Vertical {
        col: usize,
    },
    /// Bytes inserted by the last yank, for `yank-pop`.
    Yank {
        start: usize,
        end: usize,
    },
}

/// Key prefix waiting for its second key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    None,
    CtlX,
    Meta,
}

/// A line of input, read after a prompt label (§4, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub label: String,
    pub input: String,
    pub purpose: Purpose,
}

/// What a submitted prompt does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    ExecuteCommand,
    FindFile,
    WriteFile,
    SwitchBuffer,
    KillBuffer,
    GotoLine,
    ShellCommand,
    QueryFrom { regex: bool },
    QueryTo { regex: bool, from: String },
}

/// A yes-or-no question that must be answered before the command continues (I-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    SaveExternal(BufferId),
    QuitUnsaved,
    KillModified(BufferId),
    RevertModified(BufferId),
}

/// An incremental search in progress (§9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isearch {
    pub forward: bool,
    pub regex: bool,
    pub text: String,
    /// Point when the search started; restored on cancel and used after backspace.
    pub origin: usize,
    /// Where the next incremental search starts: the start of the current match.
    pub anchor: usize,
    pub failed: bool,
}

/// A query-replace session in progress (§9, A8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryReplace {
    pub pattern: Pattern,
    pub replacement: String,
    pub regex: bool,
    pub fold: bool,
    /// The match waiting for an answer, as `start..end`.
    pub current: Option<(usize, usize)>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal { prefix: Prefix },
    Confirm(Confirm),
    Isearch(Isearch),
    QueryReplace(QueryReplace),
    Prompt(Prompt),
}

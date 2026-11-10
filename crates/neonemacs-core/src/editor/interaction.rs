//! Interaction group: what the user is doing now. The mode, the last two command kinds,
//! the echo-area message, and the quit flag. Fields are visible only to `editor` (I-32).
//! `mode` may hold a `BufferId` as a value, but this group never dereferences it.

use crate::buffer::BufferId;
use crate::search::Pattern;
use crate::width::Metrics;
use std::path::PathBuf;

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
    /// Input before the cursor.
    pub before: String,
    /// Input after the cursor.
    pub after: String,
    pub purpose: Purpose,
}

impl Prompt {
    /// An empty prompt with the cursor after the label.
    #[must_use]
    pub fn new(label: &str, purpose: Purpose) -> Self {
        Self {
            label: label.to_owned(),
            before: String::new(),
            after: String::new(),
            purpose,
        }
    }

    /// The whole input line.
    #[must_use]
    pub fn input(&self) -> String {
        format!("{}{}", self.before, self.after)
    }

    /// The echo-area text: label and input.
    #[must_use]
    pub fn echo(&self) -> String {
        format!("{}{}{}", self.label, self.before, self.after)
    }

    /// Display column of the cursor in the echo area.
    #[must_use]
    pub fn cursor_width(&self, m: Metrics) -> usize {
        self.label
            .chars()
            .chain(self.before.chars())
            .map(|c| m.char(c))
            .sum()
    }
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
    QueryFrom { kind: MatchKind },
    QueryTo { kind: MatchKind, from: String },
}

/// Which way an incremental search moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// Whether a search pattern is literal text or a regular expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Literal,
    Regex,
}

/// A yes-or-no question that must be answered before the command continues (I-04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    SaveExternal(BufferId),
    /// `write-file` to `path`, where a file already exists (I-04).
    OverwriteFile {
        id: BufferId,
        path: PathBuf,
    },
    QuitUnsaved,
    KillModified(BufferId),
    RevertModified(BufferId),
}

/// A pending yes-or-no question and the text that asked it, so a wrong answer can repeat it (I-46).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub kind: Confirm,
    pub text: String,
}

/// An incremental search in progress (§9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isearch {
    pub direction: Direction,
    pub kind: MatchKind,
    pub text: String,
    /// Point when the search started; restored on cancel and used after backspace.
    pub origin: usize,
    /// Where the next incremental search starts: the start of the current match.
    pub anchor: usize,
    /// The current match as `start..end`, or `None` when the text is empty or nothing matches.
    pub found: Option<(usize, usize)>,
}

impl Isearch {
    /// A non-empty search text with no match.
    #[must_use]
    pub const fn failed(&self) -> bool {
        !self.text.is_empty() && self.found.is_none()
    }
}

/// A query-replace session in progress (§9, A8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryReplace {
    pub pattern: Pattern,
    pub replacement: String,
    pub kind: MatchKind,
    pub fold: bool,
    /// The match waiting for an answer, as `start..end`. A stored session always has one:
    /// the session is stored only after a match is found, and each later match replaces it.
    pub current: (usize, usize),
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal { prefix: Prefix },
    Confirm(Question),
    Isearch(Isearch),
    QueryReplace(QueryReplace),
    Prompt(Prompt),
}

impl Mode {
    /// Normal editing with no prefix key pending.
    #[must_use]
    pub const fn normal() -> Self {
        Self::Normal {
            prefix: Prefix::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Interaction {
    pub(super) mode: Mode,
    /// Kind of the command that just ran. The running command sets it.
    pub(super) last: LastCommand,
    /// Kind of the command before the running one. Chaining commands read it.
    pub(super) prev: LastCommand,
    pub(super) message: String,
    pub(super) quit: bool,
}

impl Interaction {
    pub const fn new() -> Self {
        Self {
            mode: Mode::normal(),
            last: LastCommand::Other,
            prev: LastCommand::Other,
            message: String::new(),
            quit: false,
        }
    }
}

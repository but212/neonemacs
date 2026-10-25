//! Interaction group: what the user is doing now. The mode, the last two command kinds,
//! the echo-area message, and the quit flag. Fields are visible only to `editor` (I-32).
//! `mode` may hold a `BufferId` as a value, but this group never dereferences it.

use crate::core::mode::{LastCommand, Mode, Prefix};

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
    pub fn new() -> Self {
        Self {
            mode: Mode::Normal {
                prefix: Prefix::None,
            },
            last: LastCommand::Other,
            prev: LastCommand::Other,
            message: String::new(),
            quit: false,
        }
    }
}

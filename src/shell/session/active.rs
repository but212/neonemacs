//! Background shell command state: the spinner and busy notices shown while a command runs.

use crate::core::CANCEL_MESSAGE;
use crate::shell::{BUSY_NOTICE, RUNNING, ShellTask};
use std::sync::mpsc::TryRecvError;

/// State of an active background shell command.
pub struct ActiveShell {
    pub task: ShellTask,
    pub(super) tick: usize,
    /// Set when a second command was refused because this one is still running.
    already_running: bool,
    /// The message this command last left in the echo area. Any other message is not its to replace.
    shown: String,
}

impl ActiveShell {
    #[must_use]
    pub fn new(task: ShellTask) -> Self {
        Self {
            task,
            tick: 0,
            already_running: false,
            shown: RUNNING.to_owned(),
        }
    }

    /// Records that a second command was refused. The refusal left its own message (I-24).
    pub fn set_already_running(&mut self) {
        self.already_running = true;
        self.shown = format!("{RUNNING} {BUSY_NOTICE}");
    }

    /// The command's result, once it has one. A worker that vanished without a result is an error.
    pub fn poll(&self) -> Option<Result<String, String>> {
        match self.task.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("Shell command terminated unexpectedly".to_owned()))
            }
        }
    }

    /// Whether the spinner may replace `message`: the echo area is empty, still shows the last
    /// spinner frame, or shows the cancel message that `C-g` leaves.
    #[must_use]
    pub fn owns(&self, message: &str) -> bool {
        message.is_empty() || message == self.shown || message == CANCEL_MESSAGE
    }

    /// The next spinner frame, which becomes the message this command owns.
    pub fn format_status(&mut self) -> String {
        const SPINNER: &[char] = &['|', '/', '-', '\\'];
        self.tick = self.tick.wrapping_add(1);
        let ch = SPINNER[self.tick % SPINNER.len()];
        self.shown = if self.already_running {
            format!("[Running... {ch}] {BUSY_NOTICE}")
        } else {
            format!("[Running... {ch}]")
        };
        self.shown.clone()
    }
}

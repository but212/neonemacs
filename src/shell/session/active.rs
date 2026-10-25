//! Background shell command state: the spinner and busy notices shown while a command runs.

use crate::shell::ShellTask;

/// State of an active background shell command.
pub struct ActiveShell {
    pub task: ShellTask,
    pub(super) tick: usize,
    /// Set when a second command was refused because this one is still running.
    already_running: bool,
}

impl ActiveShell {
    pub const fn new(task: ShellTask) -> Self {
        Self {
            task,
            tick: 0,
            already_running: false,
        }
    }

    pub const fn set_already_running(&mut self) {
        self.already_running = true;
    }

    pub fn format_status(&mut self) -> String {
        const SPINNER: &[char] = &['|', '/', '-', '\\'];
        self.tick = self.tick.wrapping_add(1);
        let ch = SPINNER[self.tick % SPINNER.len()];
        if self.already_running {
            format!("[Running... {ch}] A shell command is already running")
        } else {
            format!("[Running... {ch}]")
        }
    }
}

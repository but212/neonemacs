//! Background shell command state: the spinner and busy notices shown while a command runs.

use crate::shell::ShellTask;

/// Notice attached to a running shell task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellNotice {
    AlreadyRunning,
}

/// State of an active background shell command.
pub struct ActiveShell {
    pub task: ShellTask,
    pub(super) tick: usize,
    notice: Option<ShellNotice>,
}

impl ActiveShell {
    pub const fn new(task: ShellTask) -> Self {
        Self {
            task,
            tick: 0,
            notice: None,
        }
    }

    pub const fn set_notice(&mut self, notice: ShellNotice) {
        self.notice = Some(notice);
    }

    pub fn format_status(&mut self) -> String {
        const SPINNER: &[char] = &['|', '/', '-', '\\'];
        self.tick = self.tick.wrapping_add(1);
        let ch = SPINNER[self.tick % SPINNER.len()];
        match self.notice {
            Some(ShellNotice::AlreadyRunning) => {
                format!("[Running... {ch}] A shell command is already running")
            }
            None => format!("[Running... {ch}]"),
        }
    }
}

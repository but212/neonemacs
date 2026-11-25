//! Encapsulates the interactive shell session, event loop state, and terminal rendering (I-22, I-24).

mod screen;
#[cfg(test)]
mod tests;

use crate::core::CANCEL_MESSAGE;
use crate::core::effect::{Event as CoreEvent, InitLoad};
use crate::core::render::Cell;
use crate::core::step;
use crate::core::{Editor, Mode};
use crate::shell::startup::{InitSource, configure, init_source};
use crate::shell::terminal::from_terminal;
use crate::shell::{BUSY_NOTICE, RUNNING, ShellOutcome, handle, handle_interactive, perform_all};
use crossterm::event::{self, Event as TermEvent, KeyEventKind};
use mlua::Lua;
use std::io::{self, Write};
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

/// State of an active background shell command.
pub struct ActiveShell {
    pub task: mpsc::Receiver<Result<String, String>>,
    pub(super) tick: usize,
    /// Set when a second command was refused because this one is still running.
    already_running: bool,
    /// The message this command last left in the echo area. Any other message is not its to replace.
    shown: String,
}

impl ActiveShell {
    #[must_use]
    pub fn new(task: mpsc::Receiver<Result<String, String>>) -> Self {
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
        self.shown = crate::shell::BUSY_MESSAGE.to_owned();
    }

    /// The command's result, once it has one. A worker that vanished without a result is an error.
    #[must_use]
    pub fn poll(&self) -> Option<Result<String, String>> {
        match self.task.try_recv() {
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

/// The idle time after an edit that the WAL waits for (I-23).
const WAL_IDLE: Duration = Duration::from_secs(1);

/// Manages interactive editor state, background tasks, idle WAL scheduling, and screen updates.
pub struct ShellSession {
    pub ed: Editor,
    pub lua: Lua,
    active_shell: Option<ActiveShell>,
    /// When the core is told that input has been idle: one second after the last edit that left
    /// unsaved text, or after a failed flush. `None` when no WAL is due (I-23).
    wal_deadline: Option<Instant>,
    prev_rows: Vec<Vec<Cell>>,
    screen_size: (usize, usize),
}

pub(super) fn update_editor(slot: &mut Editor, f: impl FnOnce(Editor) -> Editor) {
    let ed = std::mem::take(slot);
    *slot = f(ed);
}

impl ShellSession {
    /// `ed` and `lua` are made together, by `startup::start` or by `lua::bootstrap`.
    #[must_use]
    pub fn new(ed: Editor, lua: Lua) -> Self {
        Self {
            ed,
            lua,
            active_shell: None,
            wal_deadline: None,
            prev_rows: Vec::new(),
            screen_size: (0, 0),
        }
    }

    /// Builds the settings and the Lua state again from `source`. They replace the current ones
    /// only when `source` applies whole or is absent; the buffers and windows stay (I-54, I-58).
    pub fn reload_init(&mut self, source: InitSource) {
        let lua_slot = &mut self.lua;
        let ed = std::mem::take(&mut self.ed);
        let (ed, load) = match configure(std::path::PathBuf::from(ed.cwd()), source) {
            Ok((fresh, lua, load @ (InitLoad::Applied | InitLoad::Absent))) => {
                *lua_slot = lua;
                (ed.adopt_settings(fresh), load)
            }
            Ok((_, _, load @ (InitLoad::Rejected(_) | InitLoad::Ignored))) => (ed, load),
            Err(e) => (ed, InitLoad::Rejected(e.to_string())),
        };
        let (ed, fx) = step(ed, CoreEvent::InitLoaded(load));
        self.ed = perform_all(ed, Some(&self.lua), fx);
    }

    #[must_use]
    pub const fn is_quit(&self) -> bool {
        self.ed.quit()
    }

    fn has_wal_dirty_file_buffer(&self) -> bool {
        self.ed
            .buffers()
            .iter()
            .any(crate::core::Buffer::is_wal_dirty)
    }
    fn update_ed(&mut self, f: impl FnOnce(Editor, &Lua) -> Editor) {
        let lua = &self.lua;
        update_editor(&mut self.ed, |ed| f(ed, lua));
    }

    /// Handles a terminal event (e.g. key press).
    pub fn handle_event(&mut self, event: &TermEvent) {
        self.handle_event_at(event, Instant::now());
    }

    /// Handles a terminal event with an explicit timestamp for deterministic idle WAL scheduling.
    pub fn handle_event_at(&mut self, event: &TermEvent, now: Instant) {
        if let TermEvent::Key(k) = event
            && k.kind == KeyEventKind::Press
            && let Some(key) = from_terminal(k.code, k.modifiers)
        {
            let prev_seq = self.ed.edit_seq();
            let shell_busy = self.active_shell.is_some();
            let mut outcome = ShellOutcome::None;
            self.update_ed(|ed, lua| {
                let (new_ed, out) =
                    handle_interactive(ed, Some(lua), CoreEvent::Key(key), shell_busy);
                outcome = out;
                new_ed
            });
            match outcome {
                ShellOutcome::Spawned(task) => {
                    self.active_shell = Some(ActiveShell::new(task));
                }
                ShellOutcome::BusyRejected => {
                    if let Some(active) = &mut self.active_shell {
                        active.set_already_running();
                    }
                }
                ShellOutcome::ReloadInit => self.reload_init(init_source()),
                ShellOutcome::None => {}
            }
            if !self.has_wal_dirty_file_buffer() {
                self.wal_deadline = None;
            } else if self.ed.edit_seq() != prev_seq {
                self.wal_deadline = Some(now + WAL_IDLE);
            }
        }
    }

    /// Periodic tick at an explicit timestamp `now` (I-23, I-24).
    pub fn tick_at(&mut self, now: Instant) {
        // 1. Check background shell task (only in Normal mode so active Prompt/Isearch/QueryReplace
        // echo state is never clobbered mid-input; completed output stays buffered in the channel).
        if matches!(self.ed.mode(), Mode::Normal { .. })
            && let Some(active) = &mut self.active_shell
        {
            match active.poll() {
                Some(result) => {
                    self.active_shell = None;
                    self.update_ed(|ed, lua| handle(ed, Some(lua), CoreEvent::ShellDone(result)));
                }
                None if active.owns(self.ed.message()) => {
                    let status = active.format_status();
                    self.update_ed(|ed, _| ed.set_message(status));
                }
                None => {}
            }
        }

        // 2. Idle WAL flush (1s after edit). The core decides what to write and what a failure shows.
        if self.wal_deadline.is_some_and(|due| now >= due) {
            self.update_ed(|ed, lua| handle(ed, Some(lua), CoreEvent::Idle));
            // A buffer whose write failed stays dirty, so the flush is tried again after another second.
            self.wal_deadline = self.has_wal_dirty_file_buffer().then(|| now + WAL_IDLE);
        }
    }

    /// Draws, then handles waiting events, background tasks, and WAL flush in a loop (I-22, I-24).
    ///
    /// # Errors
    /// Returns `io::Error` if terminal rendering, event polling, or reading fails.
    pub fn run(mut self, out: &mut impl Write) -> io::Result<()> {
        loop {
            self.render(out)?;
            if self.is_quit() {
                return Ok(());
            }

            self.tick_at(Instant::now());

            if event::poll(Duration::from_millis(50))? {
                self.handle_event(&event::read()?);
                while event::poll(Duration::ZERO)? {
                    self.handle_event(&event::read()?);
                }
            }
        }
    }
}

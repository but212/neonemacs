//! Encapsulates the interactive shell session, event loop state, and terminal rendering (I-22, I-24).

mod active;
mod screen;
#[cfg(test)]
mod tests;

pub use active::ActiveShell;

use crate::core::editor::{Editor, Mode};
use crate::core::effect::Event as CoreEvent;
use crate::core::step;
use crate::shell::terminal::from_terminal;
use crate::shell::wal;
use crate::shell::{ShellOutcome, handle_interactive, perform_all};
use crossterm::event::{self, Event as TermEvent, KeyEventKind};
use mlua::Lua;
use std::io::{self, Write};
use std::time::{Duration, Instant};

/// Manages interactive editor state, background tasks, idle WAL scheduling, and screen updates.
pub struct ShellSession {
    pub ed: Editor,
    pub lua: Lua,
    active_shell: Option<ActiveShell>,
    last_edit_time: Instant,
    needs_wal_flush: bool,
    prev_rows: Vec<Vec<crate::core::render::Cell>>,
    size: (usize, usize),
}

impl ShellSession {
    /// `ed` must come from `lua::bootstrap`: the bundled keymap and syntax definitions are loaded only there.
    #[must_use]
    pub fn new(ed: Editor, lua: Lua) -> Self {
        Self {
            ed,
            lua,
            active_shell: None,
            last_edit_time: Instant::now(),
            needs_wal_flush: false,
            prev_rows: Vec::new(),
            size: (0, 0),
        }
    }

    #[must_use]
    pub const fn is_quit(&self) -> bool {
        self.ed.quit()
    }

    fn has_wal_dirty_file_buffer(&self) -> bool {
        self.ed
            .buffers()
            .iter()
            .any(crate::core::buffer::Buffer::is_wal_dirty)
    }
    /// Handles a terminal event (e.g. key press).
    pub fn handle_event(&mut self, event: TermEvent) {
        self.handle_event_at(event, Instant::now());
    }

    /// Handles a terminal event with an explicit timestamp for deterministic idle WAL scheduling.
    pub fn handle_event_at(&mut self, event: TermEvent, now: Instant) {
        if let TermEvent::Key(k) = event
            && k.kind == KeyEventKind::Press
            && let Some(key) = from_terminal(k.code, k.modifiers)
        {
            let prev_seq = self.ed.edit_seq();
            let ed = std::mem::take(&mut self.ed);
            let shell_busy = self.active_shell.is_some();
            let (new_ed, outcome) =
                handle_interactive(ed, Some(&self.lua), CoreEvent::Key(key), shell_busy);
            self.ed = new_ed;
            match outcome {
                ShellOutcome::Spawned(task) => {
                    self.active_shell = Some(ActiveShell::new(task));
                }
                ShellOutcome::BusyRejected => {
                    if let Some(active) = &mut self.active_shell {
                        active.set_already_running();
                    }
                }
                ShellOutcome::None => {}
            }
            if self.ed.edit_seq() != prev_seq && self.has_wal_dirty_file_buffer() {
                self.last_edit_time = now;
                self.needs_wal_flush = true;
            } else if !self.has_wal_dirty_file_buffer() {
                self.needs_wal_flush = false;
            }
        }
    }

    /// Periodic tick for background tasks and idle WAL scheduling (I-23, I-24).
    pub fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    /// Periodic tick at an explicit timestamp `now` (I-23, I-24).
    pub fn tick_at(&mut self, now: Instant) {
        // 1. Check background shell task (only in Normal mode so active Prompt/Isearch/QueryReplace
        // echo state is never clobbered mid-input; completed output stays buffered in the channel).
        if matches!(self.ed.mode(), Mode::Normal { .. })
            && let Some(active) = &mut self.active_shell
        {
            match active.task.receiver.try_recv() {
                Ok(result) => {
                    let ed = std::mem::take(&mut self.ed);
                    let (new_ed, fx) = step(ed, CoreEvent::ShellDone(result));
                    self.ed = perform_all(new_ed, Some(&self.lua), fx);
                    self.active_shell = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    let msg = self.ed.message();
                    if msg.is_empty() || msg.starts_with("[Running") || msg == "Quit" {
                        let status = active.format_status();
                        self.ed = std::mem::take(&mut self.ed).set_message(status);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let ed = std::mem::take(&mut self.ed);
                    let (new_ed, fx) = step(
                        ed,
                        CoreEvent::ShellDone(Err(
                            "Shell command terminated unexpectedly".to_owned()
                        )),
                    );
                    self.ed = perform_all(new_ed, Some(&self.lua), fx);
                    self.active_shell = None;
                }
            }
        }

        // 2. Idle WAL flush (1s after edit)
        if self.needs_wal_flush
            && now.saturating_duration_since(self.last_edit_time) >= Duration::from_secs(1)
        {
            let mut flushed_ids = Vec::new();
            for buf in self.ed.buffers() {
                if buf.is_wal_dirty()
                    && let Some(path) = buf.file_path()
                    && wal::write_wal(
                        path,
                        buf.backing().disk(),
                        buf.eol(),
                        &buf.text().to_bytes(),
                    )
                    .is_ok()
                {
                    flushed_ids.push(buf.id());
                }
            }
            let mut ed = std::mem::take(&mut self.ed);
            for id in flushed_ids {
                ed = ed.clear_wal_dirty(id);
            }
            self.ed = ed;
            self.needs_wal_flush = false;
        }
    }

    /// Draws, then handles waiting events, background tasks, and WAL flush in a loop (I-22, I-24).
    pub fn run(mut self, out: &mut impl Write) -> io::Result<()> {
        loop {
            self.render(out)?;
            if self.is_quit() {
                return Ok(());
            }

            self.tick();

            if event::poll(Duration::from_millis(50))? {
                self.handle_event(event::read()?);
                while event::poll(Duration::ZERO)? {
                    self.handle_event(event::read()?);
                }
            }
        }
    }
}

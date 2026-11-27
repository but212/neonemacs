//! Encapsulates the interactive shell session, event loop state, and terminal rendering (I-22, I-24).

mod screen;
#[cfg(test)]
mod tests;

use crate::core::CANCEL_MESSAGE;
use crate::core::effect::{Event as CoreEvent, InitLoad};
use crate::core::render::Cell;
use crate::core::step;
use crate::core::{BufferId, Doc, Editor, Eol, Mode};
use crate::shell::recovery::{RecoverySnapshot, RecoveryWorker, recovery_dir};
use crate::shell::startup::{InitSource, configure, init_source};
use crate::shell::terminal::{TerminalGuard, from_terminal};
use crate::shell::{BUSY_NOTICE, RUNNING, ShellOutcome, handle, handle_interactive, perform_all};
use crossterm::event::{self, Event as TermEvent, KeyEventKind};
use mlua::Lua;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

const DEFAULT_IDLE_BACKUP_DELAY: Duration = Duration::from_secs(5);

struct TrackedBackup {
    doc: Doc,
    eol: Eol,
    path: Option<String>,
}

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

/// Manages interactive editor state, background tasks, and screen updates.
pub struct ShellSession {
    pub ed: Editor,
    pub lua: Lua,
    active_shell: Option<ActiveShell>,
    prev_rows: Vec<Cell>,
    screen_size: (usize, usize),
    #[cfg(unix)]
    pending_suspend: bool,
    recovery: Option<RecoveryWorker>,
    tracked_backups: BTreeMap<u32, TrackedBackup>,
    last_activity: Instant,
    idle_backup_delay: Duration,
}

pub(super) fn update_editor(slot: &mut Editor, f: impl FnOnce(Editor) -> Editor) {
    let ed = std::mem::take(slot);
    *slot = f(ed);
}

impl ShellSession {
    /// `ed` and `lua` are made together, by `startup::start` or by `lua::bootstrap`.
    #[must_use]
    pub fn new(ed: Editor, lua: Lua) -> Self {
        Self::with_recovery_dir(ed, lua, recovery_dir())
    }

    /// Creates a session using an explicit recovery directory (`None` disables background backup).
    #[must_use]
    pub fn with_recovery_dir(ed: Editor, lua: Lua, dir: Option<PathBuf>) -> Self {
        let recovery = dir.and_then(|d| RecoveryWorker::spawn(d, std::process::id()).ok());
        Self {
            ed,
            lua,
            active_shell: None,
            prev_rows: Vec::new(),
            screen_size: (0, 0),
            #[cfg(unix)]
            pending_suspend: false,
            recovery,
            tracked_backups: BTreeMap::new(),
            last_activity: Instant::now(),
            idle_backup_delay: DEFAULT_IDLE_BACKUP_DELAY,
        }
    }

    /// Overrides the idle duration before modified buffers are backed up in the background.
    pub fn set_idle_backup_delay(&mut self, delay: Duration) {
        self.idle_backup_delay = delay;
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

    fn update_ed(&mut self, f: impl FnOnce(Editor, &Lua) -> Editor) {
        let lua = &self.lua;
        update_editor(&mut self.ed, |ed| f(ed, lua));
    }

    /// Handles a terminal event (e.g. key press).
    pub fn handle_event(&mut self, event: &TermEvent) {
        if let TermEvent::Key(k) = event
            && k.kind == KeyEventKind::Press
            && let Some(key) = from_terminal(k.code, k.modifiers)
        {
            self.last_activity = Instant::now();
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
                ShellOutcome::Suspend => self.request_suspend(),
                ShellOutcome::None => {}
            }
            self.sync_clean_and_killed_buffers();
        }
    }

    fn sync_clean_and_killed_buffers(&mut self) {
        let mut stale = Vec::new();
        for (&id_raw, tracked) in &self.tracked_backups {
            let live = self.ed.buffers().iter().find(|b| b.id().0 == id_raw);
            let keep =
                live.is_some_and(|b| b.modified() && b.file_path() == tracked.path.as_deref());
            if !keep {
                stale.push(id_raw);
            }
        }
        for id_raw in stale {
            if let Some(tracked) = self.tracked_backups.remove(&id_raw)
                && let Some(worker) = &self.recovery
            {
                worker.enqueue_remove(BufferId(id_raw), tracked.path);
            }
        }
    }

    fn maybe_enqueue_idle_backups(&mut self) {
        if self.last_activity.elapsed() < self.idle_backup_delay {
            return;
        }
        let Some(worker) = &self.recovery else {
            return;
        };
        for buf in self.ed.buffers().iter() {
            if !buf.modified() {
                continue;
            }
            let Some(path) = buf.file_path() else {
                continue;
            };
            let unchanged = self.tracked_backups.get(&buf.id().0).is_some_and(|prev| {
                prev.eol == buf.eol()
                    && prev.path.as_deref() == Some(path)
                    && prev.doc == *buf.text()
            });
            if unchanged {
                continue;
            }
            let doc = buf.text().clone();
            let eol = buf.eol();
            let owned_path = Some(path.to_owned());
            worker.enqueue_write(RecoverySnapshot {
                buffer_id: buf.id(),
                path: owned_path.clone(),
                name: buf.name().to_owned(),
                doc: doc.clone(),
                eol,
            });
            self.tracked_backups.insert(
                buf.id().0,
                TrackedBackup {
                    doc,
                    eol,
                    path: owned_path,
                },
            );
        }
    }

    /// Periodic tick (I-24, I-64).
    pub fn tick(&mut self) {
        // 1. Check background shell task (only in Normal mode so active Prompt/Isearch/QueryReplace
        // echo state is never clobbered mid-input; completed output stays buffered in the channel).
        if matches!(self.ed.mode(), Mode::Normal { .. })
            && let Some(active) = &mut self.active_shell
        {
            match active.poll() {
                Some(result) => {
                    self.active_shell = None;
                    self.update_ed(|ed, lua| handle(ed, Some(lua), CoreEvent::ShellDone(result)));
                    self.sync_clean_and_killed_buffers();
                }
                None if active.owns(self.ed.message()) => {
                    let status = active.format_status();
                    self.update_ed(|ed, _| ed.set_message(status));
                }
                None => {}
            }
        }
        self.maybe_enqueue_idle_backups();
    }

    fn request_suspend(&mut self) {
        #[cfg(unix)]
        {
            self.pending_suspend = true;
        }
        #[cfg(not(unix))]
        {
            self.update_ed(|ed, _| ed.set_message("Suspend is only supported on Unix"));
        }
    }

    #[cfg(unix)]
    fn suspend(&mut self, guard: &mut TerminalGuard) -> io::Result<()> {
        guard.suspend()?;
        self.prev_rows.clear();
        Ok(())
    }

    /// Draws, then handles waiting events and background tasks in a loop (I-22, I-24).
    ///
    /// # Errors
    /// Returns `io::Error` if terminal rendering, event polling, or reading fails.
    pub fn run(
        mut self,
        out: &mut impl Write,
        #[cfg_attr(not(unix), allow(unused_variables))] guard: &mut TerminalGuard,
    ) -> io::Result<()> {
        loop {
            #[cfg(unix)]
            if self.pending_suspend {
                self.pending_suspend = false;
                self.suspend(guard)?;
            }
            self.render(out)?;
            if self.is_quit() {
                if let Some(worker) = self.recovery.take() {
                    worker.drain_and_stop();
                }
                return Ok(());
            }

            self.tick();

            match event::poll(Duration::from_millis(50)) {
                Ok(true) => loop {
                    match event::read() {
                        Ok(ev) => self.handle_event(&ev),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                        Err(e) => return Err(e),
                    }
                    match event::poll(Duration::ZERO) {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                        Err(e) => return Err(e),
                    }
                },
                Ok(false) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

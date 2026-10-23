//! Encapsulates the interactive shell session, event loop state, and terminal rendering (I-22, I-24).

use crate::core::editor::Editor;
use crate::core::effect::Event as CoreEvent;
use crate::core::file::Backing;
use crate::core::render;
use crate::core::step;
use crate::core::syntax::TokenKind;
use crate::shell::terminal::from_terminal;
use crate::shell::wal;
use crate::shell::{ShellOutcome, ShellTask, handle_interactive, perform_all};
use crossterm::event::{self, Event as TermEvent, KeyEventKind};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal;
use crossterm::{cursor, queue};
use mlua::Lua;
use std::io::{self, Write};
use std::time::{Duration, Instant};

/// Notice attached to a running shell task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellNotice {
    AlreadyRunning,
}

/// State of an active background shell command.
pub struct ActiveShell {
    pub task: ShellTask,
    tick: usize,
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

/// Manages interactive editor state, background tasks, idle WAL scheduling, and screen updates.
pub struct ShellSession {
    pub ed: Editor,
    pub lua: Lua,
    active_shell: Option<ActiveShell>,
    last_edit_time: Instant,
    needs_wal_flush: bool,
    prev_rows: Vec<String>,
    size: (usize, usize),
}

impl ShellSession {
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

    pub const fn is_quit(&self) -> bool {
        self.ed.quit
    }

    /// Handles a terminal event (e.g. key press).
    pub fn handle_event(&mut self, event: TermEvent) {
        if let TermEvent::Key(k) = event
            && k.kind == KeyEventKind::Press
            && let Some(key) = from_terminal(k.code, k.modifiers)
        {
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
                        active.set_notice(ShellNotice::AlreadyRunning);
                    }
                }
                ShellOutcome::None => {}
            }
            self.last_edit_time = Instant::now();
            self.needs_wal_flush = true;
        }
    }

    /// Periodic tick for background tasks and idle WAL scheduling (I-23, I-24).
    pub fn tick(&mut self) {
        // 1. Check background shell task
        if let Some(active) = &mut self.active_shell {
            match active.task.receiver.try_recv() {
                Ok(result) => {
                    let ed = std::mem::take(&mut self.ed);
                    let (new_ed, fx) = step(ed, CoreEvent::ShellDone(result));
                    self.ed = perform_all(new_ed, Some(&self.lua), fx);
                    self.active_shell = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.ed.message = active.format_status();
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.active_shell = None;
                }
            }
        }

        // 2. Idle WAL flush (1s after edit)
        if self.needs_wal_flush && self.last_edit_time.elapsed() >= Duration::from_secs(1) {
            for buf in &self.ed.buffers {
                if buf.modified
                    && let Backing::File { path, .. } = &buf.backing
                {
                    let _ = wal::write_wal(path, &buf.text.to_bytes());
                }
            }
            self.needs_wal_flush = false;
        }
    }

    /// Draws changed rows to the terminal.
    pub fn render(&mut self, out: &mut impl Write) -> io::Result<()> {
        let (cols, rows) = terminal::size()?;
        let (cols, rows) = (usize::from(cols), usize::from(rows));
        if (cols, rows) != self.size {
            self.prev_rows.clear();
            self.size = (cols, rows);
        }

        let ed = std::mem::take(&mut self.ed);
        let ed = render::scroll(ed, cols, rows);
        let frame = render::frame(&ed, cols, rows);
        self.ed = ed;

        for (row, line) in render::diff(&self.prev_rows, &frame.rows) {
            queue!(out, cursor::MoveTo(0, row as u16))?;
            let row_styles = frame.styles.get(row).map(|s| s.as_slice());
            print_styled_row(out, &line, row_styles)?;
        }
        queue!(
            out,
            cursor::MoveTo(frame.cursor.0 as u16, frame.cursor.1 as u16),
            cursor::Show
        )?;
        out.flush()?;
        self.prev_rows = frame.rows;
        Ok(())
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

fn print_styled_row(
    out: &mut impl Write,
    line: &str,
    styles: Option<&[TokenKind]>,
) -> io::Result<()> {
    let Some(styles) = styles else {
        return queue!(out, Print(line));
    };
    let mut current = TokenKind::Normal;
    let mut span = String::new();
    let mut style_iter = styles.iter();

    for c in line.chars() {
        let kind = style_iter.next().copied().unwrap_or(TokenKind::Normal);
        if kind != current {
            if !span.is_empty() {
                queue!(out, Print(&span))?;
                span.clear();
            }
            current = kind;
            match current {
                TokenKind::Keyword => queue!(out, SetForegroundColor(Color::Cyan))?,
                TokenKind::String => queue!(out, SetForegroundColor(Color::Green))?,
                TokenKind::Comment => queue!(out, SetForegroundColor(Color::DarkGrey))?,
                TokenKind::Number => queue!(out, SetForegroundColor(Color::Yellow))?,
                TokenKind::Normal => queue!(out, ResetColor)?,
            }
        }
        span.push(c);
    }
    if !span.is_empty() {
        queue!(out, Print(&span))?;
    }
    queue!(out, ResetColor)?;
    Ok(())
}

//! Encapsulates the interactive shell session, event loop state, and terminal rendering (I-22, I-24).

use crate::core::editor::{Editor, Mode};
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
    prev_rows: Vec<Vec<crate::core::render::Cell>>,
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
        self.ed.quit()
    }

    fn has_modified_file_buffer(&self) -> bool {
        self.ed
            .buffers()
            .iter()
            .any(|b| b.modified && matches!(b.backing, Backing::File { .. }))
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
            if self.has_modified_file_buffer() {
                self.last_edit_time = now;
                self.needs_wal_flush = true;
            } else {
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
            for buf in self.ed.buffers() {
                if buf.modified
                    && let Backing::File { path, disk, .. } = &buf.backing
                {
                    let _ = wal::write_wal(path, *disk, &buf.text.to_bytes());
                }
            }
            self.needs_wal_flush = false;
        }
    }

    /// Draws changed rows to the terminal.
    pub fn render(&mut self, out: &mut impl Write) -> io::Result<()> {
        let (cols, rows) = terminal::size()?;
        self.render_sized(out, usize::from(cols), usize::from(rows))
    }

    /// Draws changed rows for explicit terminal dimensions `(cols, rows)`.
    pub fn render_sized(
        &mut self,
        out: &mut impl Write,
        cols: usize,
        rows: usize,
    ) -> io::Result<()> {
        if (cols, rows) != self.size {
            self.prev_rows.clear();
            self.size = (cols, rows);
        }

        let ed = std::mem::take(&mut self.ed);
        let ed = render::scroll(ed, cols, rows);
        let frame = render::frame(&ed, cols, rows);
        self.ed = ed;

        for (row, cells) in frame.rows.iter().enumerate() {
            let row_changed = self.prev_rows.get(row) != Some(cells);
            if row_changed {
                queue!(out, cursor::MoveTo(0, row as u16))?;
                print_styled_row(out, cells)?;
            }
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

fn print_styled_row(out: &mut impl Write, cells: &[crate::core::render::Cell]) -> io::Result<()> {
    let mut current = TokenKind::Normal;
    let mut span = String::new();

    for cell in cells {
        if cell.style != current {
            if !span.is_empty() {
                queue!(out, Print(&span))?;
                span.clear();
            }
            current = cell.style;
            match current {
                TokenKind::Keyword => queue!(out, SetForegroundColor(Color::Cyan))?,
                TokenKind::String => queue!(out, SetForegroundColor(Color::Green))?,
                TokenKind::Comment => queue!(out, SetForegroundColor(Color::DarkGrey))?,
                TokenKind::Number => queue!(out, SetForegroundColor(Color::Yellow))?,
                TokenKind::Normal => queue!(out, ResetColor)?,
            }
        }
        span.push_str(&cell.glyph);
    }
    if !span.is_empty() {
        queue!(out, Print(&span))?;
    }
    queue!(out, ResetColor)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::commands::request_open;
    use crate::core::config::Config;
    use crate::core::editor::Buffer;
    use crate::core::text::Text;
    use crate::shell::lua;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventState, KeyModifiers, MouseEvent, MouseEventKind,
    };
    use std::fs;
    use std::sync::mpsc;

    fn make_session() -> ShellSession {
        let ed = Editor::new(Config::default());
        let lua = lua::new_state().unwrap();
        ShellSession::new(ed, lua)
    }

    fn key_event(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> TermEvent {
        TermEvent::Key(KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        })
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> TermEvent {
        key_event(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn hostile_event_filtering_ignores_release_repeat_mouse_resize_and_unmapped_keys() {
        let mut session = make_session();
        let t0 = session.last_edit_time;

        let ignored_events = [
            key_event(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ),
            key_event(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat),
            press(KeyCode::F(12), KeyModifiers::NONE),
            press(KeyCode::CapsLock, KeyModifiers::NONE),
            TermEvent::Resize(120, 40),
            TermEvent::FocusGained,
            TermEvent::FocusLost,
            TermEvent::Paste("injected".to_owned()),
            TermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        ];

        for (i, ev) in ignored_events.into_iter().enumerate() {
            session.handle_event_at(ev, t0 + Duration::from_secs(i as u64 + 1));
        }

        assert_eq!(session.ed.buffer().text.to_bytes(), b"");
        assert!(!session.ed.buffer().modified);
        assert!(!session.needs_wal_flush);
        assert_eq!(session.last_edit_time, t0);
        assert!(!session.is_quit());

        // Only KeyEventKind::Press with a mapped key takes effect
        session.handle_event_at(
            press(KeyCode::Char('x'), KeyModifiers::NONE),
            t0 + Duration::from_secs(10),
        );
        assert_eq!(session.ed.buffer().text.to_bytes(), b"x");
    }

    #[test]
    fn spinner_tick_wrapping_and_prompt_mode_echo_preservation() {
        let mut session = make_session();
        let (tx, rx) = mpsc::channel();
        let mut active = ActiveShell::new(ShellTask { receiver: rx });
        active.tick = usize::MAX;
        session.active_shell = Some(active);

        // 1. usize::MAX wrapping does not panic
        let t0 = Instant::now();
        session.tick_at(t0);
        assert_eq!(session.ed.message(), "[Running... |]");

        // 2. Entering M-x prompt while shell is running must NOT be overwritten by tick()
        session.handle_event_at(press(KeyCode::Char('x'), KeyModifiers::ALT), t0);
        assert!(matches!(session.ed.mode(), Mode::Prompt(_)));
        session.handle_event_at(press(KeyCode::Char('g'), KeyModifiers::NONE), t0);
        assert_eq!(session.ed.message(), "M-x g");

        // Tick multiple times while user is typing in prompt: prompt message stays intact
        session.tick_at(t0 + Duration::from_millis(50));
        session.tick_at(t0 + Duration::from_millis(100));
        assert_eq!(session.ed.message(), "M-x g");

        // 3. Cancel prompt with C-g -> back to Mode::Normal -> next tick resumes spinner
        session.handle_event_at(press(KeyCode::Char('g'), KeyModifiers::CONTROL), t0);
        assert!(matches!(session.ed.mode(), Mode::Normal { .. }));
        session.tick_at(t0 + Duration::from_millis(150));
        assert!(session.ed.message().starts_with("[Running... "));

        drop(tx);
    }

    #[test]
    fn background_shell_disconnect_clears_active_shell_and_reports_error() {
        let mut session = make_session();
        let (tx, rx) = mpsc::channel::<Result<String, String>>();
        session.active_shell = Some(ActiveShell::new(ShellTask { receiver: rx }));
        session.ed = session.ed.set_message("[Running... |]");

        // Simulate worker thread panic / abrupt channel drop without sending
        drop(tx);

        session.tick_at(Instant::now());
        assert!(session.active_shell.is_none());
        assert_eq!(
            session.ed.message(),
            "Shell command terminated unexpectedly"
        );
    }

    #[test]
    fn busy_rejection_attaches_notice_and_completion_delivers_multiline_output() {
        let mut session = make_session();
        let t0 = Instant::now();
        let (tx, rx) = mpsc::channel();
        session.active_shell = Some(ActiveShell::new(ShellTask { receiver: rx }));

        // Attempt to run a second shell command while first is active: M-x shell-command RET echo 2 RET
        session.handle_event_at(press(KeyCode::Char('x'), KeyModifiers::ALT), t0);
        for c in "shell-command".chars() {
            session.handle_event_at(press(KeyCode::Char(c), KeyModifiers::NONE), t0);
        }
        session.handle_event_at(press(KeyCode::Enter, KeyModifiers::NONE), t0);
        for c in "echo 2".chars() {
            session.handle_event_at(press(KeyCode::Char(c), KeyModifiers::NONE), t0);
        }
        session.handle_event_at(press(KeyCode::Enter, KeyModifiers::NONE), t0);

        assert_eq!(
            session.ed.message(),
            "[Running...] A shell command is already running"
        );

        // Next tick keeps the AlreadyRunning notice while advancing the spinner
        session.tick_at(t0 + Duration::from_millis(50));
        assert!(
            session
                .ed
                .message()
                .ends_with("] A shell command is already running")
        );

        // Complete the first task with multiline output -> creates *Shell Command Output* buffer
        tx.send(Ok("line1\nline2\n".to_owned())).unwrap();
        session.tick_at(t0 + Duration::from_millis(100));
        assert!(session.active_shell.is_none());
        assert!(
            session
                .ed
                .buffers()
                .iter()
                .any(|b| b.name == "*Shell Command Output*" && b.text.to_bytes() == b"line1\nline2")
        );
    }

    #[test]
    fn deterministic_wal_flush_boundaries_debounce_and_save_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        fs::write(&file_a, "init_a").unwrap();
        fs::write(&file_b, "init_b").unwrap();

        let mut session = make_session();
        let fx_a = request_open(&session.ed, file_a.clone(), None, true, Vec::new());
        session.ed = perform_all(session.ed, Some(&session.lua), fx_a);
        let fx_b = request_open(&session.ed, file_b.clone(), None, false, Vec::new());
        session.ed = perform_all(session.ed, Some(&session.lua), fx_b);

        let t0 = Instant::now();

        // 1. Non-modifying cursor key in clean file buffer does NOT arm WAL
        session.handle_event_at(press(KeyCode::Right, KeyModifiers::NONE), t0);
        assert!(!session.needs_wal_flush);

        // 2. Edit file_a at t0, then again at t0 + 600ms (debounce)
        session.handle_event_at(press(KeyCode::Char('!'), KeyModifiers::NONE), t0);
        assert!(session.needs_wal_flush);

        let t_edit2 = t0 + Duration::from_millis(600);
        session.handle_event_at(press(KeyCode::Char('?'), KeyModifiers::NONE), t_edit2);

        // At t_edit2 + 999ms (1599ms after t0): still 1ms short of 1s idle threshold
        session.tick_at(t_edit2 + Duration::from_millis(999));
        assert!(!wal::has_wal(&file_a));
        assert!(session.needs_wal_flush);

        // At exact t_edit2 + 1000ms: flushes only modified file_a, not clean file_b
        session.tick_at(t_edit2 + Duration::from_millis(1000));
        assert!(wal::has_wal(&file_a));
        assert!(!wal::has_wal(&file_b));
        assert!(!session.needs_wal_flush);
        assert_eq!(
            wal::read_wal(&file_a, crate::core::file::DiskState::Unknown).unwrap(),
            b"i!?nit_a"
        );

        // 3. Edit again, then save (C-x C-s) before 1s elapses -> disarms WAL flush & removes .wal
        let t_edit3 = t0 + Duration::from_secs(3);
        session.handle_event_at(press(KeyCode::Char('#'), KeyModifiers::NONE), t_edit3);
        assert!(session.needs_wal_flush);

        session.handle_event_at(
            press(KeyCode::Char('x'), KeyModifiers::CONTROL),
            t_edit3 + Duration::from_millis(100),
        );
        session.handle_event_at(
            press(KeyCode::Char('s'), KeyModifiers::CONTROL),
            t_edit3 + Duration::from_millis(150),
        );
        assert!(!wal::has_wal(&file_a));
        assert!(!session.needs_wal_flush);

        // Even after 2 seconds pass, tick_at must NOT recreate .wal for saved buffer
        session.tick_at(t_edit3 + Duration::from_secs(5));
        assert!(!wal::has_wal(&file_a));
    }

    #[test]
    fn print_styled_row_aligns_wide_hangul_and_combining_marks_without_shifting_colors() {
        // Line: `"가" let`
        // Cell widths: `"`(1, String), `가`(2, String), `"`(1, String), ` `(1, Normal), `l`(1, Keyword), `e`(1, Keyword), `t`(1, Keyword)
        // Notice `styles` has 8 entries (2 cells for `가`), while `line` has 7 chars!
        let cells = [
            crate::core::render::Cell {
                glyph: "\"".into(),
                style: TokenKind::String,
            },
            crate::core::render::Cell {
                glyph: "가".into(),
                style: TokenKind::String,
            },
            crate::core::render::Cell {
                glyph: "".into(),
                style: TokenKind::String,
            },
            crate::core::render::Cell {
                glyph: "\"".into(),
                style: TokenKind::String,
            },
            crate::core::render::Cell {
                glyph: " ".into(),
                style: TokenKind::Normal,
            },
            crate::core::render::Cell {
                glyph: "l".into(),
                style: TokenKind::Keyword,
            },
            crate::core::render::Cell {
                glyph: "e".into(),
                style: TokenKind::Keyword,
            },
            crate::core::render::Cell {
                glyph: "t".into(),
                style: TokenKind::Keyword,
            },
        ];

        let mut out = Vec::new();
        print_styled_row(&mut out, &cells).unwrap();

        let mut expected = Vec::new();
        queue!(
            &mut expected,
            SetForegroundColor(Color::Green),
            Print("\"가\""),
            ResetColor,
            Print(" "),
            SetForegroundColor(Color::Cyan),
            Print("let"),
            ResetColor
        )
        .unwrap();
        assert_eq!(out, expected);

        // Combining mark (`e` + U+0301 acute accent) inside a Keyword followed by Number `1`:
        // `e\u{0301}` occupies 1 cell in `styles` (`Keyword`), `' '` occupies 1 cell (`Normal`), `'1'` occupies 1 cell (`Number`).
        let comb_cells = [
            crate::core::render::Cell {
                glyph: "e\u{0301}".into(),
                style: TokenKind::Keyword,
            },
            crate::core::render::Cell {
                glyph: " ".into(),
                style: TokenKind::Normal,
            },
            crate::core::render::Cell {
                glyph: "1".into(),
                style: TokenKind::Number,
            },
        ];
        let mut comb_out = Vec::new();
        print_styled_row(&mut comb_out, &comb_cells).unwrap();

        let mut comb_expected = Vec::new();
        queue!(
            &mut comb_expected,
            SetForegroundColor(Color::Cyan),
            Print("e\u{0301}"),
            ResetColor,
            Print(" "),
            SetForegroundColor(Color::Yellow),
            Print("1"),
            ResetColor
        )
        .unwrap();
        assert_eq!(comb_out, comb_expected);
    }

    #[test]
    fn render_sized_caches_unchanged_rows_redraws_style_changes_and_handles_resize() {
        let mut session = make_session();
        session.ed = session.ed.with_current_buffer(|b| Buffer {
            name: "plain.txt".to_owned(),
            text: Text::from_bytes(b"let x = 42;"),
            ..b
        });

        // 1. Initial render draws all rows
        let mut out1 = Vec::new();
        session.render_sized(&mut out1, 20, 3).unwrap();
        let s1 = String::from_utf8(out1).unwrap();
        assert!(s1.contains("let x = 42;"));
        // plain.txt has no syntax color (no Cyan \x1b[38;5;14m or \x1b[36m)
        let mut cyan_probe = Vec::new();
        queue!(&mut cyan_probe, SetForegroundColor(Color::Cyan)).unwrap();
        let cyan_seq = String::from_utf8(cyan_probe).unwrap();
        assert!(!s1.contains(&cyan_seq));

        // 2. Second identical render emits NO row text, only cursor movement
        let mut out2 = Vec::new();
        session.render_sized(&mut out2, 20, 3).unwrap();
        let s2 = String::from_utf8(out2).unwrap();
        assert!(!s2.contains("let x = 42;"));

        // 3. Rename buffer to `code.rs` while keeping row 0 text identical ("let x = 42;")
        // Row 0 must be redrawn because `styles[0]` changed from Normal to Keyword/Number!
        session.ed = session.ed.with_current_buffer(|b| Buffer {
            name: "code.rs".to_owned(),
            ..b
        });
        let mut out3 = Vec::new();
        session.render_sized(&mut out3, 20, 3).unwrap();
        let s3 = String::from_utf8(out3).unwrap();
        assert!(s3.contains("let"));
        assert!(s3.contains(&cyan_seq));

        // 4. Resize invalidates cache and forces full redraw even when editor state is unchanged
        let mut out4 = Vec::new();
        session.render_sized(&mut out4, 25, 4).unwrap();
        let s4 = String::from_utf8(out4).unwrap();
        assert!(s4.contains("let"));

        // 5. Degenerate 0x0 and 1x1 dimensions never panic
        let mut out_zero = Vec::new();
        session.render_sized(&mut out_zero, 0, 0).unwrap();
        session.render_sized(&mut out_zero, 1, 1).unwrap();
    }
}

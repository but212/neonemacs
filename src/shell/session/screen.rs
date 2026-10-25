//! Terminal screen drawing: redraws only the rows that changed, and prints styled cells.

use super::ShellSession;
use crate::core::render;
use crate::core::syntax::TokenKind;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::{cursor, queue, terminal};
use std::io::{self, Write};

impl ShellSession {
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
}

pub(super) fn print_styled_row(
    out: &mut impl Write,
    cells: &[crate::core::render::Cell],
) -> io::Result<()> {
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
                TokenKind::Type => queue!(out, SetForegroundColor(Color::Magenta))?,
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

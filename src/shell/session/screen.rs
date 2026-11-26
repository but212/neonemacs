//! Terminal screen drawing: redraws only the rows that changed, and prints styled cells.

use super::{ShellSession, update_editor};
use crate::core::TokenKind;
use crate::core::render::{self, Cell, Frame};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::{cursor, queue, terminal};
use std::io::{self, Write};

impl ShellSession {
    /// Forces the next render to redraw every row.
    #[cfg_attr(not(any(unix, test)), allow(dead_code))]
    pub(super) fn invalidate_all(&mut self) {
        self.prev_rows.clear();
    }

    /// Draws changed rows to the terminal.
    ///
    /// # Errors
    /// Returns `io::Error` if querying terminal size or drawing fails.
    pub fn render(&mut self, out: &mut impl Write) -> io::Result<()> {
        let (cols, rows) = terminal::size()?;
        self.render_sized(out, usize::from(cols), usize::from(rows))
    }

    /// Draws changed rows for explicit terminal dimensions `(cols, rows)`.
    ///
    /// # Errors
    /// Returns `io::Error` if queueing commands or flushing `out` fails.
    pub fn render_sized(
        &mut self,
        out: &mut impl Write,
        cols: usize,
        rows: usize,
    ) -> io::Result<()> {
        update_editor(&mut self.ed, |ed| render::scroll(ed, cols, rows));
        let frame = render::frame(&self.ed, cols, rows);
        self.paint(frame, out, cols, rows)
    }

    /// Draws the rows of `frame` that differ from the last frame, then the cursor. A new size
    /// redraws everything.
    fn paint(
        &mut self,
        frame: Frame,
        out: &mut impl Write,
        cols: usize,
        rows: usize,
    ) -> io::Result<()> {
        if (cols, rows) != self.screen_size {
            self.prev_rows.clear();
            self.screen_size = (cols, rows);
        }
        for (row, cells) in frame.rows.iter().enumerate() {
            if self.prev_rows.get(row) != Some(cells) {
                queue!(
                    out,
                    cursor::MoveTo(0, u16::try_from(row).unwrap_or(u16::MAX))
                )?;
                print_styled_row(out, cells)?;
            }
        }
        queue!(
            out,
            cursor::MoveTo(
                u16::try_from(frame.cursor.0).unwrap_or(u16::MAX),
                u16::try_from(frame.cursor.1).unwrap_or(u16::MAX),
            ),
            cursor::Show
        )?;
        out.flush()?;
        self.prev_rows = frame.rows;
        Ok(())
    }
}

pub(super) fn print_styled_row(out: &mut impl Write, cells: &[Cell]) -> io::Result<()> {
    let mut current = TokenKind::Normal;
    let mut reversed = false;
    let mut span = String::new();

    for cell in cells {
        if cell.style != current || cell.reverse != reversed {
            if !span.is_empty() {
                queue!(out, Print(&span))?;
                span.clear();
            }
            if cell.reverse != reversed {
                reversed = cell.reverse;
                let attribute = if reversed {
                    Attribute::Reverse
                } else {
                    Attribute::NoReverse
                };
                queue!(out, SetAttribute(attribute))?;
            }
        }
        if cell.style != current {
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
    if reversed {
        queue!(out, SetAttribute(Attribute::NoReverse))?;
    }
    queue!(out, ResetColor)?;
    Ok(())
}

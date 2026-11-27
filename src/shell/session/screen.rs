//! Terminal screen drawing: redraws only the rows that changed, and prints styled cells.

use super::{ShellSession, update_editor};
use crate::core::TokenKind;
use crate::core::render as core_render;
use crate::shell::render::{Cell, render_into};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::{cursor, queue, terminal};
use std::io::{self, Write};

impl ShellSession {
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
        update_editor(&mut self.ed, |ed| core_render::scroll(ed, cols, rows));
        let cursor_pos = render_into(
            &self.ed,
            cols,
            rows,
            &mut self.curr_cells,
            &mut self.render_scratch,
        );
        self.paint(cursor_pos, out, cols, rows)
    }

    /// Draws the rows of `curr_cells` that differ from `prev_cells`, then the cursor. A new size
    /// redraws everything.
    fn paint(
        &mut self,
        cursor_pos: (usize, usize),
        out: &mut impl Write,
        cols: usize,
        rows: usize,
    ) -> io::Result<()> {
        if (cols, rows) != self.screen_size {
            self.prev_cells.clear();
            self.screen_size = (cols, rows);
        }
        if cols > 0 {
            let mut span = String::new();
            for (row, cells) in self.curr_cells.chunks_exact(cols).enumerate() {
                let prev_start = row * cols;
                let prev = self.prev_cells.get(prev_start..prev_start + cols);
                if prev != Some(cells) {
                    queue!(
                        out,
                        cursor::MoveTo(0, u16::try_from(row).unwrap_or(u16::MAX))
                    )?;
                    print_styled_row(out, cells, &mut span)?;
                }
            }
        }
        queue!(
            out,
            cursor::MoveTo(
                u16::try_from(cursor_pos.0).unwrap_or(u16::MAX),
                u16::try_from(cursor_pos.1).unwrap_or(u16::MAX),
            ),
            cursor::Show
        )?;
        out.flush()?;
        std::mem::swap(&mut self.curr_cells, &mut self.prev_cells);
        Ok(())
    }
}

pub(super) fn print_styled_row(
    out: &mut impl Write,
    cells: &[Cell],
    span: &mut String,
) -> io::Result<()> {
    span.clear();
    let mut current = TokenKind::Normal;
    let mut reversed = false;

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

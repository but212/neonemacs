//! Shared utility functions and editing helpers across command submodules.

use crate::core::editor::{Confirm, Editor, Mode};
use crate::core::effect::{Effect, Effects, ReadTarget};
use crate::core::text::Text;
use crate::core::undo::Edit;
use crate::core::width::unit_width;
use std::path::PathBuf;

pub(crate) const fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n')
}

/// Display column of `pos` from the start of its line.
pub fn column(text: &Text, pos: usize, tab: usize) -> usize {
    let mut col = 0;
    let mut p = text.line_start(pos);
    while p < pos {
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        col += unit_width(unit, col, tab);
        p += n;
    }
    col
}

/// Position on the line starting at `line` whose column is at most `col`.
pub(crate) fn pos_at_column(text: &Text, line: usize, col: usize, tab: usize) -> usize {
    let end = text.line_end(line);
    let (mut p, mut c) = (line, 0);
    while p < end {
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        let w = unit_width(unit, c, tab);
        if c + w > col {
            break;
        }
        c += w;
        p += n;
    }
    p
}

/// Display width of `start..end`, with tabs measured from column 0.
pub(crate) fn span_width(text: &Text, start: usize, end: usize, tab: usize) -> usize {
    let mut col = 0;
    let mut p = start;
    while p < end {
        let Some((unit, n)) = text.unit_at(p) else {
            break;
        };
        col += unit_width(unit, col, tab);
        p += n;
    }
    col
}

/// The paragraph around `p`: lines between blank lines. `None` on a blank line.
pub(crate) fn paragraph_at(text: &Text, p: usize) -> Option<(usize, usize)> {
    let blank = |line: usize| {
        let end = text.line_end(line);
        (line..end).all(|i| matches!(text.byte_at(i), b' ' | b'\t'))
    };
    let line = text.line_start(p);
    if blank(line) {
        return None;
    }
    let mut start = line;
    while start > 0 {
        let prev = text.line_start(start - 1);
        if blank(prev) {
            break;
        }
        start = prev;
    }
    let mut end = text.line_end(p);
    while end < text.len() {
        let next = end + 1;
        if blank(next) {
            break;
        }
        end = text.line_end(next);
    }
    Some((start, end))
}

pub(crate) fn ask(mut ed: Editor, question: Confirm, text: &str) -> Editor {
    ed.mode = Mode::Confirm(question);
    ed.message = text.to_owned();
    ed
}

/// Deletes `start..end`. Returns the editor and the removed bytes.
pub(crate) fn delete_range(ed: Editor, start: usize, end: usize) -> (Editor, Vec<u8>) {
    let bytes = ed.buffer().text.bytes(start..end);
    let (ed, _) = ed.apply(Edit::Delete {
        pos: start,
        bytes: bytes.clone(),
    });
    (ed, bytes)
}

/// Inserts `bytes` at `pos` and leaves point after them.
pub(crate) fn insert_at(ed: Editor, pos: usize, bytes: &[u8]) -> Editor {
    let (ed, applied) = ed.apply(Edit::Insert {
        pos,
        bytes: bytes.to_vec(),
    });
    if applied.is_some() {
        ed.move_point(pos + bytes.len())
    } else {
        ed
    }
}

/// Asks the shell to read `path` for a new buffer. The result comes back as `FileRead`.
/// `line` moves point to that line once the file is loaded (`+n`). `show` selects the buffer.
pub fn request_open(
    ed: &Editor,
    path: PathBuf,
    line: Option<usize>,
    show: bool,
    mut fx: Effects,
) -> Effects {
    fx.push(Effect::ReadFile {
        target: ReadTarget::Open { line, show },
        path,
        max_size: ed.config.max_file_size,
    });
    fx
}

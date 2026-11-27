//! Pure editor transitions. Nothing here touches the file system, a process, Lua, or the terminal (I-14).

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::ops::ControlFlow;
use weave_tree::LineBreaks;

mod buffer;
mod commands;
mod config;
mod dispatch;
mod editor;
pub mod effect;
mod eol;
mod file;
mod keymap;
pub mod render;
#[cfg(test)]
mod scenarios;
mod search;
mod syntax;
#[cfg(test)]
mod test_support;
mod undo;
pub mod width;
mod window;

pub use buffer::{Buffer, BufferId};
pub use commands::{confirmed, is_builtin, record_init_error, request_open, switch_to};
pub use config::{Config, Setting, ShellSpec, Toggles, Value};
pub use dispatch::command;
pub use editor::{CANCEL_MESSAGE, Confirm, Editor, LastCommand, Mode, Prefix, Window};
pub use eol::{Eol, EolStreamDecoder};
pub use file::{Backing, DiskState, FilePath, RawFile, loaded_from};
pub use keymap::{AliasTarget, Code, Key, parse_key_seq, parse_keys, parse_single_char};
pub use syntax::{
    LexState, SyntaxDef, SyntaxRegistry, TokenKind, highlight_line, highlight_line_with_state,
    is_ident_byte,
};
pub use undo::Edit;
pub use weave_tree::{CHUNK_CAP, DocError, MAX_SLICE_BYTES, Unit};
pub use width::str_width;

pub type Doc = weave_tree::WeaveTree<LineBreaks>;
pub type DocBuilder = weave_tree::WeaveTreeBuilder<LineBreaks>;
pub type Replace = weave_tree::Replace<LineBreaks>;

#[must_use]
pub fn unit_char(unit: Unit) -> char {
    match unit {
        Unit::Char(c) => c,
        Unit::Raw(b) => char::from(b),
    }
}

/// Total 1-based line count (`1` when empty).
#[must_use]
pub fn total_lines(doc: &Doc) -> usize {
    1 + doc.total_metric()
}

/// Maps a 0-based line index to its starting byte offset.
///
/// # Errors
/// Returns `DocError::OutOfBounds` if `line` is past the last line.
pub fn line_to_offset(doc: &Doc, line: usize) -> Result<usize, DocError> {
    doc.metric_to_byte(line)
}

/// Maps a valid `Unit`-boundary byte offset to its 0-based line index.
///
/// # Errors
/// Returns `DocError` if `offset` is out of bounds or not a `Unit` boundary.
pub fn offset_to_line(doc: &Doc, offset: usize) -> Result<usize, DocError> {
    doc.byte_to_metric(offset)
}

/// 0-based line index containing `pos` (clamped and floored to a `Unit` boundary).
#[must_use]
pub fn line_at(doc: &Doc, pos: usize) -> usize {
    let p = doc.floor_boundary(pos.min(doc.total_bytes()));
    offset_to_line(doc, p).expect("floor_boundary is valid")
}

/// Starting byte offset of the line containing `pos` (clamped and floored to a `Unit` boundary).
#[must_use]
pub fn line_start(doc: &Doc, pos: usize) -> usize {
    line_to_offset(doc, line_at(doc, pos)).expect("line < total_lines")
}

/// Ending byte offset (excluding `'\n'`) of the line containing `pos`.
#[must_use]
pub fn line_end(doc: &Doc, pos: usize) -> usize {
    let total = doc.total_bytes();
    let line = line_at(doc, pos);
    if line + 1 < total_lines(doc) {
        line_to_offset(doc, line + 1).expect("line + 1 < total_lines") - 1
    } else {
        total
    }
}

/// Starting byte offset of 1-based `one_based_line`, clamped to `[1, total_lines(doc)]`.
#[must_use]
pub fn goto_line_offset(doc: &Doc, one_based_line: usize) -> usize {
    let line_idx = one_based_line.saturating_sub(1).min(total_lines(doc) - 1);
    line_to_offset(doc, line_idx).expect("line < total_lines")
}

/// Streams all bytes of `range` from `doc` into a `Vec<u8>`.
#[must_use]
pub fn doc_range_bytes(doc: &Doc, range: core::ops::Range<usize>) -> Vec<u8> {
    let total = doc.total_bytes();
    let start = range.start.min(total);
    let end = range.end.min(total);
    if start >= end {
        return Vec::new();
    }
    let mut offset = doc.floor_boundary(start);
    let mut bytes = Vec::with_capacity(end - start);
    let _ = doc.try_for_each_slice(offset..doc.ceil_boundary(end), |chunk| {
        let lo = start.saturating_sub(offset);
        let hi = (end - offset).min(chunk.len());
        bytes.extend_from_slice(&chunk[lo..hi]);
        offset += chunk.len();
        ControlFlow::<()>::Continue(())
    });
    bytes
}

/// Copies at most one small chunk rather than pinning larger backing allocations.
#[must_use]
pub fn compact_small(doc: Doc) -> Doc {
    let len = doc.total_bytes();
    if len > CHUNK_CAP || doc.backing_bytes() <= CHUNK_CAP {
        return doc;
    }
    let bytes = doc_range_bytes(&doc, 0..len);
    Doc::from_bytes(&bytes)
}

use crate::effect::{Effects, Event};

/// Applies `event` to the editor. Returns the new editor and the effects the shell must perform.
#[must_use]
pub fn step(ed: Editor, event: Event) -> (Editor, Effects) {
    let fx = Vec::new();
    match event {
        Event::Key(key) => dispatch::handle_key(ed, key, fx),
        Event::Run(name) => dispatch::run_named(ed, &name, fx),
        Event::Disk {
            target,
            path,
            state,
        } => commands::disk_checked(ed, target, &path, state, fx),
        Event::Written { id, path, result } => commands::written(ed, id, &path, result, fx),
        Event::FileRead {
            target,
            path,
            result,
        } => commands::file_read(ed, target, path, result, fx),
        Event::RecoveryProbed { id, rec } => (commands::recovery_probed(ed, id, rec), fx),
        Event::ShellDone(result) => (commands::shell_done(ed, result), fx),
        Event::LuaDone(result) => (commands::lua_done(ed, result), fx),
        Event::InitLoaded(load) => (commands::init_loaded(ed, load), fx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::effect::{Effect, Event};
    use crate::keymap::{Code, Key};

    #[test]
    fn step_returns_editor_and_effects() {
        let ed = Editor::new(Config::default());
        let (ed, fx) = step(ed, Event::Key(Key::plain(Code::Char('a'))));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"a"));
        assert_eq!(fx, [] as [Effect; 0]);
    }
}

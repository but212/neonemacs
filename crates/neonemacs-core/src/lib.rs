//! Pure editor transitions. Nothing here touches the file system, a process, Lua, or the terminal (I-14).

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::ops::ControlFlow;
use weave_tree::summary::{LineBreakMetric as CoreLineBreakMetric, LineBreaks, count_line_breaks};
use weave_tree::{Metric, Summary};

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
mod width;
mod window;

pub use buffer::{
    Buffer, BufferId, MAX_WAL_DELTA_BYTES, MAX_WAL_DELTA_RECORDS, WAL_DELTA_RECORD_BYTES,
    WalBaseDigest, wal_base_digest,
};
pub use commands::{confirmed, is_builtin, record_init_error, request_open, switch_to};
pub use config::{Config, Setting, ShellSpec, Toggles, Value};
pub use dispatch::command;
pub use editor::{CANCEL_MESSAGE, Confirm, Editor, LastCommand, Mode, Prefix, Window};
pub use eol::{Eol, EolStreamDecoder};
pub use file::{Backing, DiskState, FilePath, RawFile, Read, loaded_from};
pub use keymap::{AliasTarget, Code, Key, KeyError, ctrl_key, parse_key_seq, parse_keys};
pub use syntax::{SyntaxDef, SyntaxRegistry, TokenKind, highlight_line};
pub use undo::Edit;
pub use weave_tree::{CHUNK_CAP, DocError, MAX_SLICE_BYTES, Unit};
pub use width::str_width;

/// Editor-specific summary combining `'\n'` line breaks and Undo backing-buffer bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorSummary {
    pub line_breaks: usize,
    pub backing_bytes: usize,
}

impl Summary for EditorSummary {
    #[inline]
    fn from_slice(bytes: &[u8]) -> Self {
        Self {
            line_breaks: count_line_breaks(bytes),
            backing_bytes: 0,
        }
    }

    #[inline]
    fn with_backing_len(mut self, backing_len: usize) -> Self {
        self.backing_bytes = backing_len;
        self
    }

    #[inline]
    fn combine(&self, other: &Self) -> Self {
        Self {
            line_breaks: self.line_breaks + other.line_breaks,
            backing_bytes: self.backing_bytes.saturating_add(other.backing_bytes),
        }
    }

    #[inline]
    fn split_pair(&self, left: &[u8], right: &[u8]) -> (Self, Self) {
        let (l, r) = LineBreaks(self.line_breaks).split_pair(left, right);
        (
            Self {
                line_breaks: l.0,
                backing_bytes: 0,
            },
            Self {
                line_breaks: r.0,
                backing_bytes: 0,
            },
        )
    }
}

/// Projects `EditorSummary::line_breaks` for `O(log P)` line/byte coordinate mapping.
#[derive(Clone, Copy, Debug, Default)]
pub struct LineBreakMetric;

impl Metric<EditorSummary> for LineBreakMetric {
    #[inline]
    fn measure(summary: &EditorSummary) -> usize {
        summary.line_breaks
    }

    #[inline]
    fn from_byte_offset(slice: &[u8], byte_offset: usize) -> usize {
        CoreLineBreakMetric::from_byte_offset(slice, byte_offset)
    }

    #[inline]
    fn to_byte_offset(slice: &[u8], metric_offset: usize) -> usize {
        CoreLineBreakMetric::to_byte_offset(slice, metric_offset)
    }
}

pub type Doc = weave_tree::WeaveTree<EditorSummary>;
pub type DocBuilder = weave_tree::WeaveTreeBuilder<EditorSummary>;
pub type Replace = weave_tree::Replace<EditorSummary>;

/// Editor-domain extensions on `Doc` (`total_lines`, line/offset mapping, Undo accounting).
pub trait EditorDocExt: Sized {
    /// Total 1-based line count (`1` when empty).
    fn total_lines(&self) -> usize;
    /// Maps a 0-based line index to its starting byte offset.
    fn line_to_offset(&self, line: usize) -> Result<usize, DocError>;
    /// Maps a valid `Unit`-boundary byte offset to its 0-based line index.
    fn offset_to_line(&self, offset: usize) -> Result<usize, DocError>;
    /// Conservative backing-allocation byte charge for Undo budget accounting.
    fn backing_bytes(&self) -> usize;
    /// Copies at most one small chunk rather than pinning larger backing allocations.
    fn compact_small(self) -> Self;
}

impl EditorDocExt for Doc {
    #[inline]
    fn total_lines(&self) -> usize {
        1 + self.total_metric::<LineBreakMetric>()
    }

    #[inline]
    fn line_to_offset(&self, line: usize) -> Result<usize, DocError> {
        let total_lines = self.total_lines();
        if line >= total_lines {
            return Err(DocError::LineOutOfBounds { line, total_lines });
        }
        self.metric_to_byte::<LineBreakMetric>(line)
    }

    #[inline]
    fn offset_to_line(&self, offset: usize) -> Result<usize, DocError> {
        self.byte_to_metric::<LineBreakMetric>(offset)
    }

    #[inline]
    fn backing_bytes(&self) -> usize {
        self.summary().backing_bytes
    }

    fn compact_small(self) -> Self {
        let len = self.total_bytes();
        if len > CHUNK_CAP || self.backing_bytes() <= CHUNK_CAP {
            return self;
        }
        let mut bytes = Vec::with_capacity(len);
        let _ = self.try_for_each_slice(0..len, |chunk| {
            bytes.extend_from_slice(chunk);
            ControlFlow::<()>::Continue(())
        });
        Self::from_bytes(&bytes)
    }
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
        Event::WalRemoved => (ed, fx),
        Event::Idle => commands::idle(ed, fx),
        Event::WalFlushed(writes) => (commands::wal_flushed(ed, writes), fx),
        Event::WalProbed { path, line, probe } => commands::wal_probed(ed, &path, line, probe, fx),
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

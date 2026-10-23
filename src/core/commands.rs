//! Built-in commands by name (§4). Lua commands join this table through `lua.rs`.
//! Every command takes the editor and its effects by value and returns both.

use crate::core::editor::{
    Buffer, BufferId, Confirm, Dir, Editor, Isearch, LastCommand, Layout, Mode, Prefix, Prompt,
    Purpose, QueryReplace, Window, snap,
};
use crate::core::effect::{self, Effect, Effects, ReadTarget};
use crate::core::file::{Backing, DiskState, LoadExtent, loaded_from};
use crate::core::keymap::{Code, Key};
use crate::core::search::{self, Pattern};
use crate::core::text::Text;
use crate::core::undo::{Edit, UndoLog};
use crate::core::width::unit_width;
use std::path::PathBuf;

pub type Builtin = fn(Editor, Effects) -> (Editor, Effects);

/// Every built-in command, by name. Lua commands are added through `lua.rs`.
pub const BUILTINS: &[(&str, Builtin)] = &[
    ("undo", Editor::undo as Builtin),
    ("fill-paragraph", fill_paragraph as Builtin),
    ("shell-command", shell_command_cmd as Builtin),
    ("forward-char", forward_char as Builtin),
    ("backward-char", backward_char as Builtin),
    ("next-line", next_line as Builtin),
    ("previous-line", previous_line as Builtin),
    ("move-beginning-of-line", move_beginning_of_line as Builtin),
    ("move-end-of-line", move_end_of_line as Builtin),
    ("delete-backward-char", delete_backward_char as Builtin),
    ("delete-char", delete_char as Builtin),
    ("set-mark-command", set_mark_command as Builtin),
    ("kill-region", kill_region as Builtin),
    ("kill-ring-save", kill_ring_save as Builtin),
    ("kill-line", kill_line as Builtin),
    ("yank", yank as Builtin),
    ("yank-pop", yank_pop as Builtin),
    ("save-buffer", save_buffer_cmd as Builtin),
    ("split-window-below", split_window_below as Builtin),
    ("split-window-right", split_window_right as Builtin),
    ("other-window", other_window as Builtin),
    ("delete-other-windows", delete_other_windows as Builtin),
    ("isearch-forward", isearch_forward as Builtin),
    ("isearch-backward", isearch_backward as Builtin),
    ("isearch-forward-regexp", isearch_forward_regexp as Builtin),
    (
        "isearch-backward-regexp",
        isearch_backward_regexp as Builtin,
    ),
    (
        "save-buffers-kill-emacs",
        save_buffers_kill_emacs as Builtin,
    ),
    (
        "exchange-point-and-mark",
        exchange_point_and_mark as Builtin,
    ),
    ("find-file", find_file_cmd as Builtin),
    ("write-file", write_file_cmd as Builtin),
    ("switch-to-buffer", switch_to_buffer_cmd as Builtin),
    ("kill-buffer", kill_buffer_cmd as Builtin),
    ("goto-line", goto_line_cmd as Builtin),
    ("query-replace", query_replace_cmd as Builtin),
    ("query-replace-regexp", query_replace_regexp_cmd as Builtin),
    ("revert-buffer", revert_buffer as Builtin),
    ("view-mode", view_mode as Builtin),
    (
        "execute-extended-command",
        execute_extended_command as Builtin,
    ),
];

pub fn find(name: &str) -> Option<Builtin> {
    BUILTINS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

fn forward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let next = ed.buffer().text.next_boundary(p);
    (ed.move_point(next), fx)
}

fn backward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let ed = if p > 0 {
        let prev = ed.buffer().text.prev_boundary(p);
        ed.move_point(prev)
    } else {
        ed
    };
    (ed, fx)
}

fn move_beginning_of_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let start = ed.buffer().text.line_start(p);
    (ed.move_point(start), fx)
}

fn move_end_of_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let end = ed.buffer().text.line_end(p);
    (ed.move_point(end), fx)
}

fn next_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (vertical(ed, true), fx)
}

fn previous_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (vertical(ed, false), fx)
}

/// Moves to the goal column on the next or previous line. The goal survives
/// consecutive vertical moves.
fn vertical(mut ed: Editor, down: bool) -> Editor {
    let tab = usize::from(ed.config.tab_width);
    let p = ed.window().point;
    let text = &ed.buffer().text;
    let goal = match ed.prev {
        LastCommand::Vertical { col } => col,
        _ => column(text, p, tab),
    };
    let line = text.line_start(p);
    let target = if down {
        let nl = text.line_end(p);
        (nl < text.len()).then_some(Ok(nl + 1))
    } else {
        (line > 0).then(|| Err(text.line_start(line - 1)))
    };
    let target = match target {
        Some(Ok(start) | Err(start)) => Some(pos_at_column(text, start, goal, tab)),
        None => None,
    };
    let mut ed = match target {
        Some(pos) => ed.move_point(pos),
        None => {
            ed.message = if down {
                "End of buffer"
            } else {
                "Beginning of buffer"
            }
            .to_owned();
            ed
        }
    };
    ed.last = LastCommand::Vertical { col: goal };
    ed
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
fn pos_at_column(text: &Text, line: usize, col: usize, tab: usize) -> usize {
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

fn delete_backward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    if p == 0 {
        return (ed, fx);
    }
    let start = ed.buffer().text.prev_boundary(p);
    (delete_range(ed, start, p).0, fx)
}

fn delete_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    if p >= ed.buffer().text.len() {
        return (ed, fx);
    }
    let end = ed.buffer().text.next_boundary(p);
    (delete_range(ed, p, end).0, fx)
}

/// Deletes `start..end`. Returns the editor and the removed bytes.
fn delete_range(ed: Editor, start: usize, end: usize) -> (Editor, Vec<u8>) {
    let bytes = ed.buffer().text.bytes(start..end);
    let (ed, _) = ed.apply(Edit::Delete {
        pos: start,
        bytes: bytes.clone(),
    });
    (ed, bytes)
}

/// Inserts `bytes` at `pos` and leaves point after them.
fn insert_at(ed: Editor, pos: usize, bytes: &[u8]) -> Editor {
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

fn set_mark_command(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    ed = ed.with_current_buffer(|b| Buffer { mark: Some(p), ..b });
    ed.message = "Mark set".to_owned();
    (ed, fx)
}

fn exchange_point_and_mark(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some(mark) = ed.buffer().mark else {
        ed.message = "No mark set in this buffer".to_owned();
        return (ed, fx);
    };
    let p = ed.window().point;
    let ed = ed.with_current_buffer(|b| Buffer { mark: Some(p), ..b });
    (ed.move_point(mark), fx)
}

/// The region between mark and point, as `start..end`.
fn region(ed: &Editor) -> Option<(usize, usize)> {
    let p = ed.window().point;
    let m = ed.buffer().mark?;
    Some((p.min(m), p.max(m)))
}

fn kill_region(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some((start, end)) = region(&ed) else {
        ed.message = "The mark is not set now".to_owned();
        return (ed, fx);
    };
    if start >= end {
        return (ed, fx);
    }
    let (ed, bytes) = delete_range(ed, start, end);
    let ed = ed.with_current_buffer(|b| Buffer { mark: None, ..b });
    (push_kill(ed, bytes), fx)
}

fn kill_ring_save(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some((start, end)) = region(&ed) else {
        ed.message = "The mark is not set now".to_owned();
        return (ed, fx);
    };
    let bytes = ed.buffer().text.bytes(start..end);
    ed.kill_ring.push(bytes);
    (ed, fx)
}

/// Kills to the end of the line, or the newline when already at the end.
fn kill_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let text = &ed.buffer().text;
    let line_end = text.line_end(p);
    let end = if line_end == p && p < text.len() {
        p + 1
    } else {
        line_end
    };
    if p >= end {
        return (ed, fx);
    }
    let (ed, bytes) = delete_range(ed, p, end);
    (push_kill(ed, bytes), fx)
}

/// Adds killed bytes to the kill ring. Consecutive kills append to one entry (I-09).
fn push_kill(mut ed: Editor, bytes: Vec<u8>) -> Editor {
    match ed.kill_ring.last_mut() {
        Some(top) if ed.prev == LastCommand::Kill => top.extend(bytes),
        _ => ed.kill_ring.push(bytes),
    }
    ed.last = LastCommand::Kill;
    ed
}

fn yank(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some(bytes) = ed.kill_ring.last().cloned() else {
        ed.message = "Kill ring is empty".to_owned();
        return (ed, fx);
    };
    let p = ed.window().point;
    let mut ed = insert_at(ed, p, &bytes);
    ed.last = LastCommand::Yank {
        start: p,
        end: p + bytes.len(),
    };
    (ed, fx)
}

/// Replaces the last yank with the previous kill-ring entry. Valid only right after `yank`.
fn yank_pop(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let LastCommand::Yank { start, end } = ed.prev else {
        ed.message = "Previous command was not a yank".to_owned();
        return (ed, fx);
    };
    let (mut ed, _) = delete_range(ed, start, end);
    ed.kill_ring.rotate_right(1);
    let bytes = ed.kill_ring.last().cloned().unwrap_or_default();
    let mut ed = insert_at(ed, start, &bytes);
    ed.last = LastCommand::Yank {
        start,
        end: start + bytes.len(),
    };
    (ed, fx)
}

fn save_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    save_buffer(ed, id, fx)
}

fn save_buffers_kill_emacs(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    if ed.buffers.iter().any(|b| b.modified) {
        let ed = ask(
            ed,
            Confirm::QuitUnsaved,
            "Modified buffers exist; quit anyway? (y or n)",
        );
        (ed, fx)
    } else {
        ed.quit = true;
        (ed, fx)
    }
}

fn ask(mut ed: Editor, question: Confirm, text: &str) -> Editor {
    ed.mode = Mode::Confirm(question);
    ed.message = text.to_owned();
    ed
}

fn file_state(ed: &Editor, id: BufferId) -> Option<(PathBuf, DiskState, LoadExtent)> {
    match &ed.buffer_by_id(id).backing {
        Backing::File { path, disk, extent } => Some((path.clone(), *disk, *extent)),
        Backing::None => None,
    }
}

/// Saves `id`. The checks run in I-04 order: truncated file, then external change.
/// A file-backed buffer asks the shell for its disk state first (`StatDisk`).
pub fn save_buffer(mut ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    let Some((path, _, extent)) = file_state(&ed, id) else {
        ed.message = "No file is visiting this buffer".to_owned();
        return (ed, fx);
    };
    if extent == LoadExtent::Truncated {
        let ed = ask(
            ed,
            Confirm::SaveTruncated(id),
            "File was truncated when read; save anyway? (y or n)",
        );
        (ed, fx)
    } else {
        fx.push(Effect::StatDisk { id, path });
        (ed, fx)
    }
}

/// Applies the disk state the shell found for `id`. A change since the read asks first (I-04).
pub fn disk_checked(ed: Editor, id: BufferId, state: DiskState, fx: Effects) -> (Editor, Effects) {
    let Some((_, disk, _)) = file_state(&ed, id) else {
        return (ed, fx);
    };
    if matches!(disk, DiskState::Known { .. }) && disk != state {
        let ed = ask(
            ed,
            Confirm::SaveExternal(id),
            "File changed on disk since it was read; overwrite? (y or n)",
        );
        (ed, fx)
    } else {
        let fx = write_buffer(&ed, id, fx);
        (ed, fx)
    }
}

/// Runs the action that a yes answer to `question` allows.
pub fn confirmed(mut ed: Editor, question: Confirm, fx: Effects) -> (Editor, Effects) {
    match question {
        Confirm::SaveTruncated(id) => {
            let ed = ed.with_buffer(id, |b| Buffer {
                backing: confirm_truncated(b.backing),
                ..b
            });
            save_buffer(ed, id, fx)
        }
        Confirm::SaveExternal(id) => {
            let fx = write_buffer(&ed, id, fx);
            (ed, fx)
        }
        Confirm::QuitUnsaved => {
            ed.quit = true;
            (ed, fx)
        }
        Confirm::KillModified(id) => (kill_now(ed, id), fx),
        Confirm::RevertModified(id) => revert_now(ed, id, fx),
    }
}

/// A file whose read was truncated may be saved once the user agrees (I-04).
fn confirm_truncated(backing: Backing) -> Backing {
    match backing {
        Backing::File { path, disk, .. } => Backing::File {
            path,
            disk,
            extent: LoadExtent::TruncatedConfirmed,
        },
        Backing::None => Backing::None,
    }
}

/// Asks the shell to write the buffer to its file. The result comes back as `Written` (I-13).
fn write_buffer(ed: &Editor, id: BufferId, mut fx: Effects) -> Effects {
    let buf = ed.buffer_by_id(id);
    let Backing::File { path, .. } = &buf.backing else {
        return fx;
    };
    fx.push(Effect::WriteFile {
        id,
        path: path.clone(),
        bytes: buf.text.to_bytes(),
    });
    fx
}

/// Records the new disk state of a file-backed buffer.
fn with_disk(backing: Backing, disk: DiskState) -> Backing {
    match backing {
        Backing::File { path, extent, .. } => Backing::File { path, disk, extent },
        Backing::None => Backing::None,
    }
}

/// Records a finished write (I-05). The undo file follows, encoded from the new disk state (I-16).
pub fn written(
    mut ed: Editor,
    id: BufferId,
    result: Result<DiskState, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    let Backing::File { path, .. } = &ed.buffer_by_id(id).backing else {
        return (ed, fx);
    };
    let path = path.clone();
    let message = match result {
        Ok(new_disk) => {
            ed = ed.with_buffer(id, |b| Buffer {
                backing: with_disk(b.backing, new_disk),
                modified: false,
                undo: b.undo.close(),
                ..b
            });
            match ed.buffer_by_id(id).undo.encode(new_disk) {
                Some(bytes) => {
                    fx.push(Effect::WriteUndo { id, path, bytes });
                    return (ed, fx);
                }
                None => format!("Wrote {}", path.display()),
            }
        }
        Err(e) => format!("Save failed: {e}"),
    };
    ed.message = message;
    (ed, fx)
}

/// Shows the result of the undo write that follows a successful save.
pub fn undo_written(mut ed: Editor, id: BufferId, result: Result<(), String>) -> Editor {
    let Backing::File { path, .. } = &ed.buffer_by_id(id).backing else {
        return ed;
    };
    let message = match result {
        Ok(()) => format!("Wrote {}", path.display()),
        Err(e) => format!("Wrote {}, undo not saved: {e}", path.display()),
    };
    ed.message = message;
    ed
}

fn isearch_forward(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, true, false), fx)
}

fn isearch_backward(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, false, false), fx)
}

fn isearch_forward_regexp(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, true, true), fx)
}

fn isearch_backward_regexp(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (start_isearch(ed, false, true), fx)
}

fn start_isearch(mut ed: Editor, forward: bool, regex: bool) -> Editor {
    let origin = ed.window().point;
    ed.mode = Mode::Isearch(Isearch {
        forward,
        regex,
        text: String::new(),
        origin,
        anchor: origin,
        failed: false,
    });
    ed.message = "I-search: ".to_owned();
    ed
}

/// Keys during an incremental search. `C-s` and `C-r` move to the next match
/// in each direction. Any other non-text key ends the search, then runs normally.
pub fn isearch_key(mut ed: Editor, key: Key, fx: Effects) -> (Editor, Effects) {
    let Mode::Isearch(mut s) = ed.mode.clone() else {
        return (ed, fx);
    };
    if key == Key::ctrl('g') {
        ed.mode = Mode::Normal {
            prefix: Prefix::None,
        };
        let mut ed = ed.move_point(s.origin);
        ed.message = "Quit".to_owned();
        return (ed, fx);
    }
    if key == Key::plain(Code::Enter) {
        ed.mode = Mode::Normal {
            prefix: Prefix::None,
        };
        ed.message.clear();
        return (ed, fx);
    }
    let (mut ed, s) = if key == Key::ctrl('s') || key == Key::ctrl('r') {
        s.forward = key == Key::ctrl('s');
        let from = ed.window().point;
        isearch_step(ed, s, from, true)
    } else if key == Key::plain(Code::Backspace) {
        s.text.pop();
        let origin = s.origin;
        s.anchor = origin;
        isearch_step(ed, s, origin, false)
    } else if let Some(c) = key.text() {
        s.text.push(c);
        let anchor = s.anchor;
        isearch_step(ed, s, anchor, false)
    } else {
        ed.mode = Mode::Normal {
            prefix: Prefix::None,
        };
        return ed.handle_normal_key(Prefix::None, key, fx);
    };
    ed.mode = Mode::Isearch(s);
    (ed, fx)
}

/// Searches from `from` in the direction of `s` and moves point to the match.
/// With `wrap`, a miss restarts at the other end of the buffer.
fn isearch_step(mut ed: Editor, mut s: Isearch, from: usize, wrap: bool) -> (Editor, Isearch) {
    if s.text.is_empty() {
        s.failed = false;
        ed.message = "I-search: ".to_owned();
        return (ed, s);
    }
    let pattern = if s.regex {
        Pattern::regex(&s.text)
    } else {
        Ok(Pattern::literal(&s.text))
    };
    let pattern = match pattern {
        Ok(p) => p,
        Err(e) => {
            s.failed = true;
            ed.message = format!("Invalid regexp: {e}");
            return (ed, s);
        }
    };
    let fold = ed.config.case_fold_search;
    let mut wrapped = false;
    let mut found = find_in_direction(&ed, &pattern, s.forward, from, fold);
    if found.is_none() && wrap {
        let other_end = if s.forward { 0 } else { ed.buffer().text.len() };
        found = find_in_direction(&ed, &pattern, s.forward, other_end, fold);
        wrapped = found.is_some();
    }
    s.failed = found.is_none();
    if let Some((start, end)) = found {
        s.anchor = start;
        ed = ed.move_point(if s.forward { end } else { start });
    }
    let failing = if s.failed { "Failing " } else { "" };
    let wrap_note = if wrapped { "Wrapped " } else { "" };
    ed.message = format!("{wrap_note}{failing}I-search: {}", s.text);
    (ed, s)
}

fn find_in_direction(
    ed: &Editor,
    pattern: &Pattern,
    forward: bool,
    from: usize,
    fold: bool,
) -> Option<(usize, usize)> {
    let text = &ed.buffer().text;
    if forward {
        search::find_forward(text, pattern, from, fold)
    } else {
        search::find_backward(text, pattern, from, fold)
    }
}

/// Starts a query-replace session from point. Errors on a bad regexp or an empty pattern.
pub fn start_query_replace(
    mut ed: Editor,
    pattern: &str,
    replacement: &str,
    regex: bool,
) -> (Editor, Result<(), String>) {
    if pattern.is_empty() {
        return (ed, Err("empty pattern".to_owned()));
    }
    let pattern = if regex {
        match Pattern::regex(pattern) {
            Ok(p) => p,
            Err(e) => return (ed, Err(e)),
        }
    } else {
        Pattern::literal(pattern)
    };
    let mut q = QueryReplace {
        pattern,
        replacement: replacement.to_owned(),
        regex,
        fold: ed.config.case_fold_search,
        current: None,
        count: 0,
    };
    let from = ed.window().point;
    q.current = next_match(&ed, &q, from);
    if q.current.is_none() {
        ed.message = "No match".to_owned();
        return (ed, Ok(()));
    }
    ed.mode = Mode::QueryReplace(q);
    ed.message = "Query replacing (y, n, !, q)".to_owned();
    (ed, Ok(()))
}

fn next_match(ed: &Editor, q: &QueryReplace, from: usize) -> Option<(usize, usize)> {
    if from > ed.buffer().text.len() {
        return None;
    }
    search::find_forward(&ed.buffer().text, &q.pattern, from, q.fold)
}

/// Answers for query-replace: `y`/SPC replace, `n`/DEL skip, `!` replace the rest,
/// `q`/RET/`C-g` stop (A8). The whole session is one undo group.
pub fn query_key(ed: Editor, key: Key) -> Editor {
    let Mode::QueryReplace(mut q) = ed.mode.clone() else {
        return ed;
    };
    let Some((start, end)) = q.current else {
        return finish(ed, &q);
    };
    match key.text() {
        Some('y' | ' ') => match replace_match(ed, &q, start, end) {
            (ed, Some(from)) => {
                q.count += 1;
                step(ed, q, from)
            }
            (ed, None) => finish(ed, &q),
        },
        Some('n') => {
            let from = resume(&ed, end, start == end);
            step(ed, q, from)
        }
        None if key == Key::plain(Code::Backspace) => {
            let from = resume(&ed, end, start == end);
            step(ed, q, from)
        }
        Some('!') => {
            let mut ed = ed;
            let mut current = Some((start, end));
            while let Some((s, e)) = current {
                let (next, from) = replace_match(ed, &q, s, e);
                ed = next;
                let Some(from) = from else {
                    break;
                };
                q.count += 1;
                current = next_match(&ed, &q, from);
            }
            q.current = None;
            finish(ed, &q)
        }
        Some('q' | '\n') => finish(ed, &q),
        None if key == Key::ctrl('g') => finish(ed, &q),
        _ => ed,
    }
}

/// Replaces `start..end` and returns the editor and where the next search starts.
/// `None` when the buffer refuses the edit.
fn replace_match(
    ed: Editor,
    q: &QueryReplace,
    start: usize,
    end: usize,
) -> (Editor, Option<usize>) {
    let matched = ed.buffer().text.bytes(start..end);
    let replacement = if q.regex {
        search::expand_replacement(&q.replacement, &matched)
    } else {
        q.replacement.as_bytes().to_vec()
    };
    let after = start + replacement.len();
    let (ed, applied) = ed.apply(Edit::Delete {
        pos: start,
        bytes: matched,
    });
    if applied.is_none() {
        return (ed, None);
    }
    let (ed, applied) = ed.apply(Edit::Insert {
        pos: start,
        bytes: replacement,
    });
    if applied.is_none() {
        return (ed, None);
    }
    let ed = ed.move_point(after);
    let from = resume(&ed, after, start == end);
    (ed, Some(from))
}

/// Next search start after a match ending at `after`. An empty match must move
/// past one unit, or the search would find it again forever.
fn resume(ed: &Editor, after: usize, matched_empty: bool) -> usize {
    let text = &ed.buffer().text;
    if !matched_empty {
        after
    } else if after >= text.len() {
        // Past the end: no later match can start, so the search finishes.
        text.len() + 1
    } else {
        text.next_boundary(after)
    }
}

fn step(mut ed: Editor, mut q: QueryReplace, from: usize) -> Editor {
    q.current = next_match(&ed, &q, from);
    if q.current.is_none() {
        finish(ed, &q)
    } else {
        ed.mode = Mode::QueryReplace(q);
        ed
    }
}

fn finish(mut ed: Editor, q: &QueryReplace) -> Editor {
    ed.mode = Mode::Normal {
        prefix: Prefix::None,
    };
    let mut ed = ed.boundary();
    ed.message = format!("Replaced {} occurrence(s)", q.count);
    ed
}

fn split_window_below(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (split(ed, Dir::Below), fx)
}

fn split_window_right(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (split(ed, Dir::Right), fx)
}

/// Replaces the selected leaf with a split of two windows that show the same buffer.
fn split(mut ed: Editor, dir: Dir) -> Editor {
    let i = ed.selected;
    ed.layout = ed.layout.replace_leaf(i, |w| Layout::Split {
        dir,
        first: Box::new(Layout::Leaf(w.clone())),
        second: Box::new(Layout::Leaf(w)),
    });
    ed
}

fn other_window(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    ed.selected = (ed.selected + 1) % ed.leaf_count();
    (ed, fx)
}

fn delete_other_windows(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let w = ed.window().clone();
    ed.layout = Layout::Leaf(w);
    ed.selected = 0;
    (ed, fx)
}

/// Shows buffer `id` in the selected window. The old buffer keeps its point.
pub fn switch_to(ed: Editor, id: BufferId) -> Editor {
    let old = ed.window().buffer;
    if old == id {
        return ed;
    }
    let point = ed.window().point;
    let ed = ed.with_buffer(old, |b| Buffer {
        saved_point: point,
        ..b
    });
    let target = ed.buffer_by_id(id).saved_point;
    ed.with_window(|w| Window {
        buffer: id,
        point: target,
        top: 0,
        ..w
    })
}

/// Kills buffer `id`. A modified buffer asks first.
pub fn kill_buffer(ed: Editor, id: BufferId) -> Editor {
    if ed.buffer_by_id(id).modified {
        ask(
            ed,
            Confirm::KillModified(id),
            "Buffer is modified; kill anyway? (y or n)",
        )
    } else {
        kill_now(ed, id)
    }
}

/// Removes buffer `id`. Windows that showed it move to another buffer (I-08).
/// Killing the last buffer leaves an empty `*scratch*` (A4).
fn kill_now(mut ed: Editor, id: BufferId) -> Editor {
    if ed.buffers.len() == 1 {
        ed = ed.add_buffer("*scratch*", Text::default(), Backing::None).0;
    }
    let replacement = ed
        .buffers
        .iter()
        .map(|b| b.id)
        .find(|&b| b != id)
        .expect("another buffer exists");
    let saved = ed.buffer_by_id(replacement).saved_point;
    ed.buffers.retain(|b| b.id != id);
    ed.layout = ed.layout.map_windows(&|w| {
        if w.buffer == id {
            Window {
                buffer: replacement,
                point: saved,
                top: 0,
                ..w
            }
        } else {
            w
        }
    });
    ed
}

fn execute_extended_command(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "M-x ", Purpose::ExecuteCommand), fx)
}

fn find_file_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Find file: ", Purpose::FindFile), fx)
}

fn write_file_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Write file: ", Purpose::WriteFile), fx)
}

fn switch_to_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Switch to buffer: ", Purpose::SwitchBuffer), fx)
}

fn kill_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Kill buffer: ", Purpose::KillBuffer), fx)
}

fn goto_line_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Goto line: ", Purpose::GotoLine), fx)
}

fn query_replace_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        prompt(ed, "Query replace: ", Purpose::QueryFrom { regex: false }),
        fx,
    )
}

fn query_replace_regexp_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (
        prompt(
            ed,
            "Query replace regexp: ",
            Purpose::QueryFrom { regex: true },
        ),
        fx,
    )
}

fn prompt(mut ed: Editor, label: &str, purpose: Purpose) -> Editor {
    ed.message = label.to_owned();
    ed.mode = Mode::Prompt(Prompt {
        label: label.to_owned(),
        input: String::new(),
        purpose,
    });
    ed
}

/// Every built-in command name with the keys bound to it, for `M-x` completion (§4).
pub fn completions(ed: &Editor, prefix: &str) -> Vec<(String, String)> {
    BUILTINS
        .iter()
        .filter(|(n, _)| n.starts_with(prefix))
        .map(|(n, _)| (n.to_string(), ed.keymap.keys_for(n).join(", ")))
        .collect()
}

fn candidates(ed: &Editor, purpose: &Purpose, input: &str) -> Vec<(String, String)> {
    match purpose {
        Purpose::ExecuteCommand => completions(ed, input),
        Purpose::SwitchBuffer | Purpose::KillBuffer => ed
            .buffers
            .iter()
            .filter(|b| b.name.starts_with(input))
            .map(|b| (b.name.clone(), String::new()))
            .collect(),
        _ => Vec::new(),
    }
}

/// TAB: extends the input to the common prefix, or lists the candidates.
/// Returns the prompt and the message to show, if any.
fn complete(ed: &Editor, mut p: Prompt) -> (Prompt, Option<String>) {
    let cands = candidates(ed, &p.purpose, &p.input);
    match cands.as_slice() {
        [] => (p, Some("No match".to_owned())),
        [(name, _)] => {
            p.input = name.clone();
            (p, None)
        }
        _ => {
            let names: Vec<&str> = cands.iter().map(|(n, _)| n.as_str()).collect();
            let common = common_prefix(&names);
            if common.len() > p.input.len() {
                p.input = common;
            }
            let list = cands
                .iter()
                .map(|(n, k)| {
                    if k.is_empty() {
                        n.clone()
                    } else {
                        format!("{n} ({k})")
                    }
                })
                .collect::<Vec<_>>()
                .join("  ");
            (p, Some(list))
        }
    }
}

fn common_prefix(names: &[&str]) -> String {
    let Some((first, rest)) = names.split_first() else {
        return String::new();
    };
    let mut end = first.len();
    for n in rest {
        let shared: usize = first
            .chars()
            .zip(n.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        end = end.min(shared);
    }
    first[..end].to_owned()
}

/// Keys inside a prompt: text edits the input, RET submits, C-g cancels, TAB completes.
pub fn prompt_key(mut ed: Editor, key: Key, fx: Effects) -> (Editor, Effects) {
    let Mode::Prompt(mut p) = ed.mode.clone() else {
        return (ed, fx);
    };
    if key == Key::ctrl('g') {
        ed.mode = Mode::Normal {
            prefix: Prefix::None,
        };
        ed.message = "Quit".to_owned();
        return (ed, fx);
    }
    if key == Key::plain(Code::Enter) {
        ed.mode = Mode::Normal {
            prefix: Prefix::None,
        };
        ed.message.clear();
        return submit(ed, p, fx);
    }
    let mut note = None;
    if key == Key::plain(Code::Backspace) {
        p.input.pop();
    } else if key == Key::plain(Code::Tab) {
        (p, note) = complete(&ed, p);
    } else if let Some(c) = key.text() {
        p.input.push(c);
    }
    ed.message = note.unwrap_or_else(|| format!("{}{}", p.label, p.input));
    ed.mode = Mode::Prompt(p);
    (ed, fx)
}

fn submit(ed: Editor, p: Prompt, fx: Effects) -> (Editor, Effects) {
    let input = p.input;
    match p.purpose {
        Purpose::ExecuteCommand => ed.run_named(&input, fx),
        Purpose::FindFile => find_file(ed, &input, fx),
        Purpose::WriteFile => write_file(ed, &input, fx),
        Purpose::SwitchBuffer => (switch_to_named(ed, &input), fx),
        Purpose::KillBuffer => (kill_named(ed, &input), fx),
        Purpose::GotoLine => (goto_line(ed, &input), fx),
        Purpose::ShellCommand => {
            let fx = run_shell(&input, fx);
            (ed, fx)
        }
        Purpose::QueryFrom { regex } => {
            if input.is_empty() {
                let mut ed = ed;
                ed.message = "Query replace needs a pattern".to_owned();
                (ed, fx)
            } else {
                let label = format!("Query replace {input} with: ");
                (
                    prompt(ed, &label, Purpose::QueryTo { regex, from: input }),
                    fx,
                )
            }
        }
        Purpose::QueryTo { regex, from } => ed.command(fx, |e, fx| {
            let (mut e, result) = start_query_replace(e, &from, &input, regex);
            if let Err(msg) = result {
                e.message = msg;
            }
            (e, fx)
        }),
    }
}

/// Opens the file `input` in a new buffer, or shows it if it is already open.
fn find_file(ed: Editor, input: &str, fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let path = PathBuf::from(input);
    let existing = ed
        .buffers
        .iter()
        .find(|b| matches!(&b.backing, Backing::File { path: p, .. } if *p == path))
        .map(|b| b.id);
    match existing {
        Some(id) => (switch_to(ed, id), fx),
        None => {
            let fx = request_open(&ed, path, None, true, fx);
            (ed, fx)
        }
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

/// Writes the current buffer to a new file name and makes that its file (§8).
fn write_file(mut ed: Editor, input: &str, fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let path = PathBuf::from(input);
    let id = ed.window().buffer;
    let name = path
        .file_name()
        .map_or_else(|| input.to_owned(), |n| n.to_string_lossy().into_owned());
    ed = ed.with_buffer(id, |b| Buffer {
        name,
        backing: Backing::File {
            path,
            disk: DiskState::Unknown,
            extent: LoadExtent::Whole,
        },
        ..b
    });
    let fx = write_buffer(&ed, id, fx);
    (ed, fx)
}

fn switch_to_named(ed: Editor, name: &str) -> Editor {
    if name.is_empty() {
        return ed;
    }
    let found = ed.buffers.iter().find(|b| b.name == name).map(|b| b.id);
    match found {
        Some(id) => switch_to(ed, id),
        None => {
            let (ed, id) = ed.add_buffer(name, Text::default(), Backing::None);
            switch_to(ed, id)
        }
    }
}

fn kill_named(mut ed: Editor, name: &str) -> Editor {
    let id = if name.is_empty() {
        Some(ed.window().buffer)
    } else {
        ed.buffers.iter().find(|b| b.name == name).map(|b| b.id)
    };
    match id {
        Some(id) => kill_buffer(ed, id),
        None => {
            ed.message = format!("No buffer named {name}");
            ed
        }
    }
}

fn goto_line(mut ed: Editor, input: &str) -> Editor {
    let Ok(n) = input.trim().parse::<usize>() else {
        ed.message = format!("Not a line number: {input}");
        return ed;
    };
    let pos = ed.buffer().text.nth_line_start(n);
    ed.move_point(pos)
}

fn revert_buffer(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    if ed.buffer().modified {
        let ed = ask(
            ed,
            Confirm::RevertModified(id),
            "Discard changes and revert? (y or n)",
        );
        (ed, fx)
    } else {
        revert_now(ed, id, fx)
    }
}

/// Asks the shell to read the file again. The result comes back as `FileRead` (C-12).
fn revert_now(mut ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    let Some((path, _, _)) = file_state(&ed, id) else {
        ed.message = "Buffer has no file to revert to".to_owned();
        return (ed, fx);
    };
    fx.push(Effect::ReadFile {
        target: ReadTarget::Revert(id),
        path,
        max_size: ed.config.max_file_size,
    });
    (ed, fx)
}

fn view_mode(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let on = !ed.buffer().view_only;
    let mut ed = ed.with_current_buffer(|b| Buffer { view_only: on, ..b });
    ed.message = if on { "View mode on" } else { "View mode off" }.to_owned();
    (ed, fx)
}

fn fill_paragraph(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let p = ed.window().point;
    let fill = usize::from(ed.config.fill_column);
    let tab = usize::from(ed.config.tab_width);
    let text = &ed.buffer().text;
    let Some((start, end)) = paragraph_at(text, p) else {
        ed.message = "No paragraph at point".to_owned();
        return (ed, fx);
    };
    // The first line's indent is repeated on every line (A10).
    let mut indent_end = start;
    while indent_end < text.line_end(start) && matches!(text.byte_at(indent_end), b' ' | b'\t') {
        indent_end += 1;
    }
    let indent = text.bytes(start..indent_end);
    let indent_width = span_width(text, start, indent_end, tab);

    let mut words = Vec::new();
    let mut i = start;
    while i < end {
        if is_space(text.byte_at(i)) {
            i += 1;
            continue;
        }
        let word_start = i;
        while i < end && !is_space(text.byte_at(i)) {
            i += 1;
        }
        words.push((word_start, i));
    }

    let mut out = indent.clone();
    let mut line_width = indent_width;
    let mut line_has_word = false;
    for (s, e) in words {
        let w = span_width(text, s, e, tab);
        if line_has_word && line_width + 1 + w > fill {
            out.push(b'\n');
            out.extend_from_slice(&indent);
            line_width = indent_width;
            line_has_word = false;
        }
        if line_has_word {
            out.push(b' ');
            line_width += 1;
        }
        out.extend(text.bytes(s..e));
        line_width += w;
        line_has_word = true;
    }

    let old = text.bytes(start..end);
    let (ed, _) = ed.apply(Edit::Delete {
        pos: start,
        bytes: old,
    });
    let (ed, _) = ed.apply(Edit::Insert {
        pos: start,
        bytes: out,
    });
    (ed.move_point(start), fx)
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n')
}

/// Display width of `start..end`, with tabs measured from column 0.
fn span_width(text: &Text, start: usize, end: usize, tab: usize) -> usize {
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
fn paragraph_at(text: &Text, p: usize) -> Option<(usize, usize)> {
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

fn shell_command_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    (prompt(ed, "Shell command: ", Purpose::ShellCommand), fx)
}

/// Asks the shell to run `command`. The output comes back as `ShellDone`.
fn run_shell(command: &str, mut fx: Effects) -> Effects {
    if command.is_empty() {
        return fx;
    }
    fx.push(Effect::RunShell {
        command: command.to_owned(),
    });
    fx
}

/// Shows the output of a shell command. One line goes to the echo area; more goes to `*Shell Command Output*` (A9).
pub fn shell_done(mut ed: Editor, result: Result<String, String>) -> Editor {
    let output = match result {
        Ok(output) => output,
        Err(e) => {
            ed.message = e;
            return ed;
        }
    };
    let text = output.replace("\r\n", "\n");
    let trimmed = text.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.lines().collect();
    match lines.as_slice() {
        [] => {
            ed.message = "(no output)".to_owned();
            ed
        }
        [only] => {
            ed.message = (*only).to_owned();
            ed
        }
        _ => show_output(ed, trimmed),
    }
}

/// Puts `text` in the output buffer and shows it in the other window.
fn show_output(ed: Editor, text: &str) -> Editor {
    const NAME: &str = "*Shell Command Output*";
    let found = ed.buffers.iter().find(|b| b.name == NAME).map(|b| b.id);
    let (ed, id) = match found {
        Some(id) => (ed, id),
        None => ed.add_buffer(NAME, Text::default(), Backing::None),
    };
    let mut ed = ed.with_buffer(id, |b| Buffer {
        text: Text::from_bytes(text.as_bytes()),
        modified: false,
        ..b
    });
    if ed.leaf_count() == 1 {
        ed = split(ed, Dir::Below);
    }
    let orig = ed.selected;
    let other = if orig + 1 < ed.leaf_count() {
        orig + 1
    } else {
        orig - 1
    };
    ed.selected = other;
    let mut ed = switch_to(ed, id);
    ed.selected = orig;
    ed
}

/// Applies the result of a file read: a new buffer for `Open`, a reload for `Revert` (C-11, C-12).
pub fn file_read(
    mut ed: Editor,
    target: ReadTarget,
    path: PathBuf,
    result: Result<effect::Read, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    match (target, result) {
        (ReadTarget::Open { line, show }, Ok(read)) => {
            let loaded = loaded_from(&path, read, ed.config.max_file_size);
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let (mut ed, id) = ed.add_buffer(&name, loaded.text, loaded.backing);
            if let Some(n) = line {
                let pos = ed.buffer_by_id(id).text.nth_line_start(n);
                ed = ed.with_buffer(id, |b| Buffer {
                    saved_point: pos,
                    ..b
                });
            }
            fx.push(Effect::ReadUndo { id, path });
            if show {
                ed = switch_to(ed, id);
            }
            (ed, fx)
        }
        (ReadTarget::Open { .. }, Err(e)) => {
            ed.message = format!("Cannot open {}: {e}", path.display());
            (ed, fx)
        }
        (ReadTarget::Revert(id), Ok(read)) => {
            let loaded = loaded_from(&path, read, ed.config.max_file_size);
            let limit = ed.config.undo_limit;
            let mut ed = ed.with_buffer(id, |b| Buffer {
                text: loaded.text,
                backing: loaded.backing,
                modified: false,
                undo: UndoLog::new(limit),
                ..b
            });
            let text = ed.buffer_by_id(id).text.clone();
            ed.layout = ed.layout.map_windows(&|w| {
                if w.buffer == id {
                    Window {
                        point: snap(&text, w.point.min(text.len())),
                        top: 0,
                        ..w
                    }
                } else {
                    w
                }
            });
            ed.message = "Reverted".to_owned();
            (ed, fx)
        }
        (ReadTarget::Revert(_), Err(e)) => {
            ed.message = format!("Revert failed: {e}");
            (ed, fx)
        }
    }
}

/// Applies the undo file read for `id`. The core keeps it only if it matches the disk state (I-10).
pub fn undo_read(ed: Editor, id: BufferId, bytes: Option<Vec<u8>>) -> Editor {
    let Some(bytes) = bytes else {
        return ed;
    };
    let disk = match &ed.buffer_by_id(id).backing {
        Backing::File { disk, .. } => *disk,
        Backing::None => DiskState::Unknown,
    };
    match UndoLog::decode(&bytes, disk, ed.config.undo_limit) {
        Some(log) => ed.with_buffer(id, |b| Buffer { undo: log, ..b }),
        None => ed,
    }
}

/// Ends a Lua command: shows the first line of its error, then closes its undo group like any command.
pub fn lua_done(mut ed: Editor, result: Result<(), String>) -> Editor {
    if let Err(e) = result {
        ed.message = e.lines().next().unwrap_or_default().to_owned();
    }
    ed.end_command()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::editor::Prefix;
    use crate::test_support::{editor, keys, point, run_command, text, typ};

    /// Starts a query-replace session and expects it to start.
    fn start_query(ed: Editor, pattern: &str, replacement: &str, regex: bool) -> Editor {
        let (ed, result) = start_query_replace(ed, pattern, replacement, regex);
        result.unwrap();
        ed
    }

    #[test]
    fn insert_hangul_saves_utf8() {
        let ed = typ(editor(), "한");
        assert_eq!(ed.buffer().text.to_bytes(), "한".as_bytes());
    }

    #[test]
    fn motion_hangul_moves_by_character() {
        let ed = typ(editor(), "가나다");
        assert_eq!(point(&ed), 9);
        let ed = keys(ed, "C-b");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "C-b C-b");
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-f");
        assert_eq!(point(&ed), 3);
        let ed = keys(ed, "C-e");
        assert_eq!(point(&ed), 9);
        let ed = keys(ed, "C-a");
        assert_eq!(point(&ed), 0);
    }

    #[test]
    fn motion_delete_hangul() {
        let ed = typ(editor(), "가나다");
        let ed = keys(ed, "DEL");
        assert_eq!(text(&ed), "가나");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "C-a C-d");
        assert_eq!(text(&ed), "나");
    }

    #[test]
    fn motion_vertical_keeps_goal_column() {
        // Lines: "ab가" (bytes 0..5, columns 0..4), "cd" (bytes 6..8), "" (byte 9).
        let ed = typ(editor(), "ab가\ncd\n");
        assert_eq!(point(&ed), 9);
        let ed = keys(ed, "C-p");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "C-e C-p");
        // Goal column 2 is the start of "가", which is two columns wide.
        assert_eq!(point(&ed), 2);
        let ed = keys(ed, "C-n");
        // The goal survives the vertical move; "cd" is only two columns wide, so it clamps.
        assert_eq!(point(&ed), 8);
    }

    #[test]
    fn kill_ring_region_and_yank() {
        let ed = typ(editor(), "abc");
        let ed = keys(ed, "C-a C-SPC C-f C-f");
        let ed = keys(ed, "C-w");
        assert_eq!(text(&ed), "c");
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-y");
        assert_eq!(text(&ed), "abc");
        assert_eq!(point(&ed), 2);
    }

    #[test]
    fn kill_ring_consecutive_kills_append() {
        let ed = typ(editor(), "one\ntwo");
        let ed = keys(ed, "C-p C-a");
        assert_eq!(point(&ed), 0);
        let ed = run_command(ed, kill_line);
        let ed = run_command(ed, kill_line);
        assert_eq!(text(&ed), "two");
        assert_eq!(ed.kill_ring, vec![b"one\n".to_vec()]);
    }

    #[test]
    fn kill_ring_yank_pop_cycles() {
        let mut ed = editor();
        ed.kill_ring = vec![b"A".to_vec(), b"B".to_vec()];
        let ed = keys(ed, "C-y");
        assert_eq!(text(&ed), "B");
        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "A");
        let ed = run_command(ed, yank_pop);
        assert_eq!(text(&ed), "B");
    }

    #[test]
    fn exchange_point_and_mark_swaps() {
        let ed = typ(editor(), "abc");
        let ed = keys(ed, "C-a C-SPC C-e");
        let ed = run_command(ed, exchange_point_and_mark);
        assert_eq!(point(&ed), 0);
        assert_eq!(ed.buffer().mark, Some(3));
    }

    #[test]
    fn isearch_moves_and_cancels() {
        let ed = typ(editor(), "abc abd");
        let ed = keys(ed, "C-a C-s");
        let ed = typ(ed, "a");
        assert_eq!(point(&ed), 1);
        let ed = typ(ed, "b");
        assert_eq!(point(&ed), 2);
        let ed = keys(ed, "C-s");
        assert_eq!(point(&ed), 6);
        let ed = keys(ed, "DEL");
        assert_eq!(point(&ed), 1);
        let ed = keys(ed, "C-g");
        assert_eq!(point(&ed), 0);
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::None
            }
        );
    }

    #[test]
    fn isearch_wraps_after_last_match() {
        let ed = typ(editor(), "abc abd");
        let ed = keys(ed, "C-a C-s");
        let ed = typ(ed, "c");
        assert_eq!(point(&ed), 3);
        let ed = keys(ed, "C-s");
        assert_eq!(point(&ed), 3);
        assert!(ed.message.contains("Wrapped"));
    }

    #[test]
    fn isearch_regexp_extends_pattern() {
        let ed = typ(editor(), "x1 x22");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, isearch_forward_regexp);
        let ed = typ(ed, "x[0-9]*");
        assert_eq!(point(&ed), 2);
    }

    #[test]
    fn query_replace_answers_each_match() {
        let ed = typ(editor(), "foo bar foo baz foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "foo", "qux", false), fx));
        let ed = keys(ed, "y");
        assert_eq!(text(&ed), "qux bar foo baz foo");
        let ed = keys(ed, "n");
        let ed = keys(ed, "y");
        assert_eq!(text(&ed), "qux bar foo baz qux");
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::None
            }
        );
        // The whole session is one undo group.
        let ed = keys(ed, "C-/");
        assert_eq!(text(&ed), "foo bar foo baz foo");
    }

    #[test]
    fn query_replace_quits_early() {
        let ed = typ(editor(), "foo bar foo");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "foo", "qux", false), fx));
        let ed = keys(ed, "y q");
        assert_eq!(text(&ed), "qux bar foo");
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::None
            }
        );
    }

    #[test]
    fn query_replace_empty_matches_terminate() {
        let ed = typ(editor(), "ab");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "x*", "-", true), fx));
        let ed = keys(ed, "!");
        assert_eq!(text(&ed), "-a-b-");
    }

    #[test]
    fn query_replace_regexp_bang_and_ampersand() {
        let ed = typ(editor(), "a1 a2");
        let ed = keys(ed, "C-a");
        let ed = run_command(ed, |e, fx| (start_query(e, "a[0-9]", "<&>", true), fx));
        let ed = keys(ed, "!");
        assert_eq!(text(&ed), "<a1> <a2>");

        let lit = typ(editor(), "a1 a2");
        let lit = keys(lit, "C-a");
        let lit = run_command(lit, |e, fx| (start_query(e, "a1", "<&>", false), fx));
        let lit = keys(lit, "!");
        assert_eq!(text(&lit), "<&> a2");

        let esc = typ(editor(), "a1 a2");
        let esc = keys(esc, "C-a");
        let esc = run_command(esc, |e, fx| (start_query(e, "a[0-9]", "\\&", true), fx));
        let esc = keys(esc, "!");
        assert_eq!(text(&esc), "& &");
    }

    #[test]
    fn windows_keep_independent_points() {
        let ed = typ(editor(), "abcd");
        let ed = run_command(ed, split_window_below);
        assert_eq!(ed.leaf_count(), 2);
        let ed = ed.with_window(|w| Window { point: 3, ..w });
        let ed = run_command(ed, other_window);
        assert_eq!(point(&ed), 4);
        let ed = run_command(ed, delete_other_windows);
        assert_eq!(ed.leaf_count(), 1);
        assert_eq!(point(&ed), 4);
    }

    #[test]
    fn buffers_remember_point_and_kill_falls_back() {
        let (ed, other) =
            typ(editor(), "hi").add_buffer("b2", Text::from_bytes(b"xyz"), Backing::None);
        let ed = switch_to(ed, other);
        assert_eq!(point(&ed), 0);
        let ed = keys(ed, "C-e");
        assert_eq!(point(&ed), 3);
        let ed = switch_to(ed, BufferId(0));
        assert_eq!(point(&ed), 2);
        let ed = switch_to(ed, other);
        assert_eq!(point(&ed), 3);

        let ed = kill_buffer(ed, other);
        assert_eq!(ed.buffers.len(), 1);
        assert_eq!(ed.buffer().name, "*scratch*");
    }

    #[test]
    fn killing_last_buffer_makes_scratch() {
        let ed = editor();
        let only = ed.window().buffer;
        let ed = kill_buffer(ed, only);
        assert_eq!(ed.buffers.len(), 1);
        assert_eq!(ed.buffer().name, "*scratch*");
        assert_ne!(ed.buffer().id, only);
    }

    #[test]
    fn kill_modified_buffer_asks() {
        let (ed, other) = editor().add_buffer("b2", Text::default(), Backing::None);
        let ed = switch_to(ed, other);
        let ed = typ(ed, "x");
        let ed = switch_to(ed, BufferId(0));

        let ed = kill_buffer(ed, other);
        assert!(matches!(ed.mode, Mode::Confirm(Confirm::KillModified(_))));
        let ed = keys(ed, "n");
        assert_eq!(ed.buffers.len(), 2);
        let ed = kill_buffer(ed, other);
        let ed = keys(ed, "y");
        assert_eq!(ed.buffers.len(), 1);
    }

    #[test]
    fn mx_completes_and_cancels() {
        let ed = keys(editor(), "M-x");
        assert!(matches!(ed.mode, Mode::Prompt(_)));
        let ed = typ(ed, "for");
        let ed = keys(ed, "TAB");
        assert!(matches!(&ed.mode, Mode::Prompt(p) if p.input == "forward-char"));
        let ed = keys(ed, "C-g");
        assert_eq!(
            ed.mode,
            Mode::Normal {
                prefix: Prefix::None
            }
        );
    }

    #[test]
    fn mx_lists_candidates_with_their_keys() {
        let ed = editor();
        assert!(completions(&ed, "und").contains(&("undo".to_owned(), "C-_".to_owned())));
        assert!(completions(&ed, "zzz").is_empty());
    }

    #[test]
    fn fill_paragraph_breaks_at_spaces_by_width() {
        let mut ed = editor();
        ed.config.fill_column = 20;
        let ed = typ(ed, "가나다 라마바 사아자 차카타 파하");
        let ed = run_command(ed, fill_paragraph);
        assert_eq!(text(&ed), "가나다 라마바 사아자\n차카타 파하");
    }

    #[test]
    fn fill_paragraph_keeps_indent_and_stops_at_blank_lines() {
        let mut ed = editor();
        ed.config.fill_column = 10;
        let ed = typ(ed, "  hello world foo");
        let ed = run_command(ed, fill_paragraph);
        assert_eq!(text(&ed), "  hello\n  world\n  foo");

        let mut ed = editor();
        ed.config.fill_column = 8;
        let ed = typ(ed, "aa bb cc dd ee ff\n\ndd ee");
        let ed = ed.with_window(|w| Window { point: 0, ..w });
        let ed = run_command(ed, fill_paragraph);
        assert_eq!(text(&ed), "aa bb cc\ndd ee ff\n\ndd ee");
    }

    #[test]
    fn shell_command_one_line_echoes_many_lines_open_buffer() {
        let ed = editor();
        let ed = run_command(ed, |e, fx| (e, run_shell("echo hi", fx)));
        assert_eq!(ed.message, "hi");

        let ed = run_command(ed, |e, fx| (e, run_shell("echo a&& echo b", fx)));
        let out = ed
            .buffers
            .iter()
            .find(|b| b.name == "*Shell Command Output*")
            .expect("output buffer");
        assert_eq!(out.text.to_bytes(), b"a\nb");
        assert_eq!(ed.leaf_count(), 2);
    }
}

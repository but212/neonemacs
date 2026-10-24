//! Text modification, kill ring, and formatting commands (§6).

use crate::core::commands::motion::region;
use crate::core::commands::util::{
    delete_range, insert_at, is_space, paragraph_at, safe_point, sanitize_point, span_width,
};
use crate::core::editor::{Buffer, Editor, LastCommand};
use crate::core::effect::Effects;
use crate::core::undo::Edit;

pub(crate) fn delete_backward_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let ed = sanitize_point(ed);
    let p = safe_point(&ed);
    if p == 0 {
        return (ed, fx);
    }
    let start = ed.buffer().text.prev_boundary(p);
    (delete_range(ed, start, p).0, fx)
}

pub(crate) fn delete_char(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let ed = sanitize_point(ed);
    let p = safe_point(&ed);
    if p >= ed.buffer().text.len() {
        return (ed, fx);
    }
    let end = ed.buffer().text.next_boundary(p);
    (delete_range(ed, p, end).0, fx)
}

pub(crate) fn kill_region(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
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

pub(crate) fn kill_ring_save(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
    let Some((start, end)) = region(&ed) else {
        ed.message = "The mark is not set now".to_owned();
        return (ed, fx);
    };
    let bytes = ed.buffer().text.bytes(start..end);
    ed.kill_ring.push(bytes);
    (ed, fx)
}

/// Kills to the end of the line, or the newline when already at the end.
pub(crate) fn kill_line(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let ed = sanitize_point(ed);
    let p = safe_point(&ed);
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

pub(crate) fn yank(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
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
pub(crate) fn yank_pop(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
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

pub(crate) fn fill_paragraph(mut ed: Editor, fx: Effects) -> (Editor, Effects) {
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

pub(crate) fn view_mode(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let on = !ed.buffer().view_only;
    let mut ed = ed.with_current_buffer(|b| Buffer { view_only: on, ..b });
    ed.message = if on { "View mode on" } else { "View mode off" }.to_owned();
    (ed, fx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Window;
    use crate::test_support::{editor, keys, point, run_command, text, typ};

    #[test]
    fn insert_hangul_saves_utf8() {
        let ed = typ(editor(), "한");
        assert_eq!(ed.buffer().text.to_bytes(), "한".as_bytes());
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
}

//! Adversarial stress and invariant tests for core commands (§3, §4, §6, §8, §9).
//! Tests edge cases, hostile key sequences, mode transitions, and boundary conditions.

use neonemacs::core::commands::find;
use neonemacs::core::commands::table::BUILTINS;
use neonemacs::core::config::Config;
use neonemacs::core::editor::{Editor, Mode, Prefix, Window};
use neonemacs::core::effect::Event;
use neonemacs::core::keymap::{Code, Key, parse_keys};
use neonemacs::shell::handle;

fn editor() -> Editor {
    Editor::new(Config::default())
}

fn keys(mut ed: Editor, s: &str) -> Editor {
    for k in parse_keys(s).unwrap() {
        ed = handle(ed, None, Event::Key(k));
    }
    ed
}

fn typ(mut ed: Editor, s: &str) -> Editor {
    for c in s.chars() {
        let code = if c == '\n' {
            Code::Enter
        } else {
            Code::Char(c)
        };
        ed = handle(ed, None, Event::Key(Key::plain(code)));
    }
    ed
}

fn text(ed: &Editor) -> String {
    String::from_utf8_lossy(&ed.buffer().text.to_bytes()).into_owned()
}

fn point(ed: &Editor) -> usize {
    ed.window().point
}

// =========================================================================
// Category 1: Extreme Text & Cursor Boundary Tests
// =========================================================================

#[test]
fn empty_buffer_all_builtins_do_not_panic() {
    // Every single built-in command must be safe against a completely empty buffer.
    for &(name, builtin) in BUILTINS {
        let ed = editor();
        let fx = Vec::new();
        let (result_ed, _) = builtin(ed, fx);
        // Editor must still have a valid buffer and valid point
        assert_eq!(
            result_ed.window().point,
            0,
            "Builtin '{}' moved point unexpectedly on empty buffer",
            name
        );
    }
}

#[test]
fn hostile_out_of_bounds_and_multibyte_point_recovery() {
    let forward_char = find("forward-char").unwrap();
    let delete_char = find("delete-char").unwrap();
    let delete_backward_char = find("delete-backward-char").unwrap();
    let kill_line = find("kill-line").unwrap();

    // UTF-8 Korean: each character is 3 bytes: 가(0..3), 나(3..6), 다(6..9)
    let ed = typ(editor(), "가나다");

    // Force point into the middle of a 3-byte UTF-8 character (point = 1 or 2)
    let bad_ed = ed.with_window(|w| Window { point: 1, ..w });
    let (after_fwd, _) = forward_char(bad_ed, Vec::new());
    // next_boundary from 1 should safely snap or move to next character boundary (3)
    assert_eq!(point(&after_fwd), 3);

    let bad_ed2 = typ(editor(), "가나다").with_window(|w| Window { point: 1, ..w });
    let (after_del, _) = delete_char(bad_ed2, Vec::new());
    // Deleting from middle of unit should delete to boundary without panic
    assert!(point(&after_del) <= after_del.buffer().text.len());

    // Force point way past end of buffer (e.g. 9999)
    let oob_ed = typ(editor(), "가나다").with_window(|w| Window { point: 9999, ..w });
    let (after_bkwd, _) = delete_backward_char(oob_ed, Vec::new());
    assert!(point(&after_bkwd) <= after_bkwd.buffer().text.len());

    let oob_ed2 = typ(editor(), "가나다").with_window(|w| Window { point: 9999, ..w });
    let (after_kill, _) = kill_line(oob_ed2, Vec::new());
    assert!(point(&after_kill) <= after_kill.buffer().text.len());
}

#[test]
fn extreme_whitespace_and_empty_paragraphs() {
    let fill_paragraph = find("fill-paragraph").unwrap();

    // Only newlines and spaces
    let ed = typ(editor(), "   \n\n\t\t\n   \n\n");
    let (ed, _) = fill_paragraph(ed, Vec::new());
    assert_eq!(ed.message, "No paragraph at point");

    // Single character with huge fill column
    let mut ed2 = editor();
    ed2.config.fill_column = 1000;
    ed2 = typ(ed2, "x");
    let (ed2, _) = fill_paragraph(ed2, Vec::new());
    assert_eq!(text(&ed2), "x");

    // Paragraph with fill_column smaller than word length
    let mut ed3 = editor();
    ed3.config.fill_column = 2;
    ed3 = typ(ed3, "supercalifragilistic");
    let (ed3, _) = fill_paragraph(ed3, Vec::new());
    assert_eq!(text(&ed3), "supercalifragilistic");
}

// =========================================================================
// Category 2: Adversarial Mode Transitions & Hostile Key Sequences
// =========================================================================

#[test]
fn prompt_hostile_backspaces_and_cancellation() {
    let mut ed = editor();
    // Enter M-x prompt
    ed = keys(ed, "M-x");
    assert!(matches!(ed.mode, Mode::Prompt(_)));

    // Spam backspaces on empty prompt - must not underflow or panic
    for _ in 0..10 {
        ed = keys(ed, "DEL");
    }
    if let Mode::Prompt(p) = &ed.mode {
        assert_eq!(p.input, "");
    } else {
        panic!("Prompt mode lost after DEL");
    }

    // Type partial, then cancel with C-g
    ed = typ(ed, "nonexistent-command-12345");
    ed = keys(ed, "C-g");
    assert_eq!(
        ed.mode,
        Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(ed.message, "Quit");

    // Empty RET on goto-line should fail gracefully
    let ed = keys(editor(), "M-x");
    let ed = typ(ed, "goto-line");
    let ed = keys(ed, "RET");
    assert!(matches!(ed.mode, Mode::Prompt(_)));
    let ed = keys(ed, "RET");
    assert_eq!(ed.message, "Not a line number: ");

    // Garbage non-number string on goto-line
    let ed = keys(editor(), "M-x");
    let ed = typ(ed, "goto-line");
    let ed = keys(ed, "RET");
    let ed = typ(ed, "not_a_number");
    let ed = keys(ed, "RET");
    assert_eq!(ed.message, "Not a line number: not_a_number");
}

#[test]
fn isearch_invalid_regex_and_rapid_reversals() {
    let mut ed = typ(editor(), "alpha beta gamma alpha");

    // Start regex isearch
    ed = keys(ed, "M-x");
    ed = typ(ed, "isearch-forward-regexp");
    ed = keys(ed, "RET");
    assert!(matches!(ed.mode, Mode::Isearch(_)));

    // Type invalid regex: open bracket "["
    ed = typ(ed, "[");
    assert!(ed.message.contains("Invalid regexp"));

    // Continue typing broken regex, then backspace it away
    ed = typ(ed, "*+?(");
    assert!(ed.message.contains("Invalid regexp"));
    ed = keys(ed, "DEL DEL DEL DEL DEL");
    assert_eq!(ed.message, "I-search: ");

    // Forward and backward flip without pattern
    ed = keys(ed, "C-s C-r C-s C-r");
    assert!(matches!(ed.mode, Mode::Isearch(_)));

    // Cancel restores origin
    let start_p = point(&ed);
    ed = typ(ed, "gamma");
    assert_ne!(point(&ed), start_p);
    ed = keys(ed, "C-g");
    assert_eq!(point(&ed), start_p);
    assert_eq!(
        ed.mode,
        Mode::Normal {
            prefix: Prefix::None
        }
    );
}

#[test]
fn query_replace_empty_and_illegal_keys() {
    let mut ed = typ(editor(), "foo bar foo");

    // Empty search pattern via prompt
    ed = keys(ed, "M-x");
    ed = typ(ed, "query-replace");
    ed = keys(ed, "RET");
    ed = keys(ed, "RET"); // Empty pattern
    assert_eq!(ed.message, "Query replace needs a pattern");
    assert_eq!(
        ed.mode,
        Mode::Normal {
            prefix: Prefix::None
        }
    );

    // Query replace with pattern not present in buffer
    let ed2 = typ(editor(), "foo bar foo");
    let ed2 = keys(ed2, "M-x");
    let ed2 = typ(ed2, "query-replace");
    let ed2 = keys(ed2, "RET");
    let ed2 = typ(ed2, "notfound");
    let ed2 = keys(ed2, "RET");
    let ed2 = typ(ed2, "replacement");
    let ed2 = keys(ed2, "RET");
    assert_eq!(ed2.message, "No match");
    assert_eq!(
        ed2.mode,
        Mode::Normal {
            prefix: Prefix::None
        }
    );
}

// =========================================================================
// Category 3: Undo & Kill Ring Lifecycle Invariants
// =========================================================================

#[test]
fn kill_ring_empty_and_illegal_operations() {
    let yank = find("yank").unwrap();
    let yank_pop = find("yank-pop").unwrap();
    let kill_region = find("kill-region").unwrap();
    let kill_ring_save = find("kill-ring-save").unwrap();

    // 1. Yank on empty kill ring
    let (ed1, _) = yank(editor(), Vec::new());
    assert_eq!(ed1.message, "Kill ring is empty");

    // 2. Yank-pop when last command was not yank
    let (ed2, _) = yank_pop(editor(), Vec::new());
    assert_eq!(ed2.message, "Previous command was not a yank");

    // 3. Kill-region without mark
    let (ed3, _) = kill_region(editor(), Vec::new());
    assert_eq!(ed3.message, "The mark is not set now");

    // 4. Kill-ring-save without mark
    let (ed4, _) = kill_ring_save(editor(), Vec::new());
    assert_eq!(ed4.message, "The mark is not set now");

    // 5. Normal kill-ring-save (M-w) with mark sets kill ring but keeps text
    let mut ed5 = typ(editor(), "hello world");
    ed5 = keys(ed5, "C-a C-SPC C-f C-f C-f C-f C-f"); // mark "hello"
    let (ed5, _) = kill_ring_save(ed5, Vec::new());
    assert_eq!(text(&ed5), "hello world");
    assert_eq!(ed5.kill_ring, vec![b"hello".to_vec()]);
}

#[test]
fn massive_kill_pop_and_undo_reversibility() {
    let initial_text = "The quick brown fox jumps over the lazy dog.\nSecond line.\nThird line.";
    let mut ed = typ(editor(), initial_text);

    // Perform a series of edits: kill lines, insert, delete, replace
    ed = keys(ed, "C-a C-k C-k"); // Kill first line and newline
    ed = typ(ed, "INSERTED ");
    ed = keys(ed, "C-y"); // Yank back
    ed = keys(ed, "M-y"); // Yank-pop

    // Now undo all operations back to the initial state
    // Consecutive undos should walk back through each edit group
    let mut steps = 0;
    while ed.buffer().text.to_bytes() != initial_text.as_bytes() && steps < 50 {
        ed = keys(ed, "C-/");
        steps += 1;
    }
    assert_eq!(
        text(&ed),
        initial_text,
        "Undo failed to perfectly recover initial state within {} steps",
        steps
    );
}

#[test]
fn view_mode_toggle_and_protection() {
    let view_mode = find("view-mode").unwrap();

    let ed = typ(editor(), "immutable text");
    assert!(!ed.buffer().view_only);

    // Toggle view-mode ON
    let (mut ed, _) = view_mode(ed, Vec::new());
    assert!(ed.buffer().view_only);
    assert_eq!(ed.message, "View mode on");

    // Attempting edits while view-mode is on must be blocked
    ed = typ(ed, "a");
    assert_eq!(text(&ed), "immutable text");
    assert_eq!(ed.message, "Buffer is read-only");

    // Toggle view-mode OFF
    let (mut ed, _) = view_mode(ed, Vec::new());
    assert!(!ed.buffer().view_only);
    assert_eq!(ed.message, "View mode off");

    // Now edits should succeed
    ed = typ(ed, "!");
    assert_eq!(text(&ed), "immutable text!");
}

#[test]
fn buffer_prompt_hostile_names_and_lifecycle() {
    let switch_to_buffer = find("switch-to-buffer").unwrap();
    let kill_buffer = find("kill-buffer").unwrap();

    // 1. switch-to-buffer with empty name is no-op
    let (ed, _) = switch_to_buffer(editor(), Vec::new());
    assert!(matches!(ed.mode, Mode::Prompt(_)));
    let ed = keys(ed, "RET");
    assert_eq!(ed.buffers.len(), 1);

    // 2. switch-to-buffer with new name creates buffer
    let (ed, _) = switch_to_buffer(ed, Vec::new());
    let ed = typ(ed, "novel_buf");
    let ed = keys(ed, "RET");
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(ed.buffer().name, "novel_buf");

    // 3. kill-buffer with nonexistent name reports error
    let (ed, _) = kill_buffer(ed, Vec::new());
    let ed = typ(ed, "ghost_buf");
    let ed = keys(ed, "RET");
    assert_eq!(ed.message, "No buffer named ghost_buf");
    assert_eq!(ed.buffers.len(), 2);

    // 4. kill-buffer with empty input targets current buffer
    let (ed, _) = kill_buffer(ed, Vec::new());
    let ed = keys(ed, "RET");
    assert_eq!(ed.buffers.len(), 1);
    assert_ne!(ed.buffer().name, "novel_buf");
}

#[test]
fn split_window_right_and_motion_boundaries() {
    let split_window_right = find("split-window-right").unwrap();
    let exchange_point_and_mark = find("exchange-point-and-mark").unwrap();

    let mut ed = typ(editor(), "single line");

    // split_window_right
    let (ed_split, _) = split_window_right(ed, Vec::new());
    assert_eq!(ed_split.leaf_count(), 2);

    // motion boundary messages
    ed = typ(editor(), "hello\nworld");
    // At line 2, next_line hits end of buffer
    ed = keys(ed, "C-n");
    assert_eq!(ed.message, "End of buffer");

    // Go to top, previous_line hits beginning of buffer
    ed = keys(ed, "C-p C-p");
    assert_eq!(ed.message, "Beginning of buffer");

    // exchange_point_and_mark without mark
    let (ed_nomark, _) = exchange_point_and_mark(editor(), Vec::new());
    assert_eq!(ed_nomark.message, "No mark set in this buffer");
}

#[test]
fn hostile_fuzz_sequence_no_panic() {
    let mut ed = editor();
    let hostile_inputs = [
        "C-x", "C-g", "M-x", "C-g", "C-s", "C-g", "C-w", "M-w", "C-y", "M-y", "C-k", "C-/",
        "C-a", "C-e", "C-n", "C-p", "DEL", "RET", "C-x b", "C-g", "C-x k", "C-g", "C-x 1",
        "C-x 2", "C-x 3", "C-x o", "C-d", "C-SPC", "C-x C-s",
    ];

    // Execute arbitrary hostile interleavings
    for _ in 0..3 {
        for &seq in &hostile_inputs {
            ed = keys(ed, seq);
            ed = typ(ed, "가a ");
        }
    }

    // Must never panic, buffer must remain intact and valid UTF-8
    assert!(!ed.buffers.is_empty());
    assert!(ed.window().point <= ed.buffer().text.len());
}


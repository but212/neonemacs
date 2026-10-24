//! Adversarial stress and invariant tests for core commands (§3, §4, §6, §8, §9).
//! Tests edge cases, hostile key sequences, mode transitions, and boundary conditions.

use crate::common::{editor, keys, point, text, typ};
use neonemacs::core::commands::find;
use neonemacs::core::commands::table::BUILTINS;
use neonemacs::core::editor::{Mode, Prefix, Window};

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
fn snap_and_insert_clamping_prevents_gap_buffer_panic() {
    let ed = typ(editor(), "abc");
    let ed = ed.move_point(9999);
    assert_eq!(point(&ed), 3);
    let ed = ed.self_insert('x');
    assert_eq!(text(&ed), "abcx");

    // Hostile window with OOB point: self_insert must clamp and never panic
    let oob_ed = typ(editor(), "abc").with_window(|w| Window { point: 9999, ..w });
    let oob_ed = oob_ed.self_insert('!');
    assert_eq!(text(&oob_ed), "abc!");
}

#[test]
fn prompt_hostile_backspaces_and_cancellation() {
    let mut ed = editor();
    // Enter M-x prompt
    ed = keys(ed, "M-x");
    assert!(matches!(ed.mode(), Mode::Prompt(_)));

    // Spam backspaces on empty prompt - must not underflow or panic
    for _ in 0..10 {
        ed = keys(ed, "DEL");
    }
    if let Mode::Prompt(p) = ed.mode() {
        assert_eq!(p.input, "");
    } else {
        panic!("Prompt mode lost after DEL");
    }

    // Type partial, then cancel with C-g
    ed = typ(ed, "nonexistent-command-12345");
    ed = keys(ed, "C-g");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(ed.message(), "Quit");

    // Empty RET on goto-line should fail gracefully
    let ed = keys(editor(), "M-x");
    let ed = typ(ed, "goto-line");
    let ed = keys(ed, "RET");
    assert!(matches!(ed.mode(), Mode::Prompt(_)));
    let ed = keys(ed, "RET");
    assert_eq!(ed.message(), "Not a line number: ");

    // Garbage non-number string on goto-line
    let ed = keys(editor(), "M-x");
    let ed = typ(ed, "goto-line");
    let ed = keys(ed, "RET");
    let ed = typ(ed, "not_a_number");
    let ed = keys(ed, "RET");
    assert_eq!(ed.message(), "Not a line number: not_a_number");
}

#[test]
fn isearch_invalid_regex_and_rapid_reversals() {
    let mut ed = typ(editor(), "alpha beta gamma alpha");

    // Start regex isearch
    ed = keys(ed, "M-x");
    ed = typ(ed, "isearch-forward-regexp");
    ed = keys(ed, "RET");
    assert!(matches!(ed.mode(), Mode::Isearch(_)));

    // Type invalid regex: open bracket "["
    ed = typ(ed, "[");
    assert!(ed.message().contains("Invalid regexp"));

    // Continue typing broken regex, then backspace it away
    ed = typ(ed, "*+?(");
    assert!(ed.message().contains("Invalid regexp"));
    ed = keys(ed, "DEL DEL DEL DEL DEL");
    assert_eq!(ed.message(), "I-search: ");

    // Forward and backward flip without pattern
    ed = keys(ed, "C-s C-r C-s C-r");
    assert!(matches!(ed.mode(), Mode::Isearch(_)));

    // Cancel restores origin
    let start_p = point(&ed);
    ed = typ(ed, "gamma");
    assert_ne!(point(&ed), start_p);
    ed = keys(ed, "C-g");
    assert_eq!(point(&ed), start_p);
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
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
    assert_eq!(ed.message(), "Query replace needs a pattern");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
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
    assert_eq!(ed2.message(), "No match");
    assert_eq!(
        ed2.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
}

#[test]
fn kill_ring_empty_and_illegal_operations() {
    let yank = find("yank").unwrap();
    let yank_pop = find("yank-pop").unwrap();
    let kill_region = find("kill-region").unwrap();
    let kill_ring_save = find("kill-ring-save").unwrap();

    // 1. Yank on empty kill ring
    let (ed1, _) = yank(editor(), Vec::new());
    assert_eq!(ed1.message(), "Kill ring is empty");

    // 2. Yank-pop when last command was not yank
    let (ed2, _) = yank_pop(editor(), Vec::new());
    assert_eq!(ed2.message(), "Previous command was not a yank");

    // 3. Kill-region without mark
    let (ed3, _) = kill_region(editor(), Vec::new());
    assert_eq!(ed3.message(), "The mark is not set now");

    // 4. Kill-ring-save without mark
    let (ed4, _) = kill_ring_save(editor(), Vec::new());
    assert_eq!(ed4.message(), "The mark is not set now");

    // 5. Normal kill-ring-save (M-w) with mark sets kill ring but keeps text
    let mut ed5 = typ(editor(), "hello world");
    ed5 = keys(ed5, "C-a C-SPC C-f C-f C-f C-f C-f"); // mark "hello"
    let (ed5, _) = kill_ring_save(ed5, Vec::new());
    assert_eq!(text(&ed5), "hello world");
    assert_eq!(ed5.kill_ring().entries(), &[b"hello".to_vec()]);
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
    assert_eq!(ed.message(), "View mode on");

    // Attempting edits while view-mode is on must be blocked
    ed = typ(ed, "a");
    assert_eq!(text(&ed), "immutable text");
    assert_eq!(ed.message(), "Buffer is read-only");

    // Toggle view-mode OFF
    let (mut ed, _) = view_mode(ed, Vec::new());
    assert!(!ed.buffer().view_only);
    assert_eq!(ed.message(), "View mode off");

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
    assert!(matches!(ed.mode(), Mode::Prompt(_)));
    let ed = keys(ed, "RET");
    assert_eq!(ed.buffers().len(), 1);

    // 2. switch-to-buffer with new name creates buffer
    let (ed, _) = switch_to_buffer(ed, Vec::new());
    let ed = typ(ed, "novel_buf");
    let ed = keys(ed, "RET");
    assert_eq!(ed.buffers().len(), 2);
    assert_eq!(ed.buffer().name, "novel_buf");

    // 3. kill-buffer with nonexistent name reports error
    let (ed, _) = kill_buffer(ed, Vec::new());
    let ed = typ(ed, "ghost_buf");
    let ed = keys(ed, "RET");
    assert_eq!(ed.message(), "No buffer named ghost_buf");
    assert_eq!(ed.buffers().len(), 2);

    // 4. kill-buffer with empty input targets current buffer
    let (ed, _) = kill_buffer(ed, Vec::new());
    let ed = keys(ed, "RET");
    assert_eq!(ed.buffers().len(), 1);
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
    assert_eq!(ed.message(), "End of buffer");

    // Go to top, previous_line hits beginning of buffer
    ed = keys(ed, "C-p C-p");
    assert_eq!(ed.message(), "Beginning of buffer");

    // exchange_point_and_mark without mark
    let (ed_nomark, _) = exchange_point_and_mark(editor(), Vec::new());
    assert_eq!(ed_nomark.message(), "No mark set in this buffer");
}

#[test]
fn hostile_fuzz_sequence_no_panic() {
    let mut ed = editor();
    let hostile_inputs = [
        "C-x", "C-g", "M-x", "C-g", "C-s", "C-g", "C-w", "M-w", "C-y", "M-y", "C-k", "C-/", "C-a",
        "C-e", "C-n", "C-p", "DEL", "RET", "C-x b", "C-g", "C-x k", "C-g", "C-x 1", "C-x 2",
        "C-x 3", "C-x o", "C-d", "C-SPC", "C-x C-s",
    ];

    // Execute arbitrary hostile interleavings
    for _ in 0..3 {
        for &seq in &hostile_inputs {
            ed = keys(ed, seq);
            ed = typ(ed, "가a ");
        }
    }

    // Must never panic, buffer must remain intact and valid UTF-8
    assert!(!ed.buffers().is_empty());
    assert!(ed.window().point <= ed.buffer().text.len());
}

#[test]
fn prompt_hostile_tab_completion_and_common_prefix() {
    let mut ed = editor();
    ed = keys(ed, "M-x");
    assert!(matches!(ed.mode(), Mode::Prompt(_)));

    // Spam TAB on empty prompt: candidate note produced without crash or input corruption
    ed = keys(ed, "TAB TAB TAB");
    if let Mode::Prompt(p) = ed.mode() {
        assert_eq!(p.input, "");
    } else {
        panic!("Prompt mode lost on empty TAB spam");
    }

    // "is" matches isearch-backward, isearch-backward-regexp, isearch-forward, isearch-forward-regexp
    ed = typ(ed, "is");
    ed = keys(ed, "TAB");
    if let Mode::Prompt(p) = ed.mode() {
        assert_eq!(p.input, "isearch-");
    }
    // Candidate list note must include options with their key bindings
    assert!(ed.message().contains("isearch-forward"));
    assert!(ed.message().contains("isearch-backward"));

    // Repeated TAB on already-common prefix must be idempotent
    ed = keys(ed, "TAB");
    if let Mode::Prompt(p) = ed.mode() {
        assert_eq!(p.input, "isearch-");
    }

    // Narrow down with 'b' -> isearch-b -> TAB expands to isearch-backward
    ed = typ(ed, "b");
    ed = keys(ed, "TAB");
    if let Mode::Prompt(p) = ed.mode() {
        assert_eq!(p.input, "isearch-backward");
    }

    // Narrow down to unique candidate "-r" -> isearch-backward-regexp
    ed = typ(ed, "-r");
    ed = keys(ed, "TAB");
    if let Mode::Prompt(p) = ed.mode() {
        assert_eq!(p.input, "isearch-backward-regexp");
    }

    // Completely nonexistent prefix TAB reports "No match" without destroying input
    ed = typ(ed, "xyz_not_a_command");
    ed = keys(ed, "TAB");
    assert_eq!(ed.message(), "No match");
    if let Mode::Prompt(p) = ed.mode() {
        assert_eq!(p.input, "isearch-backward-regexpxyz_not_a_command");
    }

    // C-g cancels gracefully
    ed = keys(ed, "C-g");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(ed.message(), "Quit");
}

#[test]
fn prompt_buffer_completion_and_existing_buffer_switch() {
    let mut ed = editor();

    // Create multiple buffers including Korean multibyte names
    let switch_to_buffer = find("switch-to-buffer").unwrap();
    let (e, _) = switch_to_buffer(ed, Vec::new());
    let e = typ(e, "work_log_2026");
    ed = keys(e, "RET");

    let (e, _) = switch_to_buffer(ed, Vec::new());
    let e = typ(e, "work_log_archive");
    ed = keys(e, "RET");

    let (e, _) = switch_to_buffer(ed, Vec::new());
    let e = typ(e, "한글_문서_1");
    ed = keys(e, "RET");

    let (e, _) = switch_to_buffer(ed, Vec::new());
    let e = typ(e, "한글_문서_2");
    ed = keys(e, "RET");

    // 1. Buffer completion on "work_l" expands to common prefix "work_log_"
    let (e, _) = switch_to_buffer(ed, Vec::new());
    let e = typ(e, "work_l");
    let mut e = keys(e, "TAB");
    if let Mode::Prompt(p) = e.mode() {
        assert_eq!(p.input, "work_log_");
    }
    assert!(e.message().contains("work_log_2026"));
    assert!(e.message().contains("work_log_archive"));

    // 2. Select existing buffer "work_log_2026" via completion
    e = typ(e, "2");
    e = keys(e, "TAB");
    if let Mode::Prompt(p) = e.mode() {
        assert_eq!(p.input, "work_log_2026");
    }
    ed = keys(e, "RET");
    assert_eq!(ed.buffer().name, "work_log_2026");

    // 3. Multibyte UTF-8 buffer completion on "한글_"
    let (e, _) = switch_to_buffer(ed, Vec::new());
    let e = typ(e, "한글_");
    let mut e = keys(e, "TAB");
    if let Mode::Prompt(p) = e.mode() {
        assert_eq!(p.input, "한글_문서_");
    }
    assert!(e.message().contains("한글_문서_1"));
    assert!(e.message().contains("한글_문서_2"));

    // Select existing Hangul buffer "한글_문서_1"
    e = typ(e, "1");
    e = keys(e, "TAB");
    if let Mode::Prompt(p) = e.mode() {
        assert_eq!(p.input, "한글_문서_1");
    }
    ed = keys(e, "RET");
    assert_eq!(ed.buffer().name, "한글_문서_1");

    // 4. Buffer completion with no match
    let (e, _) = switch_to_buffer(ed, Vec::new());
    let e = typ(e, "nonexistent_prefix_999");
    let e = keys(e, "TAB");
    assert_eq!(e.message(), "No match");
    ed = keys(e, "C-g");

    // 5. kill-buffer with completion
    let kill_buffer = find("kill-buffer").unwrap();
    let (e, _) = kill_buffer(ed, Vec::new());
    let e = typ(e, "work_log_a");
    let e = keys(e, "TAB");
    if let Mode::Prompt(p) = e.mode() {
        assert_eq!(p.input, "work_log_archive");
    }
    let initial_count = e.buffers().len();
    ed = keys(e, "RET");
    assert_eq!(ed.buffers().len(), initial_count - 1);
    assert!(!ed.buffers().iter().any(|b| b.name == "work_log_archive"));
}

#[test]
fn prompt_query_replace_chained_adversarial() {
    let mut ed = typ(editor(), "alpha beta alpha gamma");

    // Boundary check: Point is at end of buffer -> query-replace has no match forward
    ed = keys(ed, "M-x");
    ed = typ(ed, "query-replace");
    ed = keys(ed, "RET");
    ed = typ(ed, "alpha");
    ed = keys(ed, "RET");
    ed = typ(ed, "delta");
    ed = keys(ed, "RET");
    assert_eq!(ed.message(), "No match");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );

    // Move to beginning and perform two-stage prompt query-replace: from -> to -> replace
    ed = keys(ed, "C-a");
    ed = keys(ed, "M-x");
    ed = typ(ed, "query-replace");
    ed = keys(ed, "RET");
    assert!(matches!(ed.mode(), Mode::Prompt(_)));
    assert_eq!(ed.message(), "Query replace: ");

    ed = typ(ed, "alpha");
    ed = keys(ed, "RET");
    assert!(matches!(ed.mode(), Mode::Prompt(_)));
    assert_eq!(ed.message(), "Query replace alpha with: ");

    ed = typ(ed, "delta");
    ed = keys(ed, "RET");
    assert!(matches!(ed.mode(), Mode::QueryReplace(_)));

    // '!' replaces all occurrences
    ed = keys(ed, "!");
    assert_eq!(text(&ed), "delta beta delta gamma");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );

    // Hostile regex syntax error in query-replace-regexp
    ed = keys(ed, "M-x");
    ed = typ(ed, "query-replace-regexp");
    ed = keys(ed, "RET");
    ed = typ(ed, "[a-"); // Broken unclosed class regex
    ed = keys(ed, "RET");
    ed = typ(ed, "repl");
    ed = keys(ed, "RET");
    // Invalid regex must be caught gracefully and reported in the echo area without panic
    assert_eq!(ed.message(), "unclosed [");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
}

#[test]
fn prompt_shell_command_adversarial() {
    let mut ed = editor();
    ed = keys(ed, "M-x");
    ed = typ(ed, "shell-command");
    ed = keys(ed, "RET");
    assert!(matches!(ed.mode(), Mode::Prompt(_)));

    // Empty RET on shell-command prompt must be a clean no-op
    ed = keys(ed, "RET");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
}

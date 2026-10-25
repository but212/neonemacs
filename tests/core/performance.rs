//! Performance regression tests for Core transitions (§2, §5, §14).

use neonemacs::core::config::Config;
use neonemacs::core::editor::Editor;
use neonemacs::core::file::Backing;
use neonemacs::core::text::Text;
use neonemacs::core::undo::Edit;
use std::time::{Duration, Instant};

#[test]
fn editor_apply_must_not_deep_clone_entire_buffer() {
    let ed = Editor::new(Config::default());
    // Create a 32MB buffer (Config default max is 64MB)
    let size = 32 * 1024 * 1024;
    let large_bytes = vec![b'a'; size];
    let (ed, id) = ed.add_buffer("large", Text::from_bytes(&large_bytes), Backing::None);
    let ed = neonemacs::core::commands::buffer_window::switch_to(ed, id);

    let edit = Edit::Insert {
        pos: 0,
        bytes: b"x".to_vec(),
    };

    // First edit initializes the gap
    let (ed, _) = ed.apply(edit);

    // Second edit must be O(1) in-place without deep-cloning the 32MB buffer.
    // If Editor::apply clones the entire Buffer on each keypress, this will take tens of ms.
    let edit2 = Edit::Insert {
        pos: 1,
        bytes: b"y".to_vec(),
    };
    let start = Instant::now();
    let (ed, _) = ed.apply(edit2);
    let elapsed = start.elapsed();

    assert_eq!(ed.buffer().text().len(), size + 2);
    assert!(
        elapsed < Duration::from_millis(5),
        "Subsequent insert took {:?} (expected < 5ms). Full buffer clone detected in Editor::apply!",
        elapsed
    );
}

#[test]
fn large_file_nth_line_start_and_line_navigation_latency() {
    // 16MB buffer with 100,000 lines (each ~160 bytes)
    let line = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\n";
    let line_len = line.len();
    let num_lines = 100_000;
    let mut data = Vec::with_capacity(line_len * num_lines);
    for _ in 0..num_lines {
        data.extend_from_slice(line.as_bytes());
    }

    let mut text = Text::from_vec(data);
    // Insert text in the middle to create an active gap right at the 50,000th line
    let mid_pos = line_len * 50_000;
    text = text.insert(mid_pos, b"# GAP INSERTION POINT #\n");

    // Navigating to late lines should not suffer from un-vectorized byte-by-byte looping.
    // In release mode or even debug mode with SIMD slice searching, scanning 50,000 lines is near-instant (< 30ms).
    // With byte-at-a-time loop calling byte_at() millions of times with bounds checking and branches, it degrades drastically.
    let start = Instant::now();
    let pos_80k = text.nth_line_start(80_000);
    let elapsed_nth = start.elapsed();

    // Since we inserted 1 line at 50,000, line 80,000 is original line (80,000 - 2) + inserted line len
    let expected_pos_80k = (80_000 - 2) * line_len + b"# GAP INSERTION POINT #\n".len();
    assert_eq!(pos_80k, expected_pos_80k);

    let start = Instant::now();
    let lend = text.line_end(pos_80k);
    let lstart = text.line_start(lend);
    let elapsed_single = start.elapsed();

    assert_eq!(lstart, pos_80k);
    assert_eq!(lend, pos_80k + line_len - 1);

    // Strict performance assertion: nth_line_start for 80k lines must complete under 60ms (sub-millisecond in release mode)
    assert!(
        elapsed_nth < Duration::from_millis(60),
        "nth_line_start(80_000) took {:?} (expected < 60ms). Byte-by-byte linear search overhead detected!",
        elapsed_nth
    );
    assert!(
        elapsed_single < Duration::from_millis(1),
        "line_start/line_end took {:?} (expected < 1ms)",
        elapsed_single
    );
}

#[test]
fn lua_comment_dwim_100_lines_on_32mb_buffer_is_fast() {
    let lua = neonemacs::shell::lua::new_state().unwrap();
    let ed = neonemacs::shell::lua::bootstrap(Editor::new(Config::default()), &lua);

    // Create 32MB buffer with ~100 lines at start
    let mut data = Vec::with_capacity(32 * 1024 * 1024);
    for _ in 0..100 {
        data.extend_from_slice(b"let x = 42;\n");
    }
    // Pad to 32MB
    let pad = (32 * 1024 * 1024) - data.len();
    data.resize(data.len() + pad, b' ');

    let (ed, id) = ed.add_buffer("test.rs", Text::from_bytes(&data), Backing::None);
    let ed = neonemacs::core::commands::buffer_window::switch_to(ed, id);

    // Mark region covering first 100 lines (from 0 to 1200)
    let ed = ed.with_current_buffer(|b| b.set_mark(Some(1200)));
    let ed = ed.with_window(|w| neonemacs::core::editor::Window { point: 0, ..w });

    let start = Instant::now();
    let ed = neonemacs::shell::handle(
        ed,
        Some(&lua),
        neonemacs::core::effect::Event::Run("comment-dwim".to_owned()),
    );
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "comment-dwim on 100 lines of 32MB buffer took {:?} (expected < 100ms). Full Editor clone in mutate_editor detected!",
        elapsed
    );
    // Verify first line commented
    assert_eq!(&ed.buffer().text().bytes(0..15), b"// let x = 42;\n");
}

//! Performance regression tests for Core transitions (§2, §5, §14).

use neonemacs::core::Backing;
use neonemacs::core::Config;
use neonemacs::core::Edit;
use neonemacs::core::Editor;
use neonemacs::core::Text;
use std::time::{Duration, Instant};

#[test]
fn editor_apply_must_not_deep_clone_entire_buffer() {
    let ed = Editor::new(Config::default());
    // Create a 32MB buffer (Config default max is 64MB)
    let size = 32 * 1024 * 1024;
    let large_bytes = vec![b'a'; size];
    let (ed, id) = ed.add_buffer("large", Text::from_bytes(&large_bytes), Backing::None);
    let ed = neonemacs::core::switch_to(ed, id);

    let edit = Edit::insert(0, b"x".as_slice());

    // First edit initializes the gap
    let (ed, _) = ed.apply(edit);

    // Second edit must be O(1) in-place without deep-cloning the 32MB buffer.
    // If Editor::apply clones the entire Buffer on each keypress, this will take tens of ms.
    let edit2 = Edit::insert(1, b"y".as_slice());
    let start = Instant::now();
    let (ed, _) = ed.apply(edit2);
    let elapsed = start.elapsed();

    assert_eq!(ed.buffer().text().len(), size + 2);
    assert!(
        elapsed < Duration::from_millis(5),
        "Subsequent insert took {elapsed:?} (expected < 5ms). Full buffer clone detected in Editor::apply!"
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
        "nth_line_start(80_000) took {elapsed_nth:?} (expected < 60ms). Byte-by-byte linear search overhead detected!"
    );
    assert!(
        elapsed_single < Duration::from_millis(1),
        "line_start/line_end took {elapsed_single:?} (expected < 1ms)"
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
    let ed = neonemacs::core::switch_to(ed, id);

    // Mark region covering first 100 lines (from 0 to 1200)
    let ed = ed.with_current_buffer(|b| b.set_mark(Some(1200)));
    let ed = ed.with_window(|w| neonemacs::core::Window { point: 0, ..w });

    let start = Instant::now();
    let ed = neonemacs::shell::handle(
        ed,
        Some(&lua),
        neonemacs::core::effect::Event::Run("comment-dwim".to_owned()),
    );
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "comment-dwim on 100 lines of 32MB buffer took {elapsed:?} (expected < 100ms). Full Editor clone in mutate_editor detected!"
    );
    // Verify first line commented
    assert_eq!(&ed.buffer().text().bytes(0..15), b"// let x = 42;\n");
}

#[test]
fn oversized_closed_undo_group_is_evicted_on_boundary() {
    let config = Config {
        undo_limit: 4096,
        ..Config::default()
    };
    let ed = Editor::new(config);
    let large = vec![b'a'; 16 * 1024];
    let (ed, id) = ed.add_buffer("undo-limit", Text::from_bytes(&large), Backing::None);
    let ed = neonemacs::core::switch_to(ed, id);

    let (ed, _) = ed.apply(Edit::delete(0, large[..8192].to_vec()));
    let ed = ed.end_command();

    let ed = neonemacs::shell::handle(
        ed,
        None,
        neonemacs::core::effect::Event::Run("undo".to_owned()),
    );
    assert_eq!(ed.message(), "No further undo information");
    assert_eq!(ed.buffer().text().len(), 8192);
}

#[test]
fn ultra_long_single_line_scroll_and_frame_stay_bounded() {
    let ed = Editor::new(Config::default());
    let size = 10 * 1024 * 1024;
    let line = vec![b'x'; size];
    let (ed, id) = ed.add_buffer("long-line", Text::from_bytes(&line), Backing::None);
    let ed = neonemacs::core::switch_to(ed, id);

    // 1. Near line start: must not copy or highlight the 10MB tail.
    let ed = ed.move_point(200);
    let start = Instant::now();
    let ed = neonemacs::core::render::scroll(ed, 120, 40);
    let frame = neonemacs::core::render::frame(&ed, 120, 40);
    let elapsed_head = start.elapsed();
    assert!(!frame.rows.is_empty());
    assert!(
        elapsed_head < Duration::from_millis(60),
        "scroll + frame near start of 10MB single line took {elapsed_head:?} (expected < 60ms)"
    );

    // 2. Scrolled 5MB horizontally into the 10MB single line: must skip off-screen glyph allocations.
    let ed = ed.move_point(size / 2);
    let start = Instant::now();
    let ed = neonemacs::core::render::scroll(ed, 120, 40);
    let frame = neonemacs::core::render::frame(&ed, 120, 40);
    let elapsed_mid = start.elapsed();
    assert!(!frame.rows.is_empty());
    assert!(
        elapsed_mid < Duration::from_millis(150),
        "scroll + frame at 5MB offset of 10MB single line took {elapsed_mid:?} (expected < 150ms)"
    );
}

#[test]
fn wal_flush_streams_split_gap_buffer_without_full_copy() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("split.txt");
    let initial = b"0123456789abcdefghijklmnopqrstuvwxyz".to_vec();
    std::fs::write(&file_path, &initial).unwrap();

    let ed = Editor::new(Config::default());
    let fx = neonemacs::core::request_open(&ed, &file_path, None, true, Vec::new());
    let ed = neonemacs::shell::perform_all(ed, None, fx);
    let disk = neonemacs::shell::file::disk_state(&file_path);

    // Edit at offset 10 so the gap splits the buffer between left and right halves.
    let (ed, _) = ed.apply(Edit::insert(10, b"[MID]".as_slice()));
    let (ed, fx) = neonemacs::core::step(ed, neonemacs::core::effect::Event::Idle);
    assert!(
        fx.iter()
            .any(|e| matches!(e, neonemacs::core::effect::Effect::FlushWal { .. }))
    );

    let _ed = neonemacs::shell::perform_all(ed, None, fx);
    let wal = neonemacs::shell::wal::read_wal(&file_path, disk).unwrap();
    assert_eq!(wal.bytes, b"0123456789[MID]abcdefghijklmnopqrstuvwxyz");
}


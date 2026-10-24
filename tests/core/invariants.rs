//! Core invariants and anchor preservation tests (I-01, I-23, I-25).

use neonemacs::core::buffer::Buffer;
use neonemacs::core::commands::request_open;
use neonemacs::core::config::Config;
use neonemacs::core::editor::{Editor, snap};
use neonemacs::core::file::{Backing, DiskState, LoadExtent};
use neonemacs::core::text::Text;
use neonemacs::shell::file;
use neonemacs::shell::perform_all;
use neonemacs::shell::wal;
use std::fs;
use std::path::PathBuf;

#[test]
fn restore_content_must_snap_saved_point_and_mark() {
    let ed = Editor::new(Config::default());
    let path = PathBuf::from("long_to_short.txt");
    let initial_text = Text::from_bytes(b"line 1\nline 2\nline 3\nline 4\nline 5\nlong text line");
    let (mut ed, id) = ed.add_buffer(
        "test",
        initial_text,
        Backing::File {
            path: path.clone(),
            disk: DiskState::Unknown,
            extent: LoadExtent::Whole,
        },
    );

    // Set saved_point far down at offset 45 and mark at offset 40
    ed = ed.with_buffer(id, |b| Buffer {
        saved_point: 45,
        mark: Some(40),
        ..b
    });

    // WAL recovers very short content: only 5 bytes
    let recovered_ed = ed.restore_buffer_content(&path, b"short");
    let buf = recovered_ed.buffer_by_id(id);

    assert_eq!(buf.text.len(), 5);
    assert!(
        buf.saved_point <= buf.text.len(),
        "I-01 violation: saved_point ({}) exceeds restored text len ({})",
        buf.saved_point,
        buf.text.len()
    );
    assert_eq!(
        buf.saved_point,
        snap(&buf.text, buf.saved_point),
        "saved_point must be snapped to valid unit boundary"
    );
    assert!(
        buf.mark.unwrap() <= buf.text.len(),
        "I-01 violation: mark exceeds restored text len"
    );
}

#[test]
fn open_files_with_line_arg_and_shrunk_wal_preserves_i01() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("shrunk_on_wal.txt");
    // Original file has 30 lines
    let mut initial_content = String::new();
    for i in 1..=30 {
        initial_content.push_str(&format!("Line number {i} with some content\n"));
    }
    fs::write(&file_path, &initial_content).unwrap();

    let disk = file::disk_state(&file_path);
    // WAL content has only 1 short line
    wal::write_wal(&file_path, disk, b"Only line 1\n").unwrap();

    // Simulate opening with +25
    let ed = Editor::new(Config::default());
    let fx = request_open(&ed, file_path.clone(), Some(25), false, Vec::new());
    let mut ed = perform_all(ed, None, fx);

    // WAL recovery
    let wal_bytes = wal::read_wal(&file_path, disk).unwrap();
    ed = ed.restore_buffer_content(&file_path, &wal_bytes);

    // Switch to buffer as open_files does
    let buf_id = ed.buffers().iter().last().unwrap().id;
    let switched_ed = neonemacs::core::commands::buffer_window::switch_to(ed, buf_id);

    // Verify that WAL content was indeed restored
    assert_eq!(
        switched_ed.buffer().text.to_bytes(),
        b"Only line 1\n",
        "WAL content must be restored into the buffer"
    );

    // Buffer's saved_point must be normalized to restored text length (I-01)
    let buf = switched_ed.buffer();
    assert!(
        buf.saved_point <= buf.text.len(),
        "I-01 violation: buffer saved_point ({}) exceeds restored text len ({})",
        buf.saved_point,
        buf.text.len()
    );
}

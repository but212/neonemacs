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

    assert_eq!(ed.buffer().text.len(), size + 2);
    assert!(
        elapsed < Duration::from_millis(5),
        "Subsequent insert took {:?} (expected < 5ms). Full buffer clone detected in Editor::apply!",
        elapsed
    );
}

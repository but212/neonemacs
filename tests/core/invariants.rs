//! Core invariants and anchor preservation tests (I-01).

use neonemacs::core::Config;
use neonemacs::core::Doc;
use neonemacs::core::Edit;
use neonemacs::core::Editor;
use neonemacs::core::Eol;

#[test]
fn with_text_clears_stale_undo() {
    let ed = Editor::new(Config::default());
    let initial_text = Doc::from_bytes(b"line 1\nline 2\nline 3\n");
    let (ed, id) = ed.add_buffer("test", initial_text, neonemacs::core::Backing::None);

    let ed = ed.with_buffer(id, |b| {
        b.apply_edit(Edit::insert(2, b"c".as_slice()), |a| a, 1 << 20)
            .0
    });

    let replaced = ed.with_buffer(id, |b| {
        b.with_text(Doc::from_bytes(b"PRE_LINE\nline 1\nline 2\nline 3\n"))
            .set_eol(Eol::Lf)
    });
    let buf = replaced.buffer_by_id(id);

    assert!(buf.undo().groups().is_empty());

    let (switched_ed, fx) = replaced.undo(Vec::new());
    assert_eq!(switched_ed.message(), "No further undo information");
    assert_eq!(fx, [] as [neonemacs::core::effect::Effect; 0]);
}

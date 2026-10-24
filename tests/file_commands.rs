//! Commands that touch files: saving, reverting, opening, and quitting with unsaved changes.
//! They run against real files in a temporary directory.

use neonemacs::core::commands::{confirmed, request_open};
use neonemacs::core::config::Config;
use neonemacs::core::editor::{Confirm, Editor, Mode, Prefix, Window};
use neonemacs::core::effect::{Effect, Event};
use neonemacs::core::file::{Backing, LoadExtent};
use neonemacs::core::keymap::{Code, Key, parse_keys};
use neonemacs::core::step;
use neonemacs::shell::file;
use neonemacs::shell::{handle, perform_all};
use std::fs;
use std::path::Path;

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

fn visit(ed: Editor, path: &Path) -> Editor {
    let fx = request_open(&ed, path.to_path_buf(), None, true, Vec::new());
    let ed = perform_all(ed, None, fx);
    ed.with_window(|w| Window { point: 0, ..w })
}

#[test]
fn truncated_save_confirms_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    fs::write(&path, b"0123456789").unwrap();
    let ed = editor().set_config(Config {
        max_file_size: 4,
        ..*editor().config()
    });
    let mut ed = visit(ed, &path);

    ed = keys(ed, "C-x C-s");
    assert!(matches!(
        ed.mode(),
        Mode::Confirm(Confirm::SaveTruncated(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), b"0123456789");
    ed = keys(ed, "y");
    assert_eq!(fs::read(&path).unwrap(), b"0123");

    ed = typ(ed, "x");
    ed = keys(ed, "C-x C-s");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(fs::read(&path).unwrap(), b"x0123");
}

#[test]
fn external_change_asks_before_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"one").unwrap();
    let ed = visit(editor(), &path);
    let mut ed = typ(ed, "x");

    fs::write(&path, b"external change").unwrap();
    ed = keys(ed, "C-x C-s");
    assert!(matches!(ed.mode(), Mode::Confirm(Confirm::SaveExternal(_))));
    assert_eq!(fs::read(&path).unwrap(), b"external change");

    ed = keys(ed, "n");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(fs::read(&path).unwrap(), b"external change");

    keys(ed, "C-x C-s y");
    assert_eq!(fs::read(&path).unwrap(), b"xone");
}

#[test]
fn own_save_is_not_external() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let ed = visit(editor(), &path);

    let mut ed = typ(ed, "d");
    ed = keys(ed, "C-x C-s");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(fs::read(&path).unwrap(), b"dabc");

    ed = typ(ed, "e");
    ed = keys(ed, "C-x C-s");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(fs::read(&path).unwrap(), b"deabc");
}

#[test]
fn unknown_disk_state_saves_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.txt");
    let ed = visit(editor(), &path);
    let mut ed = typ(ed, "hi");
    ed = keys(ed, "C-x C-s");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(fs::read(&path).unwrap(), b"hi");
}

#[test]
fn quit_asks_only_with_unsaved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();

    let clean = visit(editor(), &path);
    let clean = keys(clean, "C-x C-c");
    assert!(clean.quit());

    let dirty = visit(editor(), &path);
    let mut dirty = typ(dirty, "x");
    dirty = keys(dirty, "C-x C-c");
    assert!(matches!(dirty.mode(), Mode::Confirm(Confirm::QuitUnsaved)));
    assert!(!dirty.quit());
    dirty = keys(dirty, "n");
    assert!(!dirty.quit());
    dirty = keys(dirty, "C-x C-c y");
    assert!(dirty.quit());
}

#[test]
fn undo_survives_reopen_when_file_matches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let ed = visit(editor(), &path);
    let ed = keys(ed, "C-e");
    let ed = typ(ed, "d");
    let _ = keys(ed, "C-x C-s");

    let again = visit(editor(), &path);
    let again = keys(again, "C-/");
    assert_eq!(text(&again), "abc");
}

#[test]
fn file_commands_find_goto_revert_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"one\ntwo\nthree\n").unwrap();
    let mut ed = editor();

    ed = keys(ed, "C-x C-f");
    ed = typ(ed, &path.display().to_string());
    ed = keys(ed, "RET");
    assert_eq!(text(&ed), "one\ntwo\nthree\n");

    ed = keys(ed, "M-x");
    ed = typ(ed, "goto-line");
    ed = keys(ed, "RET");
    ed = typ(ed, "2");
    ed = keys(ed, "RET");
    assert_eq!(point(&ed), 4);

    ed = typ(ed, "x");
    ed = keys(ed, "M-x");
    ed = typ(ed, "revert-buffer");
    ed = keys(ed, "RET");
    assert!(matches!(
        ed.mode(),
        Mode::Confirm(Confirm::RevertModified(_))
    ));
    ed = keys(ed, "y");
    assert_eq!(text(&ed), "one\ntwo\nthree\n");

    let copy = dir.path().join("copy.txt");
    ed = keys(ed, "M-x");
    ed = typ(ed, "write-file");
    ed = keys(ed, "RET");
    ed = typ(ed, &copy.display().to_string());
    keys(ed, "RET");
    assert_eq!(fs::read(&copy).unwrap(), b"one\ntwo\nthree\n");
}

#[test]
fn save_requests_disk_stat_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let ed = visit(editor(), &path);
    let id = ed.window().buffer;
    let mut ed = typ(ed, "x");

    let mut fx: Vec<Effect> = Vec::new();
    for k in parse_keys("C-x C-s").unwrap() {
        let (next, effects) = step(ed, Event::Key(k));
        ed = next;
        fx.extend(effects);
    }
    assert_eq!(
        fx,
        vec![Effect::StatDisk {
            id,
            path: path.clone()
        }]
    );
    assert_eq!(fs::read(&path).unwrap(), b"abc");
}

#[test]
fn written_records_disk_and_emits_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let ed = visit(editor(), &path);
    let id = ed.window().buffer;
    let ed = typ(ed, "x");
    fs::write(&path, b"xabc").unwrap();
    let disk = file::disk_state(&path);

    let (ed, fx) = step(
        ed,
        Event::Written {
            id,
            result: Ok(disk),
        },
    );
    let buf = ed.buffer_by_id(id);
    assert!(!buf.modified);
    assert_eq!(
        buf.backing,
        Backing::File {
            path: path.clone(),
            disk,
            extent: LoadExtent::Whole
        }
    );
    let bytes = buf.undo.encode(disk).unwrap();
    assert_eq!(fx, vec![Effect::WriteUndo { id, path, bytes }]);
}

#[test]
fn write_failure_keeps_buffer_modified() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let ed = visit(editor(), &path);
    let id = ed.window().buffer;
    let ed = typ(ed, "x");

    let (ed, fx) = step(
        ed,
        Event::Written {
            id,
            result: Err("disk full".to_owned()),
        },
    );
    assert!(fx.is_empty());
    assert_eq!(ed.message(), "Save failed: disk full");
    assert!(ed.buffer_by_id(id).modified);
}

#[test]
fn confirmed_external_writes_without_recheck() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let ed = visit(editor(), &path);
    let id = ed.window().buffer;
    let ed = typ(ed, "x");
    fs::write(&path, b"external").unwrap();

    let (_, fx) = confirmed(ed, Confirm::SaveExternal(id), Vec::new());
    assert_eq!(
        fx,
        vec![Effect::WriteFile {
            id,
            path,
            bytes: b"xabc".to_vec()
        }]
    );
}

#[test]
fn undo_write_failure_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, b"abc").unwrap();
    let ed = visit(editor(), &path);
    let id = ed.window().buffer;

    let (ed, _) = step(
        ed,
        Event::UndoWritten {
            id,
            result: Err("denied".to_owned()),
        },
    );
    assert_eq!(
        ed.message(),
        format!("Wrote {}, undo not saved: denied", path.display())
    );
}

//! Commands that touch files: saving, reverting, opening, and quitting with unsaved changes.
//! They run against real files in a temporary directory.

use neonemacs::core::commands::{confirmed, request_open};
use neonemacs::core::config::Config;
use neonemacs::core::editor::{Confirm, Editor, Mode, Prefix, Window};
use neonemacs::core::effect::{Effect, Event, StatTarget};
use neonemacs::core::file::{Backing, FilePath};
use neonemacs::core::keymap::parse_keys;
use neonemacs::core::step;
use neonemacs::shell::file;
use neonemacs::shell::perform_all;
use neonemacs::test_support::{editor, keys, point, text, typ};
use std::fs;
use std::path::Path;

fn visit(ed: Editor, path: &Path) -> Editor {
    let fx = request_open(&ed, path, None, true, Vec::new());
    let ed = perform_all(ed, None, fx);
    ed.with_window(|w| Window { point: 0, ..w })
}

#[test]
fn oversized_file_is_refused_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    fs::write(&path, b"0123456789").unwrap();
    let ed = editor().set_config(Config {
        max_file_size: 4,
        ..*editor().config()
    });
    let count = ed.buffers().iter().count();
    let ed = visit(ed, &path);

    assert_eq!(ed.buffers().iter().count(), count);
    assert_eq!(ed.message(), "File too large: 10 bytes (max-file-size 4)");
    assert_eq!(fs::read(&path).unwrap(), b"0123456789");
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
    assert!(matches!(ed.mode(), Mode::Confirm(q) if matches!(q.kind, Confirm::SaveExternal(_))));
    assert_eq!(fs::read(&path).unwrap(), b"external change");

    ed = keys(ed, "n");
    assert_eq!(
        ed.mode(),
        &Mode::Normal {
            prefix: Prefix::None
        }
    );
    assert_eq!(fs::read(&path).unwrap(), b"external change");

    let _ = keys(ed, "C-x C-s y");
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
    assert!(matches!(dirty.mode(), Mode::Confirm(q) if matches!(q.kind, Confirm::QuitUnsaved)));
    assert!(!dirty.quit());
    dirty = keys(dirty, "n");
    assert!(!dirty.quit());
    dirty = keys(dirty, "C-x C-c y");
    assert!(dirty.quit());
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
        Mode::Confirm(q) if matches!(q.kind, Confirm::RevertModified(_))
    ));
    ed = keys(ed, "y");
    assert_eq!(text(&ed), "one\ntwo\nthree\n");

    let copy = dir.path().join("copy.txt");
    ed = keys(ed, "M-x");
    ed = typ(ed, "write-file");
    ed = keys(ed, "RET");
    ed = typ(ed, &copy.display().to_string());
    let _ = keys(ed, "RET");
    assert_eq!(fs::read(&copy).unwrap(), b"one\ntwo\nthree\n");
}

#[test]
fn write_file_over_existing_file_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    let other = dir.path().join("other.txt");
    fs::write(&path, b"abc").unwrap();
    fs::write(&other, b"precious").unwrap();
    let mut ed = visit(editor(), &path);

    ed = keys(ed, "M-x");
    ed = typ(ed, "write-file");
    ed = keys(ed, "RET");
    ed = typ(ed, &other.display().to_string());
    ed = keys(ed, "RET");
    assert!(matches!(
        ed.mode(),
        Mode::Confirm(q) if matches!(q.kind, Confirm::OverwriteFile { .. })
    ));
    assert_eq!(fs::read(&other).unwrap(), b"precious");

    ed = keys(ed, "n");
    assert_eq!(fs::read(&other).unwrap(), b"precious");
    assert_eq!(ed.buffer().name(), "f.txt");

    ed = keys(ed, "M-x");
    ed = typ(ed, "write-file");
    ed = keys(ed, "RET");
    ed = typ(ed, &other.display().to_string());
    ed = keys(ed, "RET y");
    assert_eq!(fs::read(&other).unwrap(), b"abc");
    assert_eq!(ed.buffer().name(), "other.txt");
}

#[test]
fn save_detects_file_created_after_opening_missing_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.txt");
    let ed = visit(editor(), &path);
    let mut ed = typ(ed, "mine");

    fs::write(&path, b"theirs").unwrap();
    ed = keys(ed, "C-x C-s");
    assert!(matches!(ed.mode(), Mode::Confirm(q) if matches!(q.kind, Confirm::SaveExternal(_))));
    assert_eq!(fs::read(&path).unwrap(), b"theirs");
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
            target: StatTarget::Save(id),
            path: path.clone()
        }]
    );
    assert_eq!(fs::read(&path).unwrap(), b"abc");
}

#[test]
fn written_records_disk_and_reports_wrote() {
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
            path: path.clone(),
            result: Ok(disk),
        },
    );
    let buf = ed.buffer_by_id(id);
    assert!(!buf.modified());
    assert_eq!(
        *buf.backing(),
        Backing::file(FilePath::resolve(Path::new(""), &path), disk)
    );
    assert_eq!(fx, [] as [Effect; 0]);
    assert_eq!(ed.message(), format!("Wrote {}", path.display()));
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
            path,
            result: Err("disk full".to_owned()),
        },
    );
    assert_eq!(fx, [] as [Effect; 0]);
    assert_eq!(ed.message(), "Save failed: disk full");
    assert!(ed.buffer_by_id(id).modified());
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
fn save_symlink_target_and_stat_disk_consistency() {
    let dir = tempfile::tempdir().unwrap();
    let real_file = dir.path().join("real.txt");
    fs::write(&real_file, b"original real data").unwrap();

    let link_file = dir.path().join("link.txt");
    #[cfg(unix)]
    let link_created = std::os::unix::fs::symlink(&real_file, &link_file).is_ok();
    #[cfg(windows)]
    let link_created = std::os::windows::fs::symlink_file(&real_file, &link_file).is_ok();

    if !link_created {
        // Skipping test on Windows when Developer Mode / Elevation is not enabled
        return;
    }

    // Save through link_file
    let state_after_save = file::save(&link_file, b"updated through symlink").unwrap();
    let stat_via_link = file::disk_state(&link_file);
    let stat_via_target = file::disk_state(&real_file);

    assert_eq!(
        state_after_save, stat_via_link,
        "DiskState returned by save must match disk_state(link)"
    );
    assert_eq!(
        stat_via_link, stat_via_target,
        "StatDisk on link must match target file metadata"
    );
    assert_eq!(fs::read(&real_file).unwrap(), b"updated through symlink");
}

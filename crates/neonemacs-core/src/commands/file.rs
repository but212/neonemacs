//! File operations: the save check flow, writing, and confirmation (docs/context.md §3.1, §4.2).
//! Reading and opening files is in `load`.

use crate::commands::load::{name_of, revert_now};
use crate::editor::{Buffer, BufferId, Confirm, Editor};
use crate::effect::{Effect, Effects, StatTarget, WalTarget};
use crate::file::{Backing, DiskState};
use crate::undo::Edit;
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// Saves the current buffer. A file-backed buffer asks the shell for its disk state first
/// (`StatDisk`); `disk_checked` then applies the external-change check (I-04).
pub(crate) fn save_buffer(ed: Editor, mut fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    match ed.buffer_by_id(id).file_path() {
        None => (ed.set_message("No file is visiting this buffer"), fx),
        Some(path) => {
            fx.push(Effect::StatDisk {
                target: StatTarget::Save(id),
                path: path.to_owned(),
            });
            (ed, fx)
        }
    }
}

pub(crate) fn save_buffers_kill_emacs(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let names: Vec<&str> = ed
        .buffers()
        .iter()
        .filter(|b| b.modified())
        .map(Buffer::name)
        .collect();
    if names.is_empty() {
        (ed.set_quit(true), fx)
    } else {
        let question = quit_question(&names);
        (ed.confirm(Confirm::QuitUnsaved, question), fx)
    }
}

/// Names the modified buffers a quit would discard: the first three, then `+N` for the rest (P7).
fn quit_question(names: &[&str]) -> String {
    const SHOWN: usize = 3;
    let mut list = names[..names.len().min(SHOWN)].join(", ");
    if names.len() > SHOWN {
        let _ = write!(list, " +{}", names.len() - SHOWN);
    }
    format!("Modified: {list}; quit anyway? (y or n)")
}

/// Applies the disk state the shell found for `path` (I-04).
///
/// - A save asks when the state differs from the one recorded at the read. A file created
///   after the buffer opened a missing path counts as a change.
/// - A `write-file` to another path asks when a file exists there.
///
/// A buffer without a file has no disk state to change, so its write is a no-op in `write_buffer`.
#[must_use]
pub fn disk_checked(
    ed: Editor,
    target: StatTarget,
    path: &str,
    state: DiskState,
    fx: Effects,
) -> (Editor, Effects) {
    let id = target.id();
    let file = ed.resolve_path(path);
    let own = ed.buffer_by_id(id).file_path() == Some(file.as_str());
    if matches!(target, StatTarget::SaveAs(_)) && !own {
        if matches!(state, DiskState::Known { .. }) {
            let message = format!("File {path} exists; overwrite? (y or n)");
            let ed = ed.confirm(
                Confirm::OverwriteFile {
                    id,
                    path: path.to_owned(),
                },
                message,
            );
            return (ed, fx);
        }
        let mut fx = fx;
        fx.push(Effect::WriteFile {
            id,
            path: path.to_owned(),
        });
        return (ed, fx);
    }
    if ed.buffer_by_id(id).backing().disk() == state {
        let fx = write_buffer(&ed, id, fx);
        (ed, fx)
    } else {
        let ed = ed.confirm(
            Confirm::SaveExternal(id),
            "File changed on disk since it was read; overwrite? (y or n)",
        );
        (ed, fx)
    }
}

/// Runs the action that a yes answer to `question` allows.
#[must_use]
pub fn confirmed(ed: Editor, question: Confirm, fx: Effects) -> (Editor, Effects) {
    match question {
        Confirm::SaveExternal(id) => {
            let fx = write_buffer(&ed, id, fx);
            (ed, fx)
        }
        Confirm::OverwriteFile { id, path } => {
            let mut fx = fx;
            fx.push(Effect::WriteFile { id, path });
            (ed, fx)
        }
        Confirm::QuitUnsaved => {
            let mut fx = fx;
            for buf in ed.buffers() {
                fx.extend(ed.remove_wal_effect(buf.id()));
            }
            (ed.set_quit(true), fx)
        }
        Confirm::KillModified(id) => super::basic::kill_buffer_now(ed, id, fx),
        Confirm::RevertModified(id) => revert_now(ed, id, fx),
    }
}

/// Asks the shell to write the buffer to its file. The result comes back as `Written` (I-04).
pub(crate) fn write_buffer(ed: &Editor, id: BufferId, mut fx: Effects) -> Effects {
    if let Some(path) = ed.buffer_by_id(id).file_path() {
        fx.push(Effect::WriteFile {
            id,
            path: path.to_owned(),
        });
    }
    fx
}

/// Records a finished write (I-05) and reports it in the echo area. A write to a new path moves
/// the buffer there and removes the WAL of the old path (I-25, I-36). A failed write changes nothing.
#[must_use]
pub fn written(
    mut ed: Editor,
    id: BufferId,
    path: &str,
    result: Result<DiskState, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    let message = match result {
        Ok(new_disk) => {
            let file = ed.resolve_path(path);
            if ed.buffer_by_id(id).file_path() != Some(file.as_str()) {
                fx.extend(ed.remove_wal_effect(id));
                let name = name_of(file.as_str());
                ed = ed.with_buffer(id, |b| {
                    b.set_name(name).set_backing(Backing::file(file, new_disk))
                });
            }
            let track = ed.tracks_wal_deltas();
            ed = ed.with_buffer(id, |b| {
                let track = track && b.eol().preserves_saved_doc(b.text());
                b.record_saved(new_disk).track_wal_baseline(track)
            });
            fx.push(Effect::RemoveWal {
                target: WalTarget::Sibling(path.to_owned()),
            });
            format!("Wrote {path}")
        }
        Err(e) => format!("Save failed: {e}"),
    };
    (ed.set_message(message), fx)
}

/// Writes the current buffer to a new file name. `written` makes that its file on success (docs/context.md §3.1).
pub(crate) fn write_file(ed: Editor, input: &str, mut fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let id = ed.window().buffer;
    let path = ed.resolve_path(input);
    // Another buffer visits this path, so writing here would give two buffers one WAL (I-37).
    if let Some(other) = ed
        .buffers()
        .find_by_path(path.as_str())
        .filter(|b| b.id() != id)
    {
        let message = format!("Buffer {} is visiting {}", other.name(), path.as_str());
        return (ed.set_message(message), fx);
    }
    fx.push(Effect::StatDisk {
        target: StatTarget::SaveAs(id),
        path: path.as_str().to_owned(),
    });
    (ed, fx)
}

/// Switches the buffer between LF and CRLF as an undoable edit (I-31). `apply` refuses a
/// read-only buffer and sets its message.
pub(crate) fn toggle_eol(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let from = ed.buffer().eol();
    let to = from.toggled();
    let (ed, applied) = ed.apply(Edit::SetEol { from, to });
    if applied.is_none() {
        return (ed, fx);
    }
    (ed.set_message(format!("Line endings: {}", to.label())), fx)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::Doc;
    use crate::eol::Eol;
    use crate::file::FilePath;
    use crate::test_support::{editor, file_buffer, known, saved_bytes};

    fn quit_message(names: &[&str]) -> String {
        let mut ed = editor();
        for name in names {
            let (next, id) = ed.add_buffer(name, Doc::default(), Backing::None);
            ed = next.with_buffer(id, Buffer::mark_modified);
        }
        // The initial `*scratch*` stays clean, so only the named buffers are listed.
        let (ed, _) = save_buffers_kill_emacs(ed, Vec::new());
        ed.message().to_owned()
    }

    #[test]
    fn quit_question_names_the_modified_buffers() {
        assert_eq!(
            quit_message(&["a.rs"]),
            "Modified: a.rs; quit anyway? (y or n)"
        );
        assert_eq!(
            quit_message(&["a", "b", "c"]),
            "Modified: a, b, c; quit anyway? (y or n)"
        );
        assert_eq!(
            quit_message(&["a", "b", "c", "d", "e"]),
            "Modified: a, b, c +2; quit anyway? (y or n)"
        );
    }

    #[test]
    fn quit_with_nothing_modified_asks_nothing() {
        let (ed, _) = save_buffers_kill_emacs(editor(), Vec::new());
        assert!(ed.quit());
    }

    #[test]
    fn toggle_eol_flips_mode_marks_wal_dirty_and_saves_bytes() {
        for (initial, expected_eol, expected_bytes) in [
            (Eol::Lf, Eol::Crlf, &b"a\r\nb\r\n"[..]),
            (Eol::Crlf, Eol::Lf, &b"a\nb\n"[..]),
        ] {
            let ed = file_buffer(editor(), b"a\nb\n", initial);
            let id = ed.window().buffer;
            let (ed, _) = toggle_eol(ed, Vec::new());
            assert_eq!(ed.buffer().eol(), expected_eol);
            assert!(ed.buffer().modified());
            assert!(ed.buffer().backing().is_wal_dirty());
            assert_eq!(saved_bytes(&ed, id), expected_bytes);
        }
    }

    #[test]
    fn toggle_twice_saves_original_bytes() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let (ed, _) = toggle_eol(ed, Vec::new());
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(saved_bytes(&ed, id), b"a\nb\n");
    }

    #[test]
    fn toggle_eol_is_undoable() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"a\nb\n"));
        let (ed, _) = ed.undo(Vec::new());
        assert_eq!(ed.buffer().eol(), Eol::Lf);
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"a\nb\n"));
    }

    #[test]
    fn read_only_buffer_refuses_toggle() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf)
            .with_current_buffer(|b| b.set_view_only(true));
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(ed.buffer().eol(), Eol::Lf);
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "Buffer is read-only");
    }

    #[test]
    fn write_file_to_visited_path_is_refused() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let (ed, _) = ed.add_buffer(
            "g.txt",
            Doc::from_bytes(b"x"),
            Backing::file(FilePath::resolve("", "g.txt"), DiskState::Unknown),
        );
        let before = ed.buffer().backing().clone();

        for path in ["g.txt", "sub/../g.txt"] {
            let (ed, fx) = write_file(ed.clone(), path, Vec::new());
            assert_eq!(fx, [] as [Effect; 0]);
            assert_eq!(ed.message(), "Buffer g.txt is visiting g.txt");
            assert_eq!(*ed.buffer().backing(), before);
        }
    }

    #[test]
    fn write_file_failure_keeps_backing_and_wal() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let before = ed.buffer().backing().clone();
        let (ed, fx) = write_file(ed, "missing/b.txt", Vec::new());
        let [Effect::StatDisk { target, path }] = fx.as_slice() else {
            panic!("expected StatDisk, got {fx:?}");
        };
        let (ed, fx) = disk_checked(ed, *target, path, DiskState::Unknown, Vec::new());
        // Only the write is requested. The old WAL goes only after the write succeeds (I-36).
        assert!(matches!(fx.as_slice(), [Effect::WriteFile { .. }]));

        let (ed, fx) = written(
            ed,
            id,
            "missing/b.txt",
            Err("denied".to_owned()),
            Vec::new(),
        );
        assert_eq!(fx, [] as [Effect; 0]);
        assert_eq!(ed.message(), "Save failed: denied");
        assert_eq!(*ed.buffer().backing(), before);
        assert_eq!(ed.buffer().name(), "f.txt");
    }

    #[test]
    fn write_file_success_moves_backing_then_removes_old_wal() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let (stat_ed, fx) = write_file(ed.clone(), "b.txt", Vec::new());
        let [Effect::StatDisk { target, path }] = fx.as_slice() else {
            panic!("expected StatDisk, got {fx:?}");
        };
        let (_, fx) = disk_checked(stat_ed, *target, path, DiskState::Unknown, Vec::new());
        assert!(matches!(fx.as_slice(), [Effect::WriteFile { path, .. }] if path == "b.txt"));

        let disk = DiskState::Known {
            mtime: core::time::Duration::ZERO,
            size: 3,
        };
        let (ed, fx) = written(ed, id, "b.txt", Ok(disk), Vec::new());
        assert_eq!(
            fx,
            vec![
                Effect::RemoveWal {
                    target: WalTarget::Sibling("f.txt".to_owned())
                },
                Effect::RemoveWal {
                    target: WalTarget::Sibling("b.txt".to_owned())
                },
            ]
        );
        assert_eq!(
            *ed.buffer().backing(),
            Backing::file(FilePath::resolve("", "b.txt"), disk)
        );
        assert_eq!(ed.buffer().name(), "b.txt");
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "Wrote b.txt");
    }

    /// The `StatDisk` that `write_file` requests for `input`.
    fn stat_for_write(ed: Editor, input: &str) -> (Editor, StatTarget, String) {
        let (ed, fx) = write_file(ed, input, Vec::new());
        let [Effect::StatDisk { target, path }] = fx.as_slice() else {
            panic!("expected StatDisk, got {fx:?}");
        };
        (ed, *target, path.clone())
    }

    #[test]
    fn write_file_to_existing_unvisited_file_asks() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let (ed, target, path) = stat_for_write(ed, "other.txt");
        assert_eq!(target, StatTarget::SaveAs(id));

        let (ed, fx) = disk_checked(ed, target, &path, known(9), Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        assert!(matches!(
            ed.mode(),
            crate::editor::Mode::Confirm { kind, .. } if *kind == Confirm::OverwriteFile {
                id,
                path: "other.txt".to_owned()
            }
        ));
        assert_eq!(ed.message(), "File other.txt exists; overwrite? (y or n)");
    }

    #[test]
    fn write_file_to_missing_path_writes() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let (ed, target, path) = stat_for_write(ed, "other.txt");
        let (_, fx) = disk_checked(ed, target, &path, DiskState::Unknown, Vec::new());
        assert!(matches!(fx.as_slice(), [Effect::WriteFile { path, .. }] if path == "other.txt"));
    }

    #[test]
    fn write_file_to_own_path_uses_save_check() {
        // The buffer's file is f.txt with recorded state Known(3).
        let ed = file_buffer(editor(), b"abc", Eol::Lf).with_current_buffer(|b| {
            b.set_backing(Backing::file(FilePath::resolve("", "f.txt"), known(3)))
        });
        let (ed, target, path) = stat_for_write(ed, "f.txt");

        // Unchanged on disk: written without asking.
        let (same, fx) = disk_checked(ed.clone(), target, &path, known(3), Vec::new());
        assert!(matches!(fx.as_slice(), [Effect::WriteFile { .. }]));
        assert!(matches!(same.mode(), crate::editor::Mode::Normal { .. }));

        // Changed on disk: the external-change question, not the overwrite one.
        let (changed, fx) = disk_checked(ed, target, &path, known(4), Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        assert!(matches!(
            changed.mode(),
            crate::editor::Mode::Confirm { kind: Confirm::SaveExternal(_), .. }
        ));
    }

    #[test]
    fn overwrite_confirmation_yes_writes_to_the_asked_path() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let question = Confirm::OverwriteFile {
            id,
            path: "other.txt".to_owned(),
        };
        let (ed, fx) = confirmed(ed, question, Vec::new());
        assert_eq!(
            fx,
            vec![Effect::WriteFile {
                id,
                path: "other.txt".to_owned(),
            }]
        );
        // Only `written` moves the buffer (I-36).
        assert_eq!(ed.buffer().name(), "f.txt");
    }

    #[test]
    fn save_of_missing_opened_file_asks_when_created_externally_and_writes_when_missing() {
        // Opened as missing (Unknown); asks if the file exists by save time, otherwise writes.
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let (asked, fx) = disk_checked(
            ed.clone(),
            StatTarget::Save(id),
            "f.txt",
            known(5),
            Vec::new(),
        );
        assert_eq!(fx, [] as [Effect; 0]);
        assert!(matches!(
            asked.mode(),
            crate::editor::Mode::Confirm { kind: Confirm::SaveExternal(_), .. }
        ));

        let (_, fx) = disk_checked(
            ed,
            StatTarget::Save(id),
            "f.txt",
            DiskState::Unknown,
            Vec::new(),
        );
        assert!(matches!(fx.as_slice(), [Effect::WriteFile { .. }]));
    }
}

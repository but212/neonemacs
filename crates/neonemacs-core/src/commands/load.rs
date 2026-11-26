//! Opening and reading files: asking the shell to read and applying what it read (new buffer or
//! revert) (docs/context.md §3.2, §4.2).

use crate::commands::basic::switch_to;
use crate::editor::{Buffer, BufferId, Confirm, Editor};
use crate::effect::{Effect, Effects, ReadTarget, RecoveredFile};
use crate::file::{RawFile, loaded_from};
use crate::goto_line_offset;
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;

/// Asks the shell to read `path` for a new buffer. The result comes back as `FileRead`.
/// `line` moves point to that line once the file is loaded (`+n`). `show` selects the buffer.
#[must_use]
pub fn request_open(
    ed: &Editor,
    path: &str,
    line: Option<usize>,
    show: bool,
    mut fx: Effects,
) -> Effects {
    fx.push(Effect::ReadFile {
        target: ReadTarget::Open { line, show },
        path: ed.resolve_path(path).as_str().to_owned(),
        max_size: ed.config().max_file_size,
    });
    fx
}

/// The buffer name for `path`: its last component, or the whole path when it has none.
pub(crate) fn name_of(path: &str) -> String {
    crate::file::file_name(path).unwrap_or(path).to_owned()
}

/// Opens the file `input` in a new buffer, or shows it if it is already open.
pub(crate) fn find_file(ed: Editor, input: &str, fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let path = ed.resolve_path(input);
    let existing = ed.buffers().find_by_path(path.as_str()).map(Buffer::id);
    if let Some(id) = existing {
        (switch_to(ed, id), fx)
    } else {
        let fx = request_open(&ed, path.as_str(), None, true, fx);
        (ed, fx)
    }
}

pub(crate) fn revert_buffer(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    if ed.buffer().modified() {
        let ed = ed.confirm(
            Confirm::RevertModified(id),
            "Discard changes and revert? (y or n)",
        );
        (ed, fx)
    } else {
        revert_now(ed, id, fx)
    }
}

/// Asks the shell to read the file again. The result comes back as `FileRead` (C-12).
pub(crate) fn revert_now(ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    let Some(path) = ed.buffer_by_id(id).file_path().map(str::to_owned) else {
        return (ed.set_message("Buffer has no file to revert to"), fx);
    };
    fx.push(Effect::ReadFile {
        target: ReadTarget::Revert(id),
        path,
        max_size: ed.config().max_file_size,
    });
    (ed, fx)
}

/// Applies the result of a file read: a new buffer for `Open`, a reload for `Revert` (C-11, C-12).
/// A file over `max-file-size` is refused: `Open` makes no buffer, `Revert` keeps the buffer
/// unchanged (I-04).
pub fn file_read(
    ed: Editor,
    target: ReadTarget,
    path: String,
    result: Result<Option<RawFile>, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    let max_size = ed.config().max_file_size;
    let default_eol = ed.config().default_eol;
    let file = ed.resolve_path(&path);
    match (target, result) {
        (ReadTarget::Open { line, show }, Ok(read)) => {
            // A visited path keeps its one buffer (I-37), so `line` does not move it.
            if let Some(id) = ed.buffers().find_by_path(file.as_str()).map(Buffer::id) {
                let ed = if show { switch_to(ed, id) } else { ed };
                return (ed, fx);
            }
            match loaded_from(&file, read, max_size, default_eol) {
                Ok(loaded) => {
                    let (mut ed, id) = ed.add_buffer(&name_of(&path), loaded.text, loaded.backing);
                    ed = ed.with_buffer(id, |b| b.set_eol(loaded.eol));
                    if let Some(n) = line {
                        let pos = goto_line_offset(ed.buffer_by_id(id).text(), n);
                        ed = ed.with_buffer(id, |b| b.set_saved_point(pos));
                    }
                    if show {
                        ed = switch_to(ed, id);
                    }
                    fx.push(Effect::ProbeRecovery {
                        id,
                        path: file.as_str().to_owned(),
                    });
                    (ed, fx)
                }
                Err(e) => (ed.set_message(e), fx),
            }
        }
        (ReadTarget::Open { .. }, Err(e)) => {
            let ed = ed.set_message(format!("Cannot open {path}: {e}"));
            (ed, fx)
        }
        // A deleted file is not an empty one: keep the buffer.
        (ReadTarget::Revert(_), Ok(None)) => {
            let ed = ed.set_message(format!("Revert failed: {path} does not exist"));
            (ed, fx)
        }
        (ReadTarget::Revert(id), Ok(read)) => match loaded_from(&file, read, max_size, default_eol)
        {
            Ok(loaded) => {
                let ed = ed
                    .with_buffer(id, |b| {
                        b.replace_from_disk(loaded.text, loaded.backing, loaded.eol)
                    })
                    .snap_windows_for_buffer(id)
                    .set_message("Reverted");
                (ed, fx)
            }
            Err(e) => (ed.set_message(e), fx),
        },
        (ReadTarget::Revert(_), Err(e)) => {
            let ed = ed.set_message(format!("Revert failed: {e}"));
            (ed, fx)
        }
    }
}

/// Prompts the user to restore an orphan `.rec` backup when a newly opened buffer has one.
#[must_use]
pub fn recovery_probed(ed: Editor, id: BufferId, rec: Option<RecoveredFile>) -> Editor {
    let Some(rec) = rec else {
        return ed;
    };
    if !ed.buffers().iter().any(|b| b.id() == id) {
        return ed;
    }
    let name = ed.buffer_by_id(id).name().to_owned();
    ed.enqueue_recovery(
        id,
        rec,
        format!("Recover {name} from crash backup? (y or n)"),
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::Doc;
    use crate::config::Config;
    use crate::eol::Eol;
    use crate::file::RawFile;
    use crate::file::{Backing, DiskState, FilePath};
    use crate::test_support::{editor, file_buffer, saved_bytes};

    /// An editor whose `max-file-size` is 4 bytes.
    fn small_limit() -> Editor {
        Editor::new(Config {
            max_file_size: 4,
            ..Config::default()
        })
    }

    #[test]
    fn opened_files_normalize_eol_and_save_expected_bytes() {
        type Case<'a> = (&'a [u8], Eol, &'a [u8], &'a [u8]);
        let cases: &[Case<'_>] = &[
            (
                b"a\r\nb\r\nc\n",
                Eol::Crlf,
                b"a\nb\nc\n",
                b"a\r\nb\r\nc\r\n",
            ),
            (b"a\r\nb\n", Eol::Lf, b"a\nb\n", b"a\nb\n"),
            (b"a\r\nb\r\n", Eol::Crlf, b"a\nb\n", b"a\r\nb\r\n"),
        ];
        for &(raw, expected_eol, expected_doc, expected_saved) in cases {
            let read = Some(RawFile::from_bytes(
                raw.len() as u64,
                DiskState::Unknown,
                raw,
            ));
            let target = ReadTarget::Open {
                line: None,
                show: false,
            };
            let (ed, _) = file_read(editor(), target, "f.txt".to_owned(), Ok(read), Vec::new());
            let id = ed.buffers().iter().last().unwrap().id();
            assert_eq!(ed.buffer_by_id(id).eol(), expected_eol);
            assert_eq!(
                *ed.buffer_by_id(id).text(),
                crate::Doc::from_bytes(expected_doc)
            );
            assert!(!ed.buffer_by_id(id).modified());
            assert_eq!(saved_bytes(&ed, id), expected_saved);
        }
    }

    #[test]
    fn revert_picks_up_line_endings_from_disk() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let read = Some(RawFile::from_bytes(6, DiskState::Unknown, b"a\r\nb\r\n"));
        let (ed, _) = file_read(
            ed,
            ReadTarget::Revert(id),
            "f.txt".to_owned(),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\n");
    }

    #[test]
    fn oversized_open_creates_no_buffer() {
        let ed = small_limit();
        let count = ed.buffers().iter().count();
        let read = Some(RawFile::from_bytes(10, DiskState::Unknown, &[]));
        let target = ReadTarget::Open {
            line: None,
            show: true,
        };
        let (ed, fx) = file_read(ed, target, "big.txt".to_owned(), Ok(read), Vec::new());
        assert_eq!(ed.buffers().iter().count(), count);
        assert_eq!(ed.message(), "File too large: 10 bytes (max-file-size 4)");
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn oversized_revert_keeps_buffer_and_wal() {
        let ed =
            file_buffer(small_limit(), b"ab", Eol::Lf).with_current_buffer(Buffer::mark_modified);
        let id = ed.window().buffer;
        let read = Some(RawFile::from_bytes(10, DiskState::Unknown, &[]));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Revert(id),
            "f.txt".to_owned(),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(*ed.buffer().text(), crate::Doc::from_bytes(b"ab"));
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "File too large: 10 bytes (max-file-size 4)");
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn successful_revert_reloads_text() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let read = Some(RawFile::from_bytes(2, DiskState::Unknown, b"x\n"));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Revert(id),
            "f.txt".to_owned(),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(*ed.buffer().text(), crate::Doc::from_bytes(b"x\n"));
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn open_of_visited_or_parent_spelled_path_reuses_buffer() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let count = ed.buffers().iter().count();
        let read = Some(RawFile::from_bytes(3, DiskState::Unknown, b"abc"));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Open {
                line: None,
                show: true,
            },
            "./f.txt".to_owned(),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(ed.buffers().iter().count(), count);
        assert_eq!(fx, [] as [Effect; 0]);

        let ed = editor().with_cwd("/w".to_owned());
        let ed = ed.with_current_buffer(|b| {
            b.with_text(Doc::from_bytes(b"abc"))
                .set_name("a.txt")
                .set_backing(Backing::file(
                    FilePath::resolve("/w", "a.txt"),
                    DiskState::Unknown,
                ))
                .mark_clean()
        });
        let count = ed.buffers().iter().count();
        let read = Some(RawFile::from_bytes(3, DiskState::Unknown, b"abc"));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Open {
                line: None,
                show: true,
            },
            "../w/a.txt".to_owned(),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(ed.buffers().iter().count(), count);
        assert_eq!(fx, [] as [Effect; 0]);

        let (ed, fx) = find_file(ed, "../w/a.txt", Vec::new());
        assert_eq!(ed.buffers().iter().count(), count);
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn revert_of_deleted_file_keeps_buffer_and_wal() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf).with_current_buffer(Buffer::mark_modified);
        let id = ed.window().buffer;
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Revert(id),
            "f.txt".to_owned(),
            Ok(None),
            Vec::new(),
        );
        assert_eq!(*ed.buffer().text(), crate::Doc::from_bytes(b"abc"));
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "Revert failed: f.txt does not exist");
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn open_emits_probe_recovery_and_confirm_handles_yes_no_cancel() {
        use crate::effect::{Event, RecoveredFile};
        use crate::keymap::{Code, Key};
        use crate::step;
        use core::time::Duration;

        let disk = DiskState::Known {
            mtime: Duration::from_secs(10),
            size: 4,
        };
        let read = Some(RawFile::from_bytes(4, disk, b"disk"));
        let (ed, fx) = file_read(
            editor(),
            ReadTarget::Open {
                line: None,
                show: true,
            },
            "/w/note.txt".to_owned(),
            Ok(read),
            Vec::new(),
        );
        let id = ed.buffer().id();
        assert_eq!(
            fx,
            [Effect::ProbeRecovery {
                id,
                path: "/w/note.txt".to_owned(),
            }]
        );

        let rec = RecoveredFile {
            doc: Doc::from_bytes(b"recovered"),
            eol: Eol::Crlf,
            rec_paths: alloc::vec!["/state/1-1-abc.rec".to_owned()],
        };
        let (ed, fx) = step(ed, Event::RecoveryProbed { id, rec: Some(rec) });
        assert!(fx.is_empty());
        assert!(matches!(
            ed.mode(),
            crate::editor::Mode::Confirm {
                kind: Confirm::RecoverFile { .. },
                ..
            }
        ));

        // C-g cancels without deleting .rec and keeps disk content.
        let (ed_cancel, fx_cancel) = step(ed.clone(), Event::Key(Key::ctrl('g')));
        assert_eq!(*ed_cancel.buffer().text(), Doc::from_bytes(b"disk"));
        assert!(!ed_cancel.buffer().modified());
        assert!(fx_cancel.is_empty());

        // 'n' declines and deletes .rec while keeping disk content.
        let (ed_no, fx_no) = step(ed.clone(), Event::Key(Key::plain(Code::Char('n'))));
        assert_eq!(*ed_no.buffer().text(), Doc::from_bytes(b"disk"));
        assert!(!ed_no.buffer().modified());
        assert_eq!(
            fx_no,
            [Effect::DeleteRecovery {
                rec_paths: alloc::vec!["/state/1-1-abc.rec".to_owned()],
            }]
        );

        // 'y' restores doc & eol, marks modified, preserves disk Backing, and deletes .rec.
        let (ed_yes, fx_yes) = step(ed, Event::Key(Key::plain(Code::Char('y'))));
        assert_eq!(*ed_yes.buffer().text(), Doc::from_bytes(b"recovered"));
        assert_eq!(ed_yes.buffer().eol(), Eol::Crlf);
        assert!(ed_yes.buffer().modified());
        assert_eq!(ed_yes.buffer().backing().disk(), disk);
        assert_eq!(
            fx_yes,
            [Effect::DeleteRecovery {
                rec_paths: alloc::vec!["/state/1-1-abc.rec".to_owned()],
            }]
        );
    }
}

//! Opening and reading files: asking the shell to read, applying what it read (new buffer or
//! revert), and applying what it found in a WAL (docs/context.md §3.2, §4.2).

use crate::commands::basic::switch_to;
use crate::editor::{Buffer, BufferId, Confirm, Editor};
use crate::effect::{Effect, Effects, ReadTarget, WalProbe, WalTarget};
use crate::file::{Read, loaded_from};
use std::path::{Path, PathBuf};

/// Asks the shell to read `path` for a new buffer. The result comes back as `FileRead`.
/// `line` moves point to that line once the file is loaded (`+n`). `show` selects the buffer.
#[must_use]
pub fn request_open(
    ed: &Editor,
    path: &Path,
    line: Option<usize>,
    show: bool,
    mut fx: Effects,
) -> Effects {
    fx.push(Effect::ReadFile {
        target: ReadTarget::Open { line, show },
        path: ed.resolve_path(path).as_path().to_path_buf(),
        max_size: ed.config().max_file_size,
    });
    fx
}

/// The buffer name for `path`: its last component, or the whole path when it has none.
pub(crate) fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Opens the file `input` in a new buffer, or shows it if it is already open.
pub(crate) fn find_file(ed: Editor, input: &str, fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let path = ed.resolve_path(Path::new(input));
    let existing = ed.buffers().find_by_path(path.as_path()).map(Buffer::id);
    if let Some(id) = existing {
        (switch_to(ed, id), fx)
    } else {
        let fx = request_open(&ed, path.as_path(), None, true, fx);
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
    let Some(path) = ed.buffer_by_id(id).file_path().map(Path::to_path_buf) else {
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
/// and its WAL (I-04).
pub fn file_read(
    ed: Editor,
    target: ReadTarget,
    path: PathBuf,
    result: Result<Read, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    let max_size = ed.config().max_file_size;
    let default_eol = ed.config().default_eol;
    let file = ed.resolve_path(&path);
    match (target, result) {
        (ReadTarget::Open { line, show }, Ok(read)) => {
            // A visited path keeps its one buffer (I-37), so `line` does not move it.
            if let Some(id) = ed.buffers().find_by_path(file.as_path()).map(Buffer::id) {
                let ed = if show { switch_to(ed, id) } else { ed };
                return (ed, fx);
            }
            match loaded_from(&file, read, max_size, default_eol) {
                Ok(loaded) => {
                    let (mut ed, id) = ed.add_buffer(&name_of(&path), loaded.text, loaded.backing);
                    ed = ed.with_buffer(id, |b| b.set_eol(loaded.eol));
                    if let Some(n) = line {
                        let text = ed.buffer_by_id(id).text();
                        let line_idx = n.saturating_sub(1).min(text.total_lines() - 1);
                        let pos = text.line_to_offset(line_idx).expect("line < total_lines");
                        ed = ed.with_buffer(id, |b| b.set_saved_point(pos));
                    }
                    if show {
                        ed = switch_to(ed, id);
                    }
                    // The only place a WAL is looked for, so every way to open a file recovers (I-45).
                    fx.push(Effect::ProbeWal {
                        path: file.as_path().to_path_buf(),
                        disk: ed.buffer_by_id(id).backing().disk(),
                        line,
                    });
                    (ed, fx)
                }
                Err(e) => (ed.set_message(e.message()), fx),
            }
        }
        (ReadTarget::Open { .. }, Err(e)) => {
            let ed = ed.set_message(format!("Cannot open {}: {e}", path.display()));
            (ed, fx)
        }
        // A deleted file is not an empty one: keep the buffer and its WAL.
        (ReadTarget::Revert(_), Ok(Read::Missing)) => {
            let ed = ed.set_message(format!("Revert failed: {} does not exist", path.display()));
            (ed, fx)
        }
        (ReadTarget::Revert(id), Ok(read)) => match loaded_from(&file, read, max_size, default_eol)
        {
            Ok(loaded) => {
                let track = ed.tracks_wal_deltas();
                let ed = ed
                    .with_buffer(id, |b| {
                        b.replace_from_disk(loaded.text, loaded.backing, loaded.eol)
                            .track_wal_baseline(track)
                    })
                    .snap_windows_for_buffer(id)
                    .set_message("Reverted");
                // The WAL goes only after the reload succeeds, so a refused revert keeps
                // the unsaved edits recoverable.
                fx.push(Effect::RemoveWal {
                    target: WalTarget::Sibling(path),
                });
                (ed, fx)
            }
            Err(e) => (ed.set_message(e.message()), fx),
        },
        (ReadTarget::Revert(_), Err(e)) => {
            let ed = ed.set_message(format!("Revert failed: {e}"));
            (ed, fx)
        }
    }
}

/// Applies what the shell found when it looked for the WAL of the file at `path` (I-44, I-45).
#[must_use]
pub fn wal_probed(
    ed: Editor,
    path: &Path,
    line: Option<usize>,
    probe: WalProbe,
    fx: Effects,
) -> (Editor, Effects) {
    let name = name_of(path);
    let ed = match probe {
        WalProbe::Absent => ed,
        WalProbe::Valid(recovered) => ed
            .restore_buffer_content(path, recovered.doc, recovered.eol, line)
            .set_message(format!("[Recovered {name} from WAL]")),
        WalProbe::ValidDelta(delta) => ed
            .restore_buffer_deltas(path, delta, line)
            .set_message(format!("[Recovered {name} from WAL]")),
        WalProbe::Stale(Ok(kept)) => {
            ed.set_message(format!("Stale WAL kept as {}", name_of(&kept)))
        }
        WalProbe::Stale(Err(e)) => ed.set_message(format!("Stale WAL for {name} not kept: {e}")),
    };
    (ed, fx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::effect::Recovered;
    use crate::eol::Eol;
    use crate::file::{Backing, DiskState, FilePath};
    use crate::file::{RawFile, Read};
    use crate::test_support::{editor, file_buffer, known, saved_bytes};
    use crate::weave_tree::Doc;

    /// An editor whose `max-file-size` is 4 bytes.
    fn small_limit() -> Editor {
        Editor::new(Config {
            max_file_size: 4,
            ..Config::default()
        })
    }

    #[test]
    fn opened_mixed_file_normalizes_and_saves_majority_crlf() {
        let read = Read::Found(RawFile::from_bytes(7, DiskState::Unknown, b"a\r\nb\r\nc\n"));
        let target = ReadTarget::Open {
            line: None,
            show: false,
        };
        let (ed, _) = file_read(
            editor(),
            target,
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        let id = ed.buffers().iter().last().unwrap().id();
        assert_eq!(ed.buffer_by_id(id).eol(), Eol::Crlf);
        assert_eq!(
            *ed.buffer_by_id(id).text(),
            crate::weave_tree::Doc::from_bytes(b"a\nb\nc\n")
        );
        assert!(!ed.buffer_by_id(id).modified());
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\nc\r\n");
    }

    #[test]
    fn opened_mixed_file_normalizes_and_saves_majority_lf_or_tie() {
        let read = Read::Found(RawFile::from_bytes(5, DiskState::Unknown, b"a\r\nb\n"));
        let target = ReadTarget::Open {
            line: None,
            show: false,
        };
        let (ed, _) = file_read(
            editor(),
            target,
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        let id = ed.buffers().iter().last().unwrap().id();
        assert_eq!(ed.buffer_by_id(id).eol(), Eol::Lf);
        assert_eq!(
            *ed.buffer_by_id(id).text(),
            crate::weave_tree::Doc::from_bytes(b"a\nb\n")
        );
        assert!(!ed.buffer_by_id(id).modified());
        assert_eq!(saved_bytes(&ed, id), b"a\nb\n");
    }

    #[test]
    fn opened_crlf_file_saves_crlf_bytes() {
        let read = Read::Found(RawFile::from_bytes(6, DiskState::Unknown, b"a\r\nb\r\n"));
        let target = ReadTarget::Open {
            line: None,
            show: false,
        };
        let (ed, _) = file_read(
            editor(),
            target,
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        let id = ed.buffers().iter().last().unwrap().id();
        assert_eq!(ed.buffer_by_id(id).eol(), Eol::Crlf);
        assert_eq!(
            *ed.buffer_by_id(id).text(),
            crate::weave_tree::Doc::from_bytes(b"a\nb\n")
        );
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\n");
    }

    #[test]
    fn revert_picks_up_line_endings_from_disk() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let read = Read::Found(RawFile::from_bytes(6, DiskState::Unknown, b"a\r\nb\r\n"));
        let (ed, _) = file_read(
            ed,
            ReadTarget::Revert(id),
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\n");
    }

    fn found(bytes: &[u8], disk: DiskState) -> Read {
        Read::Found(RawFile::from_bytes(bytes.len() as u64, disk, bytes))
    }

    #[test]
    fn open_of_a_new_buffer_probes_its_wal() {
        // I-45: the one place a WAL is looked for.
        let disk = known(3);
        let target = ReadTarget::Open {
            line: Some(2),
            show: false,
        };
        let (_, fx) = file_read(
            editor(),
            target,
            PathBuf::from("f.txt"),
            Ok(found(b"abc", disk)),
            Vec::new(),
        );
        assert_eq!(
            fx,
            vec![Effect::ProbeWal {
                path: PathBuf::from("f.txt"),
                disk,
                line: Some(2),
            }]
        );
    }

    fn probed(probe: WalProbe) -> Editor {
        let ed = file_buffer(editor(), b"disk", Eol::Lf);
        let (ed, fx) = wal_probed(ed, &PathBuf::from("f.txt"), None, probe, Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        ed
    }

    #[test]
    fn no_wal_changes_nothing() {
        let ed = probed(WalProbe::Absent);
        assert_eq!(
            *ed.buffer().text(),
            crate::weave_tree::Doc::from_bytes(b"disk")
        );
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "");
    }

    #[test]
    fn valid_wal_restores_text_and_eol_and_says_so() {
        let ed = probed(WalProbe::Valid(Recovered {
            eol: Eol::Crlf,
            doc: Doc::from_bytes(b"draft"),
        }));
        assert_eq!(
            *ed.buffer().text(),
            crate::weave_tree::Doc::from_bytes(b"draft")
        );
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "[Recovered f.txt from WAL]");
    }

    #[test]
    fn replayed_delta_wal_restores_text_and_eol() {
        use crate::effect::RecoveredDelta;
        use crate::weave_tree::Replace;
        let delta = RecoveredDelta::replay(
            Doc::from_bytes(b"disk"),
            Eol::Crlf,
            vec![Replace {
                offset: 0,
                delete_len: 0,
                insert: Doc::from_bytes(b"X"),
            }],
        )
        .unwrap();
        let ed = probed(WalProbe::ValidDelta(delta));
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"Xdisk"));
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert!(ed.buffer().modified());
        assert!(!ed.buffer().is_wal_dirty());
        assert_eq!(ed.message(), "[Recovered f.txt from WAL]");
    }

    #[test]
    fn unusable_wal_is_reported_and_leaves_the_text() {
        let ed = probed(WalProbe::Stale(Ok(PathBuf::from("dir/.f.txt.wal.stale"))));
        assert_eq!(
            *ed.buffer().text(),
            crate::weave_tree::Doc::from_bytes(b"disk")
        );
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "Stale WAL kept as .f.txt.wal.stale");

        let ed = probed(WalProbe::Stale(Err("denied".to_owned())));
        assert_eq!(ed.message(), "Stale WAL for f.txt not kept: denied");
    }

    #[test]
    fn oversized_open_creates_no_buffer() {
        let ed = small_limit();
        let count = ed.buffers().iter().count();
        let read = Read::Found(RawFile::from_bytes(10, DiskState::Unknown, &[]));
        let target = ReadTarget::Open {
            line: None,
            show: true,
        };
        let (ed, fx) = file_read(ed, target, PathBuf::from("big.txt"), Ok(read), Vec::new());
        assert_eq!(ed.buffers().iter().count(), count);
        assert_eq!(ed.message(), "File too large: 10 bytes (max-file-size 4)");
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn oversized_revert_keeps_buffer_and_wal() {
        let ed =
            file_buffer(small_limit(), b"ab", Eol::Lf).with_current_buffer(Buffer::mark_modified);
        let id = ed.window().buffer;
        let read = Read::Found(RawFile::from_bytes(10, DiskState::Unknown, &[]));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Revert(id),
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(
            *ed.buffer().text(),
            crate::weave_tree::Doc::from_bytes(b"ab")
        );
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "File too large: 10 bytes (max-file-size 4)");
        // Nothing is removed: the unsaved edits stay recoverable from the WAL.
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn successful_revert_removes_wal_after_reload() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let read = Read::Found(RawFile::from_bytes(2, DiskState::Unknown, b"x\n"));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Revert(id),
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(
            *ed.buffer().text(),
            crate::weave_tree::Doc::from_bytes(b"x\n")
        );
        assert!(
            matches!(fx.as_slice(), [Effect::RemoveWal { target }] if target.path() == Path::new("f.txt"))
        );
    }

    #[test]
    fn open_of_visited_path_reuses_buffer() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let count = ed.buffers().iter().count();
        let read = Read::Found(RawFile::from_bytes(3, DiskState::Unknown, b"abc"));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Open {
                line: None,
                show: true,
            },
            PathBuf::from("./f.txt"),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(ed.buffers().iter().count(), count);
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn open_with_parent_spelling_reuses_buffer() {
        let ed = editor().with_cwd(PathBuf::from("/w"));
        let ed = ed.with_current_buffer(|b| {
            b.with_text(Doc::from_bytes(b"abc"))
                .set_name("a.txt")
                .set_backing(Backing::file(
                    FilePath::resolve(Path::new("/w"), Path::new("a.txt")),
                    DiskState::Unknown,
                ))
                .mark_clean()
        });
        let count = ed.buffers().iter().count();
        let read = Read::Found(RawFile::from_bytes(3, DiskState::Unknown, b"abc"));
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Open {
                line: None,
                show: true,
            },
            PathBuf::from("../w/a.txt"),
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
            PathBuf::from("f.txt"),
            Ok(Read::Missing),
            Vec::new(),
        );
        assert_eq!(
            *ed.buffer().text(),
            crate::weave_tree::Doc::from_bytes(b"abc")
        );
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "Revert failed: f.txt does not exist");
        assert_eq!(fx, [] as [Effect; 0]);
    }
}

//! File operations: save check flow, reading, reverting, and confirmation (§8, ADR 0002).

use crate::core::buffer::BufferPool;
use crate::core::commands::buffer_window::switch_to;
use crate::core::editor::{Buffer, BufferId, Confirm, Editor};
use crate::core::effect::{self, Effect, Effects, ReadTarget};
use crate::core::file::{Backing, DiskState, loaded_from};
use crate::core::undo::Edit;
use std::path::PathBuf;

/// Asks the shell to read `path` for a new buffer. The result comes back as `FileRead`.
/// `line` moves point to that line once the file is loaded (`+n`). `show` selects the buffer.
pub fn request_open(
    ed: &Editor,
    path: PathBuf,
    line: Option<usize>,
    show: bool,
    mut fx: Effects,
) -> Effects {
    fx.push(Effect::ReadFile {
        target: ReadTarget::Open { line, show },
        path,
        max_size: ed.config().max_file_size,
    });
    fx
}

// --- Layer 1: Pure domain primitives ---

/// Returns true if any buffer in the pool has unsaved modifications.
pub fn has_modified_buffers(pool: &BufferPool) -> bool {
    pool.iter().any(Buffer::modified)
}

/// Save check result for a backing file.
#[derive(Debug, PartialEq, Eq)]
pub enum SavePrecondition<'a> {
    NoFile,
    Ready(&'a PathBuf),
}

/// Checks whether a backing has a file to save to.
pub fn check_save_precondition(backing: &Backing) -> SavePrecondition<'_> {
    match backing {
        Backing::File { path, .. } => SavePrecondition::Ready(path),
        Backing::None => SavePrecondition::NoFile,
    }
}

/// Returns true if the external disk state changed since read (I-04).
pub fn is_external_change(backing_disk: DiskState, disk_now: DiskState) -> bool {
    matches!(backing_disk, DiskState::Known { .. }) && backing_disk != disk_now
}

// --- Layer 2: Built-in command adapters ---

pub(crate) fn save_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    save_buffer(ed, id, fx)
}

pub(crate) fn save_buffers_kill_emacs(ed: Editor, fx: Effects) -> (Editor, Effects) {
    if has_modified_buffers(ed.buffers()) {
        let ed = ed.confirm(
            Confirm::QuitUnsaved,
            "Modified buffers exist; quit anyway? (y or n)",
        );
        (ed, fx)
    } else {
        (ed.set_quit(true), fx)
    }
}

fn file_state(ed: &Editor, id: BufferId) -> Option<(PathBuf, DiskState)> {
    match ed.buffer_by_id(id).backing() {
        Backing::File { path, disk, .. } => Some((path.clone(), *disk)),
        Backing::None => None,
    }
}

/// Saves `id`. A file-backed buffer asks the shell for its disk state first (`StatDisk`);
/// `disk_checked` then applies the external-change check (I-04).
pub fn save_buffer(ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    match check_save_precondition(ed.buffer_by_id(id).backing()) {
        SavePrecondition::NoFile => (ed.set_message("No file is visiting this buffer"), fx),
        SavePrecondition::Ready(path) => {
            fx.push(Effect::StatDisk {
                id,
                path: path.clone(),
            });
            (ed, fx)
        }
    }
}

/// Applies the disk state the shell found for `id`. A change since the read asks first (I-04).
pub fn disk_checked(ed: Editor, id: BufferId, state: DiskState, fx: Effects) -> (Editor, Effects) {
    let Some((_, disk)) = file_state(&ed, id) else {
        return (ed, fx);
    };
    if is_external_change(disk, state) {
        let ed = ed.confirm(
            Confirm::SaveExternal(id),
            "File changed on disk since it was read; overwrite? (y or n)",
        );
        (ed, fx)
    } else {
        let fx = write_buffer(&ed, id, fx);
        (ed, fx)
    }
}

/// Runs the action that a yes answer to `question` allows.
pub fn confirmed(ed: Editor, question: Confirm, fx: Effects) -> (Editor, Effects) {
    match question {
        Confirm::SaveExternal(id) => {
            let fx = write_buffer(&ed, id, fx);
            (ed, fx)
        }
        Confirm::QuitUnsaved => {
            let mut fx = fx;
            for buf in ed.buffers() {
                if let Some(eff) = Effect::remove_wal_if_file(buf) {
                    fx.push(eff);
                }
            }
            (ed.set_quit(true), fx)
        }
        Confirm::KillModified(id) => {
            let mut fx = fx;
            if let Some(eff) = Effect::remove_wal_if_file(ed.buffer_by_id(id)) {
                fx.push(eff);
            }
            (ed.kill_buffer_id(id), fx)
        }
        Confirm::RevertModified(id) => revert_now(ed, id, fx),
    }
}

/// Asks the shell to write the buffer to its file. The result comes back as `Written` (I-13).
pub(crate) fn write_buffer(ed: &Editor, id: BufferId, mut fx: Effects) -> Effects {
    let buf = ed.buffer_by_id(id);
    let Some(path) = buf.file_path() else {
        return fx;
    };
    fx.push(Effect::WriteFile {
        id,
        path: path.to_path_buf(),
        bytes: buf.eol().encode(&buf.text().to_bytes()),
    });
    fx
}

/// Records a finished write (I-05) and reports it in the echo area.
pub fn written(
    mut ed: Editor,
    id: BufferId,
    result: Result<DiskState, String>,
    fx: Effects,
) -> (Editor, Effects) {
    let Some(path) = ed.buffer_by_id(id).file_path().map(PathBuf::from) else {
        return (ed, fx);
    };
    let message = match result {
        Ok(new_disk) => {
            ed = ed.with_buffer(id, |b| b.record_saved(new_disk));
            format!("Wrote {}", path.display())
        }
        Err(e) => format!("Save failed: {e}"),
    };
    (ed.set_message(message), fx)
}

/// Opens the file `input` in a new buffer, or shows it if it is already open.
pub(crate) fn find_file(ed: Editor, input: &str, fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let path = PathBuf::from(input);
    let existing = ed.buffers().find_by_path(&path).map(Buffer::id);
    match existing {
        Some(id) => (switch_to(ed, id), fx),
        None => {
            let fx = request_open(&ed, path, None, true, fx);
            (ed, fx)
        }
    }
}

/// Writes the current buffer to a new file name and makes that its file (§8).
pub(crate) fn write_file(mut ed: Editor, input: &str, mut fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let id = ed.window().buffer;
    if let Some(eff) = Effect::remove_wal_if_file(ed.buffer_by_id(id)) {
        fx.push(eff);
    }
    let path = PathBuf::from(input);
    let name = path
        .file_name()
        .map_or_else(|| input.to_owned(), |n| n.to_string_lossy().into_owned());
    ed = ed.with_buffer(id, |b| {
        b.set_name(name)
            .set_backing(Backing::file(path, DiskState::Unknown))
    });
    let fx = write_buffer(&ed, id, fx);
    (ed, fx)
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
fn revert_now(ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    let Some((path, _)) = file_state(&ed, id) else {
        return (ed.set_message("Buffer has no file to revert to"), fx);
    };
    fx.push(Effect::ReadFile {
        target: ReadTarget::Revert(id),
        path,
        max_size: ed.config().max_file_size,
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

/// Applies the result of a file read: a new buffer for `Open`, a reload for `Revert` (C-11, C-12).
/// A file over `max-file-size` is refused: `Open` makes no buffer, `Revert` keeps the buffer
/// and its WAL (I-04).
pub fn file_read(
    ed: Editor,
    target: ReadTarget,
    path: PathBuf,
    result: Result<effect::Read, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    let max_size = ed.config().max_file_size;
    match (target, result) {
        (ReadTarget::Open { line, show }, Ok(read)) => match loaded_from(&path, read, max_size) {
            Ok(loaded) => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let (mut ed, id) = ed.add_buffer(&name, loaded.text, loaded.backing);
                ed = ed.with_buffer(id, |b| b.set_eol(loaded.eol));
                if let Some(n) = line {
                    let pos = ed.buffer_by_id(id).text().nth_line_start(n);
                    ed = ed.with_buffer(id, |b| b.set_saved_point(pos));
                }
                if show {
                    ed = switch_to(ed, id);
                }
                (ed, fx)
            }
            Err(e) => (ed.set_message(e.message()), fx),
        },
        (ReadTarget::Open { .. }, Err(e)) => {
            let ed = ed.set_message(format!("Cannot open {}: {e}", path.display()));
            (ed, fx)
        }
        (ReadTarget::Revert(id), Ok(read)) => match loaded_from(&path, read, max_size) {
            Ok(loaded) => {
                let limit = ed.config().undo_limit;
                let ed = ed
                    .with_buffer(id, |b| {
                        b.replace_from_disk(loaded.text, loaded.backing, loaded.eol, limit)
                    })
                    .snap_windows_for_buffer(id)
                    .set_message("Reverted");
                // The WAL goes only after the reload succeeds, so a refused revert keeps
                // the unsaved edits recoverable.
                fx.push(Effect::RemoveWal { path });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use crate::core::effect::{RawFile, Read};
    use crate::core::eol::Eol;
    use crate::core::text::Text;
    use crate::test_support::editor;
    use std::path::Path;

    /// An editor whose `max-file-size` is 4 bytes.
    fn small_limit() -> Editor {
        Editor::new(Config {
            max_file_size: 4,
            ..Config::default()
        })
    }

    /// Replaces the selected buffer with an unmodified file-backed buffer holding `bytes`.
    fn file_buffer(ed: Editor, bytes: &[u8], eol: Eol) -> Editor {
        ed.with_current_buffer(|b| {
            b.with_text(Text::from_bytes(bytes))
                .set_backing(Backing::file(PathBuf::from("f.txt"), DiskState::Unknown))
                .set_eol(eol)
                .set_modified(false)
        })
    }

    /// The bytes that saving buffer `id` would write.
    fn saved_bytes(ed: &Editor, id: BufferId) -> Vec<u8> {
        let fx = write_buffer(ed, id, Vec::new());
        let Effect::WriteFile { bytes, .. } = &fx[0] else {
            panic!("expected WriteFile");
        };
        bytes.clone()
    }

    #[test]
    fn toggle_to_crlf_saves_crlf_bytes() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert!(ed.buffer().modified());
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\n");
    }

    #[test]
    fn toggle_back_to_lf_saves_lf_bytes() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Crlf);
        let id = ed.window().buffer;
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(ed.buffer().eol(), Eol::Lf);
        assert_eq!(saved_bytes(&ed, id), b"a\nb\n");
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
        assert_eq!(ed.buffer().text().to_bytes(), b"a\nb\n");
        let (ed, _) = ed.undo(Vec::new());
        assert_eq!(ed.buffer().eol(), Eol::Lf);
        assert_eq!(ed.buffer().text().to_bytes(), b"a\nb\n");
    }

    #[test]
    fn toggle_eol_marks_wal_dirty() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert!(ed.buffer().backing().is_wal_dirty());
    }

    #[test]
    fn opened_mixed_file_normalizes_and_saves_majority_crlf() {
        let read = Read::Found(RawFile {
            len: 7,
            disk: DiskState::Unknown,
            bytes: b"a\r\nb\r\nc\n".to_vec(),
        });
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
        assert_eq!(ed.buffer_by_id(id).text().to_bytes(), b"a\nb\nc\n");
        assert!(!ed.buffer_by_id(id).modified());
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\nc\r\n");
    }

    #[test]
    fn opened_mixed_file_normalizes_and_saves_majority_lf_or_tie() {
        let read = Read::Found(RawFile {
            len: 5,
            disk: DiskState::Unknown,
            bytes: b"a\r\nb\n".to_vec(),
        });
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
        assert_eq!(ed.buffer_by_id(id).text().to_bytes(), b"a\nb\n");
        assert!(!ed.buffer_by_id(id).modified());
        assert_eq!(saved_bytes(&ed, id), b"a\nb\n");
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
    fn opened_crlf_file_saves_crlf_bytes() {
        let read = Read::Found(RawFile {
            len: 6,
            disk: DiskState::Unknown,
            bytes: b"a\r\nb\r\n".to_vec(),
        });
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
        assert_eq!(ed.buffer_by_id(id).text().to_bytes(), b"a\nb\n");
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\n");
    }

    #[test]
    fn revert_picks_up_line_endings_from_disk() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let read = Read::Found(RawFile {
            len: 6,
            disk: DiskState::Unknown,
            bytes: b"a\r\nb\r\n".to_vec(),
        });
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

    #[test]
    fn oversized_open_creates_no_buffer() {
        let ed = small_limit();
        let count = ed.buffers().iter().count();
        let read = Read::Found(RawFile {
            len: 10,
            disk: DiskState::Unknown,
            bytes: Vec::new(),
        });
        let target = ReadTarget::Open {
            line: None,
            show: true,
        };
        let (ed, fx) = file_read(ed, target, PathBuf::from("big.txt"), Ok(read), Vec::new());
        assert_eq!(ed.buffers().iter().count(), count);
        assert_eq!(ed.message(), "File too large: 10 bytes (max-file-size 4)");
        assert!(fx.is_empty());
    }

    #[test]
    fn oversized_revert_keeps_buffer_and_wal() {
        let ed = file_buffer(small_limit(), b"ab", Eol::Lf)
            .with_current_buffer(|b| b.set_modified(true));
        let id = ed.window().buffer;
        let read = Read::Found(RawFile {
            len: 10,
            disk: DiskState::Unknown,
            bytes: Vec::new(),
        });
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Revert(id),
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(ed.buffer().text().to_bytes(), b"ab");
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "File too large: 10 bytes (max-file-size 4)");
        // Nothing is removed: the unsaved edits stay recoverable from the WAL.
        assert!(fx.is_empty());
    }

    #[test]
    fn successful_revert_removes_wal_after_reload() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let id = ed.window().buffer;
        let read = Read::Found(RawFile {
            len: 2,
            disk: DiskState::Unknown,
            bytes: b"x\n".to_vec(),
        });
        let (ed, fx) = file_read(
            ed,
            ReadTarget::Revert(id),
            PathBuf::from("f.txt"),
            Ok(read),
            Vec::new(),
        );
        assert_eq!(ed.buffer().text().to_bytes(), b"x\n");
        assert!(
            matches!(fx.as_slice(), [Effect::RemoveWal { path }] if path == Path::new("f.txt"))
        );
    }
}

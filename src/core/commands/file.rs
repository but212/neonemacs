//! File operations: save check flow, reading, reverting, and confirmation (§8, ADR 0002).

use crate::core::buffer::BufferPool;
use crate::core::commands::buffer_window::{kill_now, switch_to};
use crate::core::commands::util::{ask, request_open};
use crate::core::editor::{Buffer, BufferId, Confirm, Editor, Window, snap};
use crate::core::effect::{self, Effect, Effects, ReadTarget};
use crate::core::file::{Backing, DiskState, LoadExtent, loaded_from};
use crate::core::undo::UndoLog;
use std::path::PathBuf;

// --- Layer 1: Pure domain primitives ---

/// Returns true if any buffer in the pool has unsaved modifications.
pub fn has_modified_buffers(pool: &BufferPool) -> bool {
    pool.iter().any(|b| b.modified)
}

/// Save check result for a backing file.
#[derive(Debug, PartialEq, Eq)]
pub enum SavePrecondition<'a> {
    NoFile,
    Truncated(&'a PathBuf),
    Ready(&'a PathBuf),
}

/// Checks the save preconditions for a backing in I-04 order.
pub fn check_save_precondition(backing: &Backing) -> SavePrecondition<'_> {
    match backing {
        Backing::File { path, extent, .. } => {
            if *extent == LoadExtent::Truncated {
                SavePrecondition::Truncated(path)
            } else {
                SavePrecondition::Ready(path)
            }
        }
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
        let ed = ask(
            ed,
            Confirm::QuitUnsaved,
            "Modified buffers exist; quit anyway? (y or n)",
        );
        (ed, fx)
    } else {
        (ed.set_quit(true), fx)
    }
}

fn file_state(ed: &Editor, id: BufferId) -> Option<(PathBuf, DiskState, LoadExtent)> {
    match &ed.buffer_by_id(id).backing {
        Backing::File { path, disk, extent } => Some((path.clone(), *disk, *extent)),
        Backing::None => None,
    }
}

/// Saves `id`. The checks run in I-04 order: truncated file, then external change.
/// A file-backed buffer asks the shell for its disk state first (`StatDisk`).
pub fn save_buffer(ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    match check_save_precondition(&ed.buffer_by_id(id).backing) {
        SavePrecondition::NoFile => (ed.set_message("No file is visiting this buffer"), fx),
        SavePrecondition::Truncated(_) => {
            let ed = ask(
                ed,
                Confirm::SaveTruncated(id),
                "File was truncated when read; save anyway? (y or n)",
            );
            (ed, fx)
        }
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
    let Some((_, disk, _)) = file_state(&ed, id) else {
        return (ed, fx);
    };
    if is_external_change(disk, state) {
        let ed = ask(
            ed,
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
        Confirm::SaveTruncated(id) => {
            let ed = ed.with_buffer(id, |b| Buffer {
                backing: confirm_truncated(b.backing),
                ..b
            });
            save_buffer(ed, id, fx)
        }
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
            (kill_now(ed, id), fx)
        }
        Confirm::RevertModified(id) => revert_now(ed, id, fx),
    }
}

/// A file whose read was truncated may be saved once the user agrees (I-04).
fn confirm_truncated(backing: Backing) -> Backing {
    match backing {
        Backing::File { path, disk, .. } => Backing::File {
            path,
            disk,
            extent: LoadExtent::TruncatedConfirmed,
        },
        Backing::None => Backing::None,
    }
}

/// Asks the shell to write the buffer to its file. The result comes back as `Written` (I-13).
pub(crate) fn write_buffer(ed: &Editor, id: BufferId, mut fx: Effects) -> Effects {
    let buf = ed.buffer_by_id(id);
    let Backing::File { path, .. } = &buf.backing else {
        return fx;
    };
    fx.push(Effect::WriteFile {
        id,
        path: path.clone(),
        bytes: buf.eol.encode(&buf.text.to_bytes()),
    });
    fx
}

/// Records the new disk state of a file-backed buffer.
fn with_disk(backing: Backing, disk: DiskState) -> Backing {
    match backing {
        Backing::File { path, extent, .. } => Backing::File { path, disk, extent },
        Backing::None => Backing::None,
    }
}

/// Records a finished write (I-05). The undo file follows, encoded from the new disk state (I-16).
pub fn written(
    mut ed: Editor,
    id: BufferId,
    result: Result<DiskState, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    let Backing::File { path, .. } = &ed.buffer_by_id(id).backing else {
        return (ed, fx);
    };
    let path = path.clone();
    let message = match result {
        Ok(new_disk) => {
            ed = ed.with_buffer(id, |b| Buffer {
                backing: with_disk(b.backing, new_disk),
                modified: false,
                undo: b.undo.close(),
                ..b
            });
            match ed.buffer_by_id(id).undo.encode(new_disk) {
                Some(bytes) => {
                    fx.push(Effect::WriteUndo { id, path, bytes });
                    return (ed, fx);
                }
                None => format!("Wrote {}", path.display()),
            }
        }
        Err(e) => format!("Save failed: {e}"),
    };
    (ed.set_message(message), fx)
}

/// Shows the result of the undo write that follows a successful save.
pub fn undo_written(ed: Editor, id: BufferId, result: Result<(), String>) -> Editor {
    let Backing::File { path, .. } = &ed.buffer_by_id(id).backing else {
        return ed;
    };
    let message = match result {
        Ok(()) => format!("Wrote {}", path.display()),
        Err(e) => format!("Wrote {}, undo not saved: {e}", path.display()),
    };
    ed.set_message(message)
}

/// Opens the file `input` in a new buffer, or shows it if it is already open.
pub(crate) fn find_file(ed: Editor, input: &str, fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let path = PathBuf::from(input);
    let existing = ed
        .buffers()
        .iter()
        .find(|b| matches!(&b.backing, Backing::File { path: p, .. } if *p == path))
        .map(|b| b.id);
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
    ed = ed.with_buffer(id, |b| Buffer {
        name,
        backing: Backing::File {
            path,
            disk: DiskState::Unknown,
            extent: LoadExtent::Whole,
        },
        ..b
    });
    let fx = write_buffer(&ed, id, fx);
    (ed, fx)
}

pub(crate) fn revert_buffer(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    if ed.buffer().modified {
        let ed = ask(
            ed,
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
    let Some((path, _, _)) = file_state(&ed, id) else {
        return (ed.set_message("Buffer has no file to revert to"), fx);
    };
    fx.push(Effect::ReadFile {
        target: ReadTarget::Revert(id),
        path,
        max_size: ed.config().max_file_size,
    });
    (ed, fx)
}

/// Switches the buffer between LF and CRLF. Only the save encoding changes, so the switch
/// is not in the undo log.
pub(crate) fn toggle_eol(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    let buf = ed.buffer_by_id(id);
    if buf.view_only {
        return (ed.set_message("Buffer is read-only"), fx);
    }
    let eol = buf.eol.toggled();
    let ed = ed.with_buffer(id, |b| Buffer {
        eol,
        modified: true,
        ..b
    });
    (ed.set_message(format!("Line endings: {}", eol.label())), fx)
}

/// Applies the result of a file read: a new buffer for `Open`, a reload for `Revert` (C-11, C-12).
pub fn file_read(
    ed: Editor,
    target: ReadTarget,
    path: PathBuf,
    result: Result<effect::Read, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    match (target, result) {
        (ReadTarget::Open { line, show }, Ok(read)) => {
            let loaded = loaded_from(&path, read, ed.config().max_file_size);
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let (mut ed, id) = ed.add_buffer(&name, loaded.text, loaded.backing);
            ed = ed.with_buffer(id, |b| Buffer {
                eol: loaded.eol,
                ..b
            });
            if let Some(n) = line {
                let pos = ed.buffer_by_id(id).text.nth_line_start(n);
                ed = ed.with_buffer(id, |b| Buffer {
                    saved_point: pos,
                    ..b
                });
            }
            fx.push(Effect::ReadUndo { id, path });
            if show {
                ed = switch_to(ed, id);
            }
            (ed, fx)
        }
        (ReadTarget::Open { .. }, Err(e)) => {
            let ed = ed.set_message(format!("Cannot open {}: {e}", path.display()));
            (ed, fx)
        }
        (ReadTarget::Revert(id), Ok(read)) => {
            let loaded = loaded_from(&path, read, ed.config().max_file_size);
            let limit = ed.config().undo_limit;
            let ed = ed.with_buffer(id, |b| Buffer {
                text: loaded.text,
                backing: loaded.backing,
                eol: loaded.eol,
                modified: false,
                undo: UndoLog::new(limit),
                ..b
            });
            let text = ed.buffer_by_id(id).text.clone();
            let windows = ed.windows().clone().map_windows(&|w: Window| {
                if w.buffer == id {
                    Window {
                        point: snap(&text, w.point.min(text.len())),
                        top: 0,
                        ..w
                    }
                } else {
                    w
                }
            });
            let ed = ed.set_windows(windows).set_message("Reverted");
            (ed, fx)
        }
        (ReadTarget::Revert(_), Err(e)) => {
            let ed = ed.set_message(format!("Revert failed: {e}"));
            (ed, fx)
        }
    }
}

/// Applies the undo file read for `id`. The core keeps it only if it matches the disk state (I-10).
pub fn undo_read(ed: Editor, id: BufferId, bytes: Option<Vec<u8>>) -> Editor {
    let Some(bytes) = bytes else {
        return ed;
    };
    let disk = match &ed.buffer_by_id(id).backing {
        Backing::File { disk, .. } => *disk,
        Backing::None => DiskState::Unknown,
    };
    match UndoLog::decode(&bytes, disk, ed.config().undo_limit) {
        Some(log) => ed.with_buffer(id, |b| Buffer { undo: log, ..b }),
        None => ed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::effect::{RawFile, Read};
    use crate::core::eol::Eol;
    use crate::core::text::Text;
    use crate::test_support::editor;

    /// Replaces the selected buffer with an unmodified file-backed buffer holding `bytes`.
    fn file_buffer(ed: Editor, bytes: &[u8], eol: Eol) -> Editor {
        ed.with_current_buffer(|b| Buffer {
            text: Text::from_bytes(bytes),
            backing: Backing::File {
                path: PathBuf::from("f.txt"),
                disk: DiskState::Unknown,
                extent: LoadExtent::Whole,
            },
            eol,
            modified: false,
            ..b
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
        assert_eq!(ed.buffer().eol, Eol::Crlf);
        assert!(ed.buffer().modified);
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\n");
    }

    #[test]
    fn toggle_back_to_lf_saves_lf_bytes() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Crlf);
        let id = ed.window().buffer;
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(ed.buffer().eol, Eol::Lf);
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
    fn toggle_leaves_text_and_undo_alone() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf);
        let undo_before = ed.buffer().undo.clone();
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(ed.buffer().text.to_bytes(), b"a\nb\n");
        assert_eq!(ed.buffer().undo, undo_before);
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
        let id = ed.buffers().iter().last().unwrap().id;
        assert_eq!(ed.buffer_by_id(id).eol, Eol::Crlf);
        assert_eq!(ed.buffer_by_id(id).text.to_bytes(), b"a\nb\nc\n");
        assert!(!ed.buffer_by_id(id).modified);
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
        let id = ed.buffers().iter().last().unwrap().id;
        assert_eq!(ed.buffer_by_id(id).eol, Eol::Lf);
        assert_eq!(ed.buffer_by_id(id).text.to_bytes(), b"a\nb\n");
        assert!(!ed.buffer_by_id(id).modified);
        assert_eq!(saved_bytes(&ed, id), b"a\nb\n");
    }

    #[test]
    fn read_only_buffer_refuses_toggle() {
        let ed = file_buffer(editor(), b"a\nb\n", Eol::Lf).with_current_buffer(|b| Buffer {
            view_only: true,
            ..b
        });
        let (ed, _) = toggle_eol(ed, Vec::new());
        assert_eq!(ed.buffer().eol, Eol::Lf);
        assert!(!ed.buffer().modified);
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
        let id = ed.buffers().iter().last().unwrap().id;
        assert_eq!(ed.buffer_by_id(id).eol, Eol::Crlf);
        assert_eq!(ed.buffer_by_id(id).text.to_bytes(), b"a\nb\n");
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
        assert_eq!(ed.buffer().eol, Eol::Crlf);
        assert_eq!(saved_bytes(&ed, id), b"a\r\nb\r\n");
    }
}

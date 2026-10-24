//! File operations: save check flow, reading, reverting, and confirmation (§8, ADR 0002).

use crate::core::commands::buffer_window::{kill_now, switch_to};
use crate::core::commands::util::{ask, request_open};
use crate::core::editor::{Buffer, BufferId, Confirm, Editor, Window, snap};
use crate::core::effect::{self, Effect, Effects, ReadTarget};
use crate::core::file::{Backing, DiskState, LoadExtent, loaded_from};
use crate::core::undo::UndoLog;
use std::path::PathBuf;

pub(crate) fn save_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    save_buffer(ed, id, fx)
}

pub(crate) fn save_buffers_kill_emacs(ed: Editor, fx: Effects) -> (Editor, Effects) {
    if ed.buffers().iter().any(|b| b.modified) {
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
    let Some((path, _, extent)) = file_state(&ed, id) else {
        return (ed.set_message("No file is visiting this buffer"), fx);
    };
    if extent == LoadExtent::Truncated {
        let ed = ask(
            ed,
            Confirm::SaveTruncated(id),
            "File was truncated when read; save anyway? (y or n)",
        );
        (ed, fx)
    } else {
        fx.push(Effect::StatDisk { id, path });
        (ed, fx)
    }
}

/// Applies the disk state the shell found for `id`. A change since the read asks first (I-04).
pub fn disk_checked(ed: Editor, id: BufferId, state: DiskState, fx: Effects) -> (Editor, Effects) {
    let Some((_, disk, _)) = file_state(&ed, id) else {
        return (ed, fx);
    };
    if matches!(disk, DiskState::Known { .. }) && disk != state {
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
        bytes: buf.text.to_bytes(),
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

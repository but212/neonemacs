//! File operations: save check flow, reading, reverting, and confirmation (§8, ADR 0002).

use crate::core::commands::buffer_window::switch_to;
use crate::core::editor::{Buffer, BufferId, Confirm, Editor};
use crate::core::effect::{self, Effect, Effects, ReadTarget, StatTarget, WalProbe};
use crate::core::file::{Backing, DiskState, loaded_from};
use crate::core::undo::Edit;
use std::fmt::Write;
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

pub(crate) fn save_buffer_cmd(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let id = ed.window().buffer;
    save_buffer(ed, id, fx)
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

/// Saves `id`. A file-backed buffer asks the shell for its disk state first (`StatDisk`);
/// `disk_checked` then applies the external-change check (I-04).
#[must_use]
pub fn save_buffer(ed: Editor, id: BufferId, mut fx: Effects) -> (Editor, Effects) {
    match ed.buffer_by_id(id).file_path() {
        None => (ed.set_message("No file is visiting this buffer"), fx),
        Some(path) => {
            fx.push(Effect::StatDisk {
                target: StatTarget::Save(id),
                path: path.to_path_buf(),
            });
            (ed, fx)
        }
    }
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
    path: &Path,
    state: DiskState,
    fx: Effects,
) -> (Editor, Effects) {
    let id = target.id();
    let file = ed.resolve_path(path);
    let own = ed.buffer_by_id(id).file_path() == Some(file.as_path());
    if matches!(target, StatTarget::SaveAs(_)) && !own {
        if matches!(state, DiskState::Known { .. }) {
            let message = format!("File {} exists; overwrite? (y or n)", path.display());
            let ed = ed.confirm(
                Confirm::OverwriteFile {
                    id,
                    path: path.to_path_buf(),
                },
                message,
            );
            return (ed, fx);
        }
        let fx = write_to(&ed, id, path.to_path_buf(), fx);
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
            let fx = write_to(&ed, id, path, fx);
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

/// Asks the shell to write the buffer to its file. The result comes back as `Written` (I-04).
pub(crate) fn write_buffer(ed: &Editor, id: BufferId, fx: Effects) -> Effects {
    match ed.buffer_by_id(id).file_path() {
        Some(path) => write_to(ed, id, path.to_path_buf(), fx),
        None => fx,
    }
}

/// Asks the shell to write the buffer to `path`. The buffer's file changes only in `written` (I-36).
fn write_to(ed: &Editor, id: BufferId, path: PathBuf, mut fx: Effects) -> Effects {
    let buf = ed.buffer_by_id(id);
    fx.push(Effect::WriteFile {
        id,
        path,
        bytes: buf.eol().encode(&buf.text().to_bytes()),
    });
    fx
}

/// Records a finished write (I-05) and reports it in the echo area. A write to a new path moves
/// the buffer there and removes the WAL of the old path (I-25, I-36). A failed write changes nothing.
#[must_use]
pub fn written(
    mut ed: Editor,
    id: BufferId,
    path: &Path,
    result: Result<DiskState, String>,
    mut fx: Effects,
) -> (Editor, Effects) {
    let message = match result {
        Ok(new_disk) => {
            let file = ed.resolve_path(path);
            if ed.buffer_by_id(id).file_path() != Some(file.as_path()) {
                if let Some(eff) = Effect::remove_wal_if_file(ed.buffer_by_id(id)) {
                    fx.push(eff);
                }
                let name = name_of(file.as_path());
                ed = ed.with_buffer(id, |b| {
                    b.set_name(name).set_backing(Backing::file(file, new_disk))
                });
            }
            ed = ed.with_buffer(id, |b| b.record_saved(new_disk));
            format!("Wrote {}", path.display())
        }
        Err(e) => format!("Save failed: {e}"),
    };
    (ed.set_message(message), fx)
}

/// The buffer name for `path`: its last component, or the whole path when it has none.
fn name_of(path: &Path) -> String {
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

/// Writes the current buffer to a new file name. `written` makes that its file on success (§8).
pub(crate) fn write_file(ed: Editor, input: &str, mut fx: Effects) -> (Editor, Effects) {
    if input.is_empty() {
        return (ed, fx);
    }
    let id = ed.window().buffer;
    let path = ed.resolve_path(Path::new(input));
    // Another buffer visits this path, so writing here would give two buffers one WAL (I-37).
    if let Some(other) = ed
        .buffers()
        .find_by_path(path.as_path())
        .filter(|b| b.id() != id)
    {
        let message = format!(
            "Buffer {} is visiting {}",
            other.name(),
            path.as_path().display()
        );
        return (ed.set_message(message), fx);
    }
    fx.push(Effect::StatDisk {
        target: StatTarget::SaveAs(id),
        path: path.as_path().to_path_buf(),
    });
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

/// Switches the buffer between LF and CRLF as an undoable edit (I-31). `apply` refuses a
/// read-only buffer and sets its message.
pub(crate) fn toggle_eol(ed: Editor, fx: Effects) -> (Editor, Effects) {
    let from = ed.buffer().eol();
    let to = from.toggled();
    let (ed, applied) = ed.apply(&Edit::SetEol { from, to });
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
    let file = ed.resolve_path(&path);
    match (target, result) {
        (ReadTarget::Open { line, show }, Ok(read)) => {
            // A visited path keeps its one buffer (I-37), so `line` does not move it.
            if let Some(id) = ed.buffers().find_by_path(file.as_path()).map(Buffer::id) {
                let ed = if show { switch_to(ed, id) } else { ed };
                return (ed, fx);
            }
            match loaded_from(&file, read, max_size) {
                Ok(loaded) => {
                    let (mut ed, id) = ed.add_buffer(&name_of(&path), loaded.text, loaded.backing);
                    ed = ed.with_buffer(id, |b| b.set_eol(loaded.eol));
                    if let Some(n) = line {
                        let pos = ed.buffer_by_id(id).text().nth_line_start(n);
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
        (ReadTarget::Revert(_), Ok(effect::Read::Missing)) => {
            let ed = ed.set_message(format!("Revert failed: {} does not exist", path.display()));
            (ed, fx)
        }
        (ReadTarget::Revert(id), Ok(read)) => match loaded_from(&file, read, max_size) {
            Ok(loaded) => {
                let ed = ed
                    .with_buffer(id, |b| {
                        b.replace_from_disk(loaded.text, loaded.backing, loaded.eol)
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
            .restore_buffer_content(path, &recovered.bytes, recovered.eol, line)
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
    use crate::core::config::Config;
    use crate::core::effect::{RawFile, Read, Recovered};
    use crate::core::eol::Eol;
    use crate::core::file::FilePath;
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
                .set_name("f.txt")
                .set_backing(Backing::file(
                    FilePath::resolve(Path::new(""), Path::new("f.txt")),
                    DiskState::Unknown,
                ))
                .set_eol(eol)
                .mark_clean()
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

    fn quit_message(names: &[&str]) -> String {
        let mut ed = editor();
        for name in names {
            let (next, id) = ed.add_buffer(name, Text::default(), Backing::None);
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

    fn found(bytes: &[u8], disk: DiskState) -> Read {
        Read::Found(RawFile {
            len: bytes.len() as u64,
            disk,
            bytes: bytes.to_vec(),
        })
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
        assert_eq!(ed.buffer().text().to_bytes(), b"disk");
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "");
    }

    #[test]
    fn valid_wal_restores_text_and_eol_and_says_so() {
        let ed = probed(WalProbe::Valid(Recovered {
            eol: Eol::Crlf,
            bytes: b"draft".to_vec(),
        }));
        assert_eq!(ed.buffer().text().to_bytes(), b"draft");
        assert_eq!(ed.buffer().eol(), Eol::Crlf);
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "[Recovered f.txt from WAL]");
    }

    #[test]
    fn unusable_wal_is_reported_and_leaves_the_text() {
        let ed = probed(WalProbe::Stale(Ok(PathBuf::from("dir/.f.txt.wal.stale"))));
        assert_eq!(ed.buffer().text().to_bytes(), b"disk");
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "Stale WAL kept as .f.txt.wal.stale");

        let ed = probed(WalProbe::Stale(Err("denied".to_owned())));
        assert_eq!(ed.message(), "Stale WAL for f.txt not kept: denied");
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
        assert_eq!(fx, [] as [Effect; 0]);
    }

    #[test]
    fn oversized_revert_keeps_buffer_and_wal() {
        let ed =
            file_buffer(small_limit(), b"ab", Eol::Lf).with_current_buffer(Buffer::mark_modified);
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
        assert_eq!(fx, [] as [Effect; 0]);
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

    #[test]
    fn open_of_visited_path_reuses_buffer() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let count = ed.buffers().iter().count();
        let read = Read::Found(RawFile {
            len: 3,
            disk: DiskState::Unknown,
            bytes: b"abc".to_vec(),
        });
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
            b.with_text(Text::from_bytes(b"abc"))
                .set_name("a.txt")
                .set_backing(Backing::file(
                    FilePath::resolve(Path::new("/w"), Path::new("a.txt")),
                    DiskState::Unknown,
                ))
                .mark_clean()
        });
        let count = ed.buffers().iter().count();
        let read = Read::Found(RawFile {
            len: 3,
            disk: DiskState::Unknown,
            bytes: b"abc".to_vec(),
        });
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
    fn write_file_to_visited_path_is_refused() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let (ed, _) = ed.add_buffer(
            "g.txt",
            Text::from_bytes(b"x"),
            Backing::file(
                FilePath::resolve(Path::new(""), Path::new("g.txt")),
                DiskState::Unknown,
            ),
        );
        let before = ed.buffer().backing().clone();

        let (ed, fx) = write_file(ed, "g.txt", Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        assert_eq!(ed.message(), "Buffer g.txt is visiting g.txt");
        assert_eq!(*ed.buffer().backing(), before);
    }

    #[test]
    fn write_file_to_parent_spelling_of_visited_path_is_refused() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let (ed, _) = ed.add_buffer(
            "g.txt",
            Text::from_bytes(b"x"),
            Backing::file(
                FilePath::resolve(Path::new(""), Path::new("g.txt")),
                DiskState::Unknown,
            ),
        );

        let (ed, fx) = write_file(ed, "sub/../g.txt", Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        assert_eq!(ed.message(), "Buffer g.txt is visiting g.txt");
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
            &PathBuf::from("missing/b.txt"),
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
        assert!(
            matches!(fx.as_slice(), [Effect::WriteFile { path, .. }] if path == Path::new("b.txt"))
        );

        let disk = DiskState::Known {
            mtime: std::time::UNIX_EPOCH,
            size: 3,
        };
        let (ed, fx) = written(ed, id, &PathBuf::from("b.txt"), Ok(disk), Vec::new());
        assert_eq!(
            fx,
            vec![Effect::RemoveWal {
                path: PathBuf::from("f.txt")
            }]
        );
        assert_eq!(
            *ed.buffer().backing(),
            Backing::file(FilePath::resolve(Path::new(""), Path::new("b.txt")), disk)
        );
        assert_eq!(ed.buffer().name(), "b.txt");
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "Wrote b.txt");
    }

    fn known(size: u64) -> DiskState {
        DiskState::Known {
            mtime: std::time::UNIX_EPOCH,
            size,
        }
    }

    /// The `StatDisk` that `write_file` requests for `input`.
    fn stat_for_write(ed: Editor, input: &str) -> (Editor, StatTarget, PathBuf) {
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
            crate::core::mode::Mode::Confirm(q) if q.kind == Confirm::OverwriteFile {
                id,
                path: PathBuf::from("other.txt")
            }
        ));
        assert_eq!(ed.message(), "File other.txt exists; overwrite? (y or n)");
    }

    #[test]
    fn write_file_to_missing_path_writes() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let (ed, target, path) = stat_for_write(ed, "other.txt");
        let (_, fx) = disk_checked(ed, target, &path, DiskState::Unknown, Vec::new());
        assert!(
            matches!(fx.as_slice(), [Effect::WriteFile { path, .. }] if path == Path::new("other.txt"))
        );
    }

    #[test]
    fn write_file_to_own_path_uses_save_check() {
        // The buffer's file is f.txt with recorded state Known(3).
        let ed = file_buffer(editor(), b"abc", Eol::Lf).with_current_buffer(|b| {
            b.set_backing(Backing::file(
                FilePath::resolve(Path::new(""), Path::new("f.txt")),
                known(3),
            ))
        });
        let (ed, target, path) = stat_for_write(ed, "f.txt");

        // Unchanged on disk: written without asking.
        let (same, fx) = disk_checked(ed.clone(), target, &path, known(3), Vec::new());
        assert!(matches!(fx.as_slice(), [Effect::WriteFile { .. }]));
        assert!(matches!(
            same.mode(),
            crate::core::mode::Mode::Normal { .. }
        ));

        // Changed on disk: the external-change question, not the overwrite one.
        let (changed, fx) = disk_checked(ed, target, &path, known(4), Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        assert!(matches!(
            changed.mode(),
            crate::core::mode::Mode::Confirm(q) if matches!(q.kind, Confirm::SaveExternal(_))
        ));
    }

    #[test]
    fn overwrite_confirmation_yes_writes_to_the_asked_path() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let question = Confirm::OverwriteFile {
            id,
            path: PathBuf::from("other.txt"),
        };
        let (ed, fx) = confirmed(ed, question, Vec::new());
        assert_eq!(
            fx,
            vec![Effect::WriteFile {
                id,
                path: PathBuf::from("other.txt"),
                bytes: b"abc".to_vec()
            }]
        );
        // Only `written` moves the buffer (I-36).
        assert_eq!(ed.buffer().name(), "f.txt");
    }

    #[test]
    fn save_of_missing_opened_file_asks_when_created_externally() {
        // Opened as missing (Unknown); the file exists by the time of the save.
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let (ed, fx) = disk_checked(
            ed,
            StatTarget::Save(id),
            Path::new("f.txt"),
            known(5),
            Vec::new(),
        );
        assert_eq!(fx, [] as [Effect; 0]);
        assert!(matches!(
            ed.mode(),
            crate::core::mode::Mode::Confirm(q) if matches!(q.kind, Confirm::SaveExternal(_))
        ));
    }

    #[test]
    fn save_of_new_file_still_writes() {
        let ed = file_buffer(editor(), b"abc", Eol::Lf);
        let id = ed.window().buffer;
        let (_, fx) = disk_checked(
            ed,
            StatTarget::Save(id),
            Path::new("f.txt"),
            DiskState::Unknown,
            Vec::new(),
        );
        assert!(matches!(fx.as_slice(), [Effect::WriteFile { .. }]));
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
        assert_eq!(ed.buffer().text().to_bytes(), b"abc");
        assert!(ed.buffer().modified());
        assert_eq!(ed.message(), "Revert failed: f.txt does not exist");
        assert_eq!(fx, [] as [Effect; 0]);
    }
}

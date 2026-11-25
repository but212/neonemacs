//! Opens the startup files. Each one is read like `find-file` does, which also recovers its WAL
//! (I-44, I-45).

use super::args::FileArg;
use crate::core::Buffer;
use crate::core::DiskState;
use crate::core::Editor;
use crate::core::effect::WalProbe;
use crate::core::{request_open, switch_to};
use crate::shell::{perform_all, wal};
use std::path::{Path, PathBuf};

/// Puts the unsaved text left in the scratch slot of `ed` by an earlier run into the scratch buffer.
/// An unusable slot is moved aside and reported (I-44). The notice follows an earlier message.
#[must_use]
pub fn restore_scratch(ed: Editor) -> Editor {
    let Some(slot) = ed.scratch_slot().map(PathBuf::from) else {
        return ed;
    };
    let (ed, note) = match wal::probe_file(&slot, DiskState::Unknown) {
        WalProbe::Absent | WalProbe::ValidDelta(_) => return ed,
        WalProbe::Valid(r) => (
            ed.restore_scratch(r.doc, r.eol),
            "[Recovered *scratch* from WAL]".to_owned(),
        ),
        WalProbe::Stale(Ok(kept)) => {
            let name = Path::new(&kept)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (ed, format!("Stale WAL kept as {name}"))
        }
        WalProbe::Stale(Err(e)) => (ed, format!("Stale WAL for *scratch* not kept: {e}")),
    };
    ed.append_message(note)
}

/// Opens every file in the background. The first one that opens is shown. A `+n` sets its starting line.
///
/// The notices of all files follow the earlier message (for example an `init.lua` error), joined
/// by `; ` (I-56).
pub fn open_files(mut ed: Editor, lua: Option<&mlua::Lua>, files: &[FileArg]) -> Editor {
    let first_new = ed.buffers().len();
    let before = ed.message().to_owned();
    let mut notes = Vec::new();

    for f in files {
        ed = ed.clear_message();
        ed = if let Some(raw_path) = f.path.to_str() {
            let path = ed.resolve_path(raw_path);
            let fx = request_open(&ed, path.as_str(), f.line, false, Vec::new());
            perform_all(ed, lua, fx)
        } else {
            ed.append_message(format!(
                "Cannot open {}: path is not valid UTF-8",
                f.path.display()
            ))
        };
        if !ed.message().is_empty() {
            notes.push(ed.message().to_owned());
        }
    }

    ed = ed.set_message(before);
    for note in notes {
        ed = ed.append_message(note);
    }

    match ed.buffers().iter().nth(first_new).map(Buffer::id) {
        Some(id) => switch_to(ed, id),
        None => ed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Config;
    use crate::core::Eol;
    use crate::shell::startup::files_of;
    use crate::shell::wal;
    use std::fs;

    #[test]
    fn first_file_is_shown_and_line_applies_to_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"a\n").unwrap();
        fs::write(&b, b"1\n2\n3\n4\n").unwrap();
        let args = [
            a.display().to_string(),
            "+3".to_owned(),
            b.display().to_string(),
        ];
        let files = files_of(&args);

        let ed = open_files(Editor::new(Config::default()), None, &files);
        assert_eq!(ed.buffer().name(), "a.txt");
        let other = ed.buffers().iter().find(|x| x.name() == "b.txt").unwrap();
        assert_eq!(other.saved_point(), 4);
    }

    #[test]
    fn duplicate_args_recover_wal_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "disk a").unwrap();
        let disk = crate::shell::file::disk_state(&path);
        wal::write_wal(&path, disk, Eol::Lf, b"recovered a").unwrap();

        let args = [path.display().to_string(), path.display().to_string()];
        let files = files_of(&args);
        let ed = open_files(Editor::new(Config::default()), None, &files);
        assert_eq!(ed.message(), "[Recovered a.txt from WAL]");
    }

    #[test]
    fn duplicate_args_make_one_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, b"a\n").unwrap();
        // Both `.` and `sub/..` fold lexically to the same file, so each must reuse the buffer.
        for alias in [
            dir.path().join(".").join("a.txt"),
            dir.path().join("sub").join("..").join("a.txt"),
        ] {
            let args = [a.display().to_string(), alias.display().to_string()];
            let files = files_of(&args);
            let ed = open_files(Editor::new(Config::default()), None, &files);
            assert_eq!(
                ed.buffers().iter().filter(|b| b.name() == "a.txt").count(),
                1
            );
        }
    }

    #[test]
    fn open_files_empty_arguments() {
        let ed = open_files(Editor::new(Config::default()), None, &[]);
        assert_eq!(ed.buffers().len(), 1);
        assert_eq!(ed.buffer().name(), "*scratch*");
        assert_eq!(ed.message(), "");
    }

    #[test]
    fn open_files_wal_recovery_multiple_and_message() {
        use crate::core::Doc;
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.txt");
        let path_b = dir.path().join("b.txt");
        fs::write(&path_a, "disk a").unwrap();
        fs::write(&path_b, "disk b").unwrap();

        let disk_a = crate::shell::file::disk_state(&path_a);
        let disk_b = crate::shell::file::disk_state(&path_b);

        wal::write_wal(&path_a, disk_a, Eol::Lf, b"recovered a").unwrap();
        wal::write_wal(&path_b, disk_b, Eol::Lf, b"recovered b").unwrap();

        let files = vec![
            FileArg {
                path: path_a,
                line: None,
            },
            FileArg {
                path: path_b,
                line: None,
            },
        ];

        let ed = open_files(Editor::new(Config::default()), None, &files);
        assert_eq!(ed.buffer().name(), "a.txt");
        assert_eq!(*ed.buffer().text(), Doc::from_bytes(b"recovered a"));
        assert!(ed.buffer().modified());

        let buf_b = ed.buffers().iter().find(|b| b.name() == "b.txt").unwrap();
        assert_eq!(*buf_b.text(), Doc::from_bytes(b"recovered b"));
        assert!(buf_b.modified());

        assert_eq!(
            ed.message(),
            "[Recovered a.txt from WAL]; [Recovered b.txt from WAL]"
        );
    }

    #[test]
    fn open_files_wal_corruption_and_external_modification_resilience() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.txt");
        fs::write(&path, "original content").unwrap();

        let disk = crate::shell::file::disk_state(&path);
        wal::write_wal(&path, disk, Eol::Lf, b"wal draft").unwrap();

        // 1. External change after WAL: modified on disk
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, "external change from git").unwrap();

        let files = vec![FileArg { path, line: None }];
        let ed = open_files(Editor::new(Config::default()), None, &files);
        // Stale WAL must not be recovered; disk content loaded
        assert_eq!(
            *ed.buffer().text(),
            crate::core::Doc::from_bytes(b"external change from git")
        );
        assert!(!ed.buffer().modified());
        // The draft is not lost: it is moved aside and the user is told (I-44).
        assert_eq!(ed.message(), "Stale WAL kept as .corrupt.txt.wal.stale");
        assert!(!wal::has_wal(&dir.path().join("corrupt.txt")));
        assert!(wal::stale_path(&dir.path().join("corrupt.txt")).is_file());

        // 2. Corrupted WAL file (invalid bytes in WAL path)
        let corrupt_path = dir.path().join("bad_wal.txt");
        fs::write(&corrupt_path, "good disk").unwrap();
        let cur_disk = crate::shell::file::disk_state(&corrupt_path);
        wal::write_wal(&corrupt_path, cur_disk, Eol::Lf, b"wal draft").unwrap();
        let wal_file = wal::wal_path(&corrupt_path);
        // Overwrite WAL with garbage
        fs::write(&wal_file, b"GARBAGE_NOT_A_VALID_WAL_FORMAT").unwrap();

        let files2 = vec![FileArg {
            path: corrupt_path,
            line: None,
        }];
        let ed2 = open_files(Editor::new(Config::default()), None, &files2);
        assert_eq!(
            *ed2.buffer().text(),
            crate::core::Doc::from_bytes(b"good disk")
        );
        assert!(!ed2.buffer().modified());
        assert_eq!(ed2.message(), "Stale WAL kept as .bad_wal.txt.wal.stale");
    }

    #[test]
    fn open_notices_come_only_from_the_files_that_have_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.txt");
        fs::write(&path, "initial").unwrap();
        let files = vec![FileArg {
            path: path.clone(),
            line: None,
        }];

        // Without a WAL: opens in the background, no notice.
        let core_path = |ed: &Editor| ed.resolve_path(path.to_str().unwrap());
        let ed = open_files(Editor::new(Config::default()), None, &files);
        let buf = ed.buffers().find_by_path(core_path(&ed).as_str()).unwrap();
        assert_eq!(*buf.text(), crate::core::Doc::from_bytes(b"initial"));
        assert!(!buf.modified());
        assert_eq!(ed.message(), "");

        // With a WAL: recovers the text.
        let disk = crate::shell::file::disk_state(&path);
        wal::write_wal(&path, disk, Eol::Lf, b"wal draft").unwrap();
        let ed = open_files(Editor::new(Config::default()), None, &files);
        let buf = ed.buffers().find_by_path(core_path(&ed).as_str()).unwrap();
        assert_eq!(*buf.text(), crate::core::Doc::from_bytes(b"wal draft"));
        assert!(buf.modified());
        assert_eq!(ed.message(), "[Recovered single.txt from WAL]");
    }

    #[test]
    fn scratch_slot_restores_into_the_scratch_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("scratch.wal");
        wal::write_scratch(&slot, Eol::Lf, b"draft").unwrap();

        let ed = restore_scratch(
            Editor::new(Config::default())
                .with_scratch_slot(Some(slot.to_string_lossy().into_owned())),
        );
        assert_eq!(ed.buffer().name(), "*scratch*");
        assert_eq!(*ed.buffer().text(), crate::core::Doc::from_bytes(b"draft"));
        assert!(ed.buffer().modified());
        assert!(!ed.buffer().is_wal_dirty());
        assert_eq!(ed.message(), "[Recovered *scratch* from WAL]");
    }

    #[test]
    fn scratch_slot_notice_follows_an_earlier_message() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("scratch.wal");
        wal::write_scratch(&slot, Eol::Lf, b"draft").unwrap();
        let ed = Editor::new(Config::default()).set_message("init.lua: boom");
        let ed = restore_scratch(ed.with_scratch_slot(Some(slot.to_string_lossy().into_owned())));
        assert_eq!(
            ed.message(),
            "init.lua: boom; [Recovered *scratch* from WAL]"
        );
    }

    #[test]
    fn no_slot_or_an_empty_one_restores_nothing() {
        let ed = restore_scratch(Editor::new(Config::default()));
        assert!(!ed.buffer().modified());
        let dir = tempfile::tempdir().unwrap();
        let ed = restore_scratch(
            ed.with_scratch_slot(Some(
                dir.path()
                    .join("missing.wal")
                    .to_string_lossy()
                    .into_owned(),
            )),
        );
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "");
    }

    #[test]
    fn an_unusable_scratch_slot_is_kept_aside_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let slot = dir.path().join("scratch.wal");
        fs::write(&slot, b"GARBAGE").unwrap();
        let ed = restore_scratch(
            Editor::new(Config::default())
                .with_scratch_slot(Some(slot.to_string_lossy().into_owned())),
        );
        assert!(!ed.buffer().modified());
        assert_eq!(ed.message(), "Stale WAL kept as scratch.wal.stale");
        assert!(dir.path().join("scratch.wal.stale").is_file());
    }

    #[test]
    fn an_earlier_message_survives_files_without_notices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "a").unwrap();
        let files = vec![FileArg { path, line: None }];
        let ed = Editor::new(Config::default()).set_message("init.lua: boom");
        let ed = open_files(ed, None, &files);
        assert_eq!(ed.message(), "init.lua: boom");
    }

    #[test]
    fn file_notices_follow_an_earlier_message() {
        // I-56
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "disk a").unwrap();
        let disk = crate::shell::file::disk_state(&path);
        wal::write_wal(&path, disk, Eol::Lf, b"recovered a").unwrap();
        let files = vec![FileArg { path, line: None }];
        let ed = Editor::new(Config::default()).set_message("init.lua: boom");
        let ed = open_files(ed, None, &files);
        assert_eq!(ed.message(), "init.lua: boom; [Recovered a.txt from WAL]");
    }

    #[test]
    fn oversized_file_is_not_recovered_from_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        fs::write(&path, "0123456789").unwrap();
        wal::write_wal(
            &path,
            crate::shell::file::disk_state(&path),
            Eol::Lf,
            b"wal draft",
        )
        .unwrap();

        let ed = Editor::new(Config {
            max_file_size: 4,
            ..Config::default()
        });
        let files = vec![FileArg {
            path: path.clone(),
            line: None,
        }];
        let ed = open_files(ed, None, &files);

        // The open is refused: no buffer to recover into, the notice says why, and the WAL stays.
        assert!(
            ed.buffers()
                .find_by_path(ed.resolve_path(path.to_str().unwrap()).as_str())
                .is_none()
        );
        assert!(ed.message().starts_with("File too large"));
        assert!(wal::has_wal(&path));
    }

    #[test]
    fn wal_recovery_restores_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eol.txt");
        fs::write(&path, "a\nb\n").unwrap();
        let disk = crate::shell::file::disk_state(&path);
        wal::write_wal(&path, disk, Eol::Crlf, b"a\nb\nc\n").unwrap();

        let files = vec![FileArg {
            path: path.clone(),
            line: None,
        }];
        let ed = open_files(Editor::new(Config::default()), None, &files);

        assert_eq!(ed.message(), "[Recovered eol.txt from WAL]");
        let buf = ed
            .buffers()
            .find_by_path(ed.resolve_path(path.to_str().unwrap()).as_str())
            .unwrap();
        assert_eq!(buf.eol(), Eol::Crlf);
        assert_eq!(*buf.text(), crate::core::Doc::from_bytes(b"a\nb\nc\n"));
    }
}

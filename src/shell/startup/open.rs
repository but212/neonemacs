//! Opens the startup files. Each one is read like `find-file` does (§12).

use super::args::FileArg;
use crate::core::{Buffer, Editor, Mode, request_open, switch_to};
use crate::shell::perform_all_with_recovery_dir;
use std::path::Path;

/// Opens every file in the background. The first one that opens is shown. A `+n` sets its starting line.
///
/// The notices of all files follow the earlier message (for example an `init.lua` error), joined
/// by `; ` (I-56).
pub fn open_files(ed: Editor, lua: Option<&mlua::Lua>, files: &[FileArg]) -> Editor {
    open_files_with_recovery_dir(ed, lua, files, None)
}

/// Like [`open_files`], but uses `rec_dir` when probing for orphan `.rec` files.
pub fn open_files_with_recovery_dir(
    mut ed: Editor,
    lua: Option<&mlua::Lua>,
    files: &[FileArg],
    rec_dir: Option<&Path>,
) -> Editor {
    let first_new = ed.buffers().len();
    let before = ed.message().to_owned();
    let mut notes = Vec::new();

    for f in files {
        ed = ed.clear_message();
        ed = if let Some(raw_path) = f.path.to_str() {
            let path = ed.resolve_path(raw_path);
            let fx = request_open(&ed, path.as_str(), f.line, false, Vec::new());
            perform_all_with_recovery_dir(ed, lua, fx, rec_dir)
        } else {
            ed.append_message(format!(
                "Cannot open {}: path is not valid UTF-8",
                f.path.display()
            ))
        };
        let is_confirm_prompt =
            matches!(ed.mode(), Mode::Confirm { text, .. } if text == ed.message());
        if !ed.message().is_empty() && !is_confirm_prompt {
            notes.push(ed.message().to_owned());
        }
    }

    let confirm_text = match ed.mode() {
        Mode::Confirm { text, .. } => Some(text.clone()),
        _ => None,
    };
    ed = ed.set_message(before);
    for note in notes {
        ed = ed.append_message(note);
    }
    if let Some(text) = confirm_text {
        ed = ed.append_message(text);
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
    use crate::shell::startup::files_of;
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
    fn recovery_prompt_preserves_prior_message_and_later_open_errors() {
        use crate::core::{BufferId, Doc, Eol, FilePath};
        use crate::shell::recovery::{RecoverySnapshot, write_recovery_file};

        let dir = tempfile::tempdir().unwrap();
        let rec_dir = dir.path().join("rec");
        fs::create_dir_all(&rec_dir).unwrap();

        let good = dir.path().join("a.txt");
        fs::write(&good, b"disk").unwrap();
        let bad_dir = dir.path().join("subdir");
        fs::create_dir_all(&bad_dir).unwrap();

        let resolved = FilePath::resolve("", good.to_str().unwrap());
        write_recovery_file(
            &rec_dir,
            std::process::id().wrapping_add(1),
            &RecoverySnapshot {
                buffer_id: BufferId(1),
                path: Some(resolved.as_str().to_owned()),
                name: "a.txt".to_owned(),
                doc: Doc::from_bytes(b"recovered"),
                eol: Eol::Lf,
            },
        )
        .unwrap();

        let files = vec![
            FileArg {
                path: good,
                line: None,
            },
            FileArg {
                path: bad_dir,
                line: None,
            },
        ];
        let ed = Editor::new(Config::default()).set_message("init.lua: boom");
        let ed = open_files_with_recovery_dir(ed, None, &files, Some(&rec_dir));
        assert!(matches!(ed.mode(), Mode::Confirm { .. }));
        assert!(ed.message().contains("init.lua: boom"));
        assert!(ed.message().contains("Cannot open"));
        assert!(ed.message().contains("Recover a.txt from crash backup? (y or n)"));
    }
}

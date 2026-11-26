//! Opens the startup files. Each one is read like `find-file` does (§12).

use super::args::FileArg;
use crate::core::Buffer;
use crate::core::Editor;
use crate::core::{request_open, switch_to};
use crate::shell::perform_all;

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
}

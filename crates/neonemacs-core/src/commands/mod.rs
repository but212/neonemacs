//! Built-in commands by name (§4). Lua commands join this table through `lua.rs`.
//! Every command takes the editor and its effects by value and returns both.

pub mod basic;
pub mod file;
pub mod load;
pub mod prompt;
pub mod search;
pub mod shell_output;
pub mod wal;

pub use basic::switch_to;
pub use file::{confirmed, disk_checked, written};
pub use load::{file_read, request_open, wal_probed};
pub use prompt::prompt_key;
pub use search::{isearch_key, query_key};
pub use shell_output::{init_loaded, lua_done, record_init_error, shell_done};
pub use wal::{idle, wal_flushed};

use crate::commands::basic::{
    backward_char, delete_backward_char, delete_char, delete_other_windows, forward_char,
    kill_region, kill_ring_save, move_beginning_of_line, move_end_of_line, next_line, other_window,
    previous_line, recenter, set_mark_command, split, view_mode, yank, yank_pop,
};
use crate::commands::file::{save_buffer, save_buffers_kill_emacs, toggle_eol};
use crate::commands::load::revert_buffer;
use crate::commands::prompt::prompt;
use crate::commands::search::start_isearch;
use crate::editor::{Dir, Direction, Editor, MatchKind, Purpose};
use crate::effect::{Effect, Effects};

/// Function signature of a built-in command.
pub type BuiltinFn = fn(Editor, Effects) -> (Editor, Effects);

/// Every built-in command, by name in alphabetical order (§4).
/// Kept sorted so lookup is O(log N) via binary search.
pub const BUILTINS: &[(&str, BuiltinFn)] = &[
    ("backward-char", backward_char),
    ("delete-backward-char", delete_backward_char),
    ("delete-char", delete_char),
    ("delete-other-windows", delete_other_windows),
    ("execute-extended-command", |ed, fx| {
        (prompt(ed, "M-x ", Purpose::ExecuteCommand), fx)
    }),
    ("find-file", |ed, fx| {
        (prompt(ed, "Find file: ", Purpose::FindFile), fx)
    }),
    ("forward-char", forward_char),
    ("goto-line", |ed, fx| {
        (prompt(ed, "Goto line: ", Purpose::GotoLine), fx)
    }),
    ("isearch-backward", |ed, fx| {
        (
            start_isearch(ed, Direction::Backward, MatchKind::Literal),
            fx,
        )
    }),
    ("isearch-backward-regexp", |ed, fx| {
        (start_isearch(ed, Direction::Backward, MatchKind::Regex), fx)
    }),
    ("isearch-forward", |ed, fx| {
        (
            start_isearch(ed, Direction::Forward, MatchKind::Literal),
            fx,
        )
    }),
    ("isearch-forward-regexp", |ed, fx| {
        (start_isearch(ed, Direction::Forward, MatchKind::Regex), fx)
    }),
    ("kill-buffer", |ed, fx| {
        (prompt(ed, "Kill buffer: ", Purpose::KillBuffer), fx)
    }),
    ("kill-region", kill_region),
    ("kill-ring-save", kill_ring_save),
    ("move-beginning-of-line", move_beginning_of_line),
    ("move-end-of-line", move_end_of_line),
    ("next-line", next_line),
    ("other-window", other_window),
    ("previous-line", previous_line),
    ("query-replace", |ed, fx| {
        (
            prompt(
                ed,
                "Query replace: ",
                Purpose::QueryFrom {
                    kind: MatchKind::Literal,
                },
            ),
            fx,
        )
    }),
    ("query-replace-regexp", |ed, fx| {
        (
            prompt(
                ed,
                "Query replace regexp: ",
                Purpose::QueryFrom {
                    kind: MatchKind::Regex,
                },
            ),
            fx,
        )
    }),
    ("recenter", recenter),
    ("reload-init", |ed, mut fx| {
        fx.push(Effect::ReloadInit);
        (ed, fx)
    }),
    ("revert-buffer", revert_buffer),
    ("save-buffer", save_buffer),
    ("save-buffers-kill-emacs", save_buffers_kill_emacs),
    ("set-mark-command", set_mark_command),
    ("shell-command", |ed, fx| {
        (prompt(ed, "Shell command: ", Purpose::ShellCommand), fx)
    }),
    ("split-window-below", |ed, fx| (split(ed, Dir::Below), fx)),
    ("split-window-right", |ed, fx| (split(ed, Dir::Right), fx)),
    ("switch-to-buffer", |ed, fx| {
        (prompt(ed, "Switch to buffer: ", Purpose::SwitchBuffer), fx)
    }),
    ("toggle-eol", toggle_eol),
    ("undo", Editor::undo),
    ("view-mode", view_mode),
    ("write-file", |ed, fx| {
        (prompt(ed, "Write file: ", Purpose::WriteFile), fx)
    }),
    ("yank", yank),
    ("yank-pop", yank_pop),
];

/// Looks up a built-in command by its exact name via binary search.
pub fn find(name: &str) -> Option<BuiltinFn> {
    BUILTINS
        .binary_search_by_key(&name, |(n, _)| *n)
        .ok()
        .map(|idx| BUILTINS[idx].1)
}

/// Whether `name` is a built-in command.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    BUILTINS.binary_search_by_key(&name, |(n, _)| *n).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_strictly_sorted_without_duplicates() {
        for window in BUILTINS.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "BUILTINS out of order or duplicate: '{}' >= '{}'",
                window[0].0,
                window[1].0
            );
        }
    }

    #[test]
    fn find_locates_every_builtin() {
        for (name, _) in BUILTINS {
            assert!(find(name).is_some(), "Failed to find builtin: {name}");
        }
        assert!(find("nonexistent-command").is_none());
    }
}

//! The WAL flush policy: which buffers go to their WALs after an idle second, and what a failed
//! write shows (I-23, I-43). The shell only measures the second and performs the writes.

use crate::editor::{Editor, Mode};
use crate::effect::{Effect, Effects, WalEntry, WalWrite};

/// One idle second after an edit: asks the shell to write every dirty buffer that has a WAL.
/// A dirty buffer without one (the scratch buffer with no slot) lives in memory only, so it is
/// clean as far as the WAL goes.
#[must_use]
pub fn idle(ed: Editor, mut fx: Effects) -> (Editor, Effects) {
    let mut entries = Vec::new();
    let mut memory_only = Vec::new();
    for buf in ed.buffers().iter().filter(|b| b.is_wal_dirty()) {
        match ed.wal_target(buf.id()) {
            Some(target) => entries.push(WalEntry {
                id: buf.id(),
                target,
                disk: buf.backing().disk(),
                eol: buf.eol(),
            }),
            None => memory_only.push(buf.id()),
        }
    }
    let ed = memory_only
        .into_iter()
        .fold(ed, super::super::editor::Editor::clear_wal_dirty);
    if !entries.is_empty() {
        fx.push(Effect::FlushWal { entries });
    }
    (ed, fx)
}

/// Applies the outcomes of a flush. A buffer whose write failed stays dirty, so the next idle
/// second tries again. The failure is shown only in `Mode::Normal`, so a pending question stays
/// visible (I-04).
#[must_use]
pub fn wal_flushed(ed: Editor, writes: Vec<WalWrite>) -> Editor {
    let mut failures = Vec::new();
    let mut ed = ed;
    for write in writes {
        match write.result {
            Ok(()) => ed = ed.clear_wal_dirty(write.id),
            Err(e) => failures.push(format!("{}: {e}", write.target.path().display())),
        }
    }
    match failures.split_first() {
        Some((first, rest)) if matches!(ed.mode(), Mode::Normal { .. }) => {
            let more = if rest.is_empty() {
                String::new()
            } else {
                format!(" (+{} more)", rest.len())
            };
            ed.set_message(format!("WAL write failed: {first}{more}"))
        }
        _ => ed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::WalTarget;
    use crate::eol::Eol;
    use crate::file::DiskState;
    use crate::test_support::{editor, keys, keys_with_effects};
    use std::path::PathBuf;

    fn slot() -> PathBuf {
        PathBuf::from("state/scratch.wal")
    }

    fn typed_scratch(slot: Option<PathBuf>) -> Editor {
        keys(editor().with_scratch_slot(slot), "x")
    }

    #[test]
    fn idle_with_nothing_dirty_asks_for_nothing() {
        let (ed, fx) = idle(editor(), Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        assert!(!ed.buffer().is_wal_dirty());
    }

    #[test]
    fn idle_flushes_the_dirty_scratch_to_its_slot() {
        let ed = typed_scratch(Some(slot()));
        assert!(ed.buffer().is_wal_dirty());
        let id = ed.buffer().id();
        let (ed, fx) = idle(ed, Vec::new());
        assert_eq!(
            fx,
            vec![Effect::FlushWal {
                entries: vec![WalEntry {
                    id,
                    target: WalTarget::Slot(slot()),
                    disk: DiskState::Unknown,
                    eol: Eol::Lf,
                }]
            }]
        );
        // Still dirty: only `WalFlushed` says the write happened.
        assert!(ed.buffer().is_wal_dirty());
    }

    #[test]
    fn scratch_without_a_slot_is_clean_after_idle_with_no_effect() {
        let ed = typed_scratch(None);
        assert!(ed.buffer().is_wal_dirty());
        let (ed, fx) = idle(ed, Vec::new());
        assert_eq!(fx, [] as [Effect; 0]);
        assert!(!ed.buffer().is_wal_dirty());
        assert!(ed.buffer().modified());
    }

    fn write(ed: &Editor, result: Result<(), String>) -> WalWrite {
        WalWrite {
            id: ed.buffer().id(),
            target: WalTarget::Slot(slot()),
            result,
        }
    }

    #[test]
    fn a_written_buffer_is_clean_and_says_nothing() {
        let ed = typed_scratch(Some(slot()));
        let ed = wal_flushed(ed.clone(), vec![write(&ed, Ok(()))]);
        assert!(!ed.buffer().is_wal_dirty());
        assert_eq!(ed.message(), "");
    }

    #[test]
    fn a_failed_buffer_stays_dirty_and_is_reported() {
        let ed = typed_scratch(Some(slot()));
        let ed = wal_flushed(ed.clone(), vec![write(&ed, Err("disk full".to_owned()))]);
        assert!(ed.buffer().is_wal_dirty());
        assert_eq!(
            ed.message(),
            format!("WAL write failed: {}: disk full", slot().display())
        );
    }

    #[test]
    fn several_failures_show_the_first_and_count_the_rest() {
        let ed = typed_scratch(Some(slot()));
        let fail = |e: &str| write(&ed, Err(e.to_owned()));
        let ed = wal_flushed(ed.clone(), vec![fail("a"), fail("b"), fail("c")]);
        assert_eq!(
            ed.message(),
            format!("WAL write failed: {}: a (+2 more)", slot().display())
        );
    }

    #[test]
    fn a_pending_question_keeps_its_message() {
        let ed = keys(typed_scratch(Some(slot())), "C-x C-c");
        let before = ed.message().to_owned();
        assert!(matches!(ed.mode(), Mode::Confirm(_)));
        let ed = wal_flushed(ed.clone(), vec![write(&ed, Err("disk full".to_owned()))]);
        assert_eq!(ed.message(), before);
    }

    fn remove_slot() -> Effect {
        Effect::RemoveWal {
            target: WalTarget::Slot(slot()),
        }
    }

    #[test]
    fn undoing_the_scratch_back_to_clean_removes_the_slot() {
        let ed = typed_scratch(Some(slot()));
        let (ed, fx) = keys_with_effects(ed, "C-/");
        assert!(!ed.buffer().modified());
        assert_eq!(fx, vec![remove_slot()]);
    }

    #[test]
    fn quitting_with_unsaved_scratch_text_discards_the_slot() {
        let ed = typed_scratch(Some(slot()));
        let (ed, fx) = keys_with_effects(ed, "C-x C-c y");
        assert!(ed.quit());
        assert_eq!(fx, vec![remove_slot()]);
    }

    #[test]
    fn a_scratch_without_a_slot_has_nothing_to_remove() {
        let ed = typed_scratch(None);
        let (_, fx) = keys_with_effects(ed, "C-/");
        assert_eq!(fx, [] as [Effect; 0]);
    }
}

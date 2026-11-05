//! Window layout and `WindowManager` state (§5).
//! Enforces I-19: selected < `layout.leaf_count()` at all times.

use crate::core::buffer::BufferId;
use crate::core::text::Text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Below,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub buffer: BufferId,
    pub point: usize,
    pub top: usize,
    pub left_col: usize,
    /// Body rows at the last layout. 0 until the first render.
    pub height: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Leaf(Window),
    Split {
        dir: Dir,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl Layout {
    /// Every window, in layout order.
    #[must_use]
    pub fn windows(&self) -> Vec<&Window> {
        match self {
            Self::Leaf(w) => vec![w],
            Self::Split { first, second, .. } => {
                let mut out = first.windows();
                out.extend(second.windows());
                out
            }
        }
    }

    #[must_use]
    pub const fn leaf_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// The window at layout index `i`.
    ///
    /// # Panics
    /// Panics if `i >= self.leaf_count()` (violating I-19).
    #[must_use]
    pub fn leaf(&self, i: usize) -> &Window {
        match self {
            Self::Leaf(w) if i == 0 => w,
            Self::Leaf(_) => panic!("window index out of range (I-19)"),
            Self::Split { first, second, .. } => {
                let k = first.leaf_count();
                if i < k {
                    first.leaf(i)
                } else {
                    second.leaf(i - k)
                }
            }
        }
    }

    /// Replaces the window at layout index `i` by the layout `f` builds from it.
    ///
    /// # Panics
    /// Panics if `i >= self.leaf_count()` (violating I-19).
    #[must_use]
    pub fn replace_leaf(self, i: usize, f: impl FnOnce(Window) -> Self) -> Self {
        match self {
            Self::Leaf(w) if i == 0 => f(w),
            Self::Leaf(_) => panic!("window index out of range (I-19)"),
            Self::Split { dir, first, second } => {
                let k = first.leaf_count();
                if i < k {
                    Self::Split {
                        dir,
                        first: Box::new((*first).replace_leaf(i, f)),
                        second,
                    }
                } else {
                    Self::Split {
                        dir,
                        first,
                        second: Box::new((*second).replace_leaf(i - k, f)),
                    }
                }
            }
        }
    }

    /// Replaces the window at layout index `i`. Panics when `i` is out of range (I-19).
    #[must_use]
    pub fn set_leaf(self, i: usize, w: Window) -> Self {
        self.replace_leaf(i, |_| Self::Leaf(w))
    }

    /// Applies `f` to every window. Edits use this to move anchors (I-01).
    #[must_use]
    pub fn map_windows(self, f: &impl Fn(Window) -> Window) -> Self {
        match self {
            Self::Leaf(w) => Self::Leaf(f(w)),
            Self::Split { dir, first, second } => Self::Split {
                dir,
                first: Box::new((*first).map_windows(f)),
                second: Box::new((*second).map_windows(f)),
            },
        }
    }
}

/// Moves an anchor that landed inside a unit back to that unit's start.
#[must_use]
pub fn snap_point(text: &Text, a: usize) -> usize {
    let a = a.min(text.len());
    if text.is_boundary(a) {
        a
    } else {
        text.prev_boundary((a + 1).min(text.len()))
    }
}

/// Encapsulates layout tree and selected focus, maintaining I-19 invariant:
/// `selected < layout.leaf_count()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowManager {
    layout: Layout,
    selected: usize,
}

impl WindowManager {
    #[must_use]
    pub const fn new(initial: Window) -> Self {
        Self {
            layout: Layout::Leaf(initial),
            selected: 0,
        }
    }

    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    #[must_use]
    pub const fn leaf_count(&self) -> usize {
        self.layout.leaf_count()
    }

    #[must_use]
    pub fn current(&self) -> &Window {
        self.layout.leaf(self.selected)
    }

    #[must_use]
    pub fn windows(&self) -> Vec<&Window> {
        self.layout.windows()
    }

    /// Sets the selected window index.
    ///
    /// # Panics
    /// Panics if `index >= self.layout.leaf_count()` (violating I-19).
    #[must_use]
    pub fn set_selected(mut self, index: usize) -> Self {
        assert!(
            index < self.layout.leaf_count(),
            "window index out of range (I-19)"
        );
        self.selected = index;
        self
    }

    #[must_use]
    pub const fn next_window(mut self) -> Self {
        self.selected = (self.selected + 1) % self.layout.leaf_count();
        self
    }

    #[must_use]
    pub fn split(mut self, dir: Dir) -> Self {
        let i = self.selected;
        self.layout = self.layout.replace_leaf(i, |w| Layout::Split {
            dir,
            first: Box::new(Layout::Leaf(w)),
            second: Box::new(Layout::Leaf(w)),
        });
        self
    }

    #[must_use]
    pub fn set_leaf(mut self, i: usize, w: Window) -> Self {
        self.layout = self.layout.set_leaf(i, w);
        self
    }

    #[must_use]
    pub fn delete_other_windows(mut self) -> Self {
        let w = *self.current();
        self.layout = Layout::Leaf(w);
        self.selected = 0;
        self
    }

    #[must_use]
    pub fn retarget(mut self, from: BufferId, to: BufferId, saved_point: usize) -> Self {
        self.layout = self.layout.map_windows(&|w| {
            if w.buffer == from {
                Window {
                    buffer: to,
                    point: saved_point,
                    top: 0,
                    ..w
                }
            } else {
                w
            }
        });
        self
    }

    #[must_use]
    pub fn map_windows(mut self, f: &impl Fn(Window) -> Window) -> Self {
        self.layout = self.layout.map_windows(f);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_manager_initial_state_satisfies_i19() {
        let wm = WindowManager::new(Window {
            buffer: BufferId(0),
            point: 0,
            top: 0,
            left_col: 0,
            height: 0,
        });
        assert_eq!(wm.leaf_count(), 1);
        assert_eq!(wm.selected(), 0);
        assert!(wm.selected() < wm.leaf_count());
    }

    #[test]
    fn window_manager_split_and_cycle_satisfies_i19() {
        let wm = WindowManager::new(Window {
            buffer: BufferId(0),
            point: 0,
            top: 0,
            left_col: 0,
            height: 0,
        });
        let wm = wm.split(Dir::Below);
        assert_eq!(wm.leaf_count(), 2);
        assert_eq!(wm.selected(), 0);

        let wm = wm.next_window();
        assert_eq!(wm.selected(), 1);

        let wm = wm.next_window();
        assert_eq!(wm.selected(), 0);

        let wm = wm.delete_other_windows();
        assert_eq!(wm.leaf_count(), 1);
        assert_eq!(wm.selected(), 0);
    }

    #[test]
    #[should_panic(expected = "I-19")]
    fn window_manager_out_of_bounds_selected_panics() {
        let wm = WindowManager::new(Window {
            buffer: BufferId(0),
            point: 0,
            top: 0,
            left_col: 0,
            height: 0,
        });
        let _ = wm.set_selected(5);
    }
}

//! Window layout and WindowManager state (§5).
//! Enforces I-19: selected < layout.leaf_count() at all times.

use crate::core::buffer::BufferId;
use crate::core::text::Text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Below,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub buffer: BufferId,
    pub point: usize,
    pub top: usize,
    pub left_col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Leaf(Window),
    Split {
        dir: Dir,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// Every window, in layout order.
    pub fn windows(&self) -> Vec<&Window> {
        match self {
            Layout::Leaf(w) => vec![w],
            Layout::Split { first, second, .. } => {
                let mut out = first.windows();
                out.extend(second.windows());
                out
            }
        }
    }

    pub const fn leaf_count(&self) -> usize {
        match self {
            Layout::Leaf(_) => 1,
            Layout::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// The window at layout index `i`. Panics when `i` is out of range (I-19).
    pub fn leaf(&self, i: usize) -> &Window {
        match self {
            Layout::Leaf(w) if i == 0 => w,
            Layout::Leaf(_) => panic!("window index out of range (I-19)"),
            Layout::Split { first, second, .. } => {
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
    /// Panics when `i` is out of range (I-19).
    pub fn replace_leaf(self, i: usize, f: impl FnOnce(Window) -> Layout) -> Layout {
        match self {
            Layout::Leaf(w) if i == 0 => f(w),
            Layout::Leaf(_) => panic!("window index out of range (I-19)"),
            Layout::Split { dir, first, second } => {
                let k = first.leaf_count();
                if i < k {
                    Layout::Split {
                        dir,
                        first: Box::new((*first).replace_leaf(i, f)),
                        second,
                    }
                } else {
                    Layout::Split {
                        dir,
                        first,
                        second: Box::new((*second).replace_leaf(i - k, f)),
                    }
                }
            }
        }
    }

    /// Replaces the window at layout index `i`. Panics when `i` is out of range (I-19).
    pub fn set_leaf(self, i: usize, w: Window) -> Layout {
        self.replace_leaf(i, |_| Layout::Leaf(w))
    }

    /// Applies `f` to every window. Edits use this to move anchors (I-01).
    pub fn map_windows(self, f: &impl Fn(Window) -> Window) -> Layout {
        match self {
            Layout::Leaf(w) => Layout::Leaf(f(w)),
            Layout::Split { dir, first, second } => Layout::Split {
                dir,
                first: Box::new((*first).map_windows(f)),
                second: Box::new((*second).map_windows(f)),
            },
        }
    }
}

/// Moves an anchor that landed inside a unit back to that unit's start.
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
    pub const fn new(initial: Window) -> Self {
        Self {
            layout: Layout::Leaf(initial),
            selected: 0,
        }
    }

    pub fn from_layout(layout: Layout, selected: usize) -> Self {
        assert!(
            selected < layout.leaf_count(),
            "window index out of range (I-19)"
        );
        Self { layout, selected }
    }

    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    pub const fn selected(&self) -> usize {
        self.selected
    }

    pub const fn leaf_count(&self) -> usize {
        self.layout.leaf_count()
    }

    pub fn current(&self) -> &Window {
        self.layout.leaf(self.selected)
    }

    pub fn windows(&self) -> Vec<&Window> {
        self.layout.windows()
    }

    pub fn set_selected(mut self, index: usize) -> Self {
        assert!(
            index < self.layout.leaf_count(),
            "window index out of range (I-19)"
        );
        self.selected = index;
        self
    }

    pub const fn next_window(mut self) -> Self {
        self.selected = (self.selected + 1) % self.layout.leaf_count();
        self
    }

    pub fn with_selected(mut self, f: impl FnOnce(Window) -> Window, text: &Text) -> Self {
        let w = f(self.current().clone());
        let w = Window {
            point: snap_point(text, w.point),
            top: snap_point(text, w.top),
            ..w
        };
        self.layout = self.layout.set_leaf(self.selected, w);
        self
    }

    pub fn split(mut self, dir: Dir) -> Self {
        let i = self.selected;
        self.layout = self.layout.replace_leaf(i, |w| Layout::Split {
            dir,
            first: Box::new(Layout::Leaf(w.clone())),
            second: Box::new(Layout::Leaf(w)),
        });
        self
    }

    pub fn set_leaf(mut self, i: usize, w: Window) -> Self {
        self.layout = self.layout.set_leaf(i, w);
        self
    }

    pub fn delete_other_windows(mut self) -> Self {
        let w = self.current().clone();
        self.layout = Layout::Leaf(w);
        self.selected = 0;
        self
    }

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

    pub fn map_windows(mut self, f: &impl Fn(Window) -> Window) -> Self {
        self.layout = self.layout.map_windows(f);
        self
    }

    pub fn replace_layout(self, layout: Layout, selected: usize) -> Self {
        Self::from_layout(layout, selected)
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
        });
        let _ = wm.set_selected(5);
    }
}

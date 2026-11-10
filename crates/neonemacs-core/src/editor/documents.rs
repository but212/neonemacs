//! Documents group: the buffers, the window layout, the kill ring, and the edit counter (I-32).
//! `edit_seq` belongs here, so `Editor::apply` changes it in the same group it changes buffers
//! and windows (I-31). Fields are visible only to `editor`.

use crate::buffer::BufferPool;
use crate::kill_ring::KillRing;
use crate::window::{Window, WindowManager};

#[derive(Debug, Clone)]
pub struct Documents {
    pub(super) buffers: BufferPool,
    pub(super) windows: WindowManager,
    pub(super) kill_ring: KillRing,
    pub(super) edit_seq: u64,
}

impl Documents {
    pub fn new(initial_window: Window) -> Self {
        Self {
            buffers: BufferPool::new(),
            windows: WindowManager::new(initial_window),
            kill_ring: KillRing::new(),
            edit_seq: 0,
        }
    }
}

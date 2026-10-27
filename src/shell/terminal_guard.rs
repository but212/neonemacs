//! Raw mode and the alternate screen, restored on drop or panic (§13).

use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use std::io;
use std::sync::Once;

/// Puts the terminal back as it was. Safe to call more than once.
fn restore_terminal() {
    let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// Restores the terminal when dropped, including on early return or panic (§13).
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        Self::install_panic_hook();
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }

    fn install_panic_hook() {
        static PANIC_HOOK: Once = Once::new();
        PANIC_HOOK.call_once(|| {
            let default = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal();
                default(info);
            }));
        });
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

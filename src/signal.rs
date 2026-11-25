//! SIGINT/SIGTERM cleanup (DESIGN §5.1).
//!
//! The handler never exits the process itself: it flips a flag and kills every
//! live ssh process group, then the main thread (or TUI loop) restores the
//! terminal and exits normally.

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

static INSTALL: Once = Once::new();
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn install() {
    INSTALL.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            INTERRUPTED.store(true, Ordering::SeqCst);
            crate::probe::ssh::cancel_all();
        });
    });
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    #[test]
    fn not_interrupted_by_default() {
        assert!(!super::interrupted());
    }
}

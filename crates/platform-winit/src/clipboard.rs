//! System clipboard access (arboard). Created lazily on first use —
//! constructing it spawns platform helpers on some backends, so an app
//! that never copies never pays for it. UI thread only.

use std::cell::RefCell;

thread_local! {
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<R>) -> Option<R> {
    CLIPBOARD.with(|c| {
        let mut guard = c.borrow_mut();
        if guard.is_none() {
            *guard = arboard::Clipboard::new().ok();
        }
        guard.as_mut().and_then(f)
    })
}

/// The system clipboard behind the `craie_ui` clipboard seam.
pub struct SystemClipboard;

impl craie_ui::clipboard::Clipboard for SystemClipboard {
    fn get(&mut self) -> Option<String> {
        with(|c| c.get_text().ok())
    }

    fn set(&mut self, text: &str) {
        with(|c| c.set_text(text).ok());
    }
}

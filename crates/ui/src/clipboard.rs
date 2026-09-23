//! Clipboard seam. Editing code copies and pastes through this trait;
//! the platform adapter installs the system clipboard. The default is an
//! in-process buffer, which is what tests and headless hosts get.

pub trait Clipboard {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, text: &str);
}

/// In-process clipboard: a single string.
#[derive(Default)]
pub struct MemoryClipboard(Option<String>);

impl Clipboard for MemoryClipboard {
    fn get(&mut self) -> Option<String> {
        self.0.clone()
    }

    fn set(&mut self, text: &str) {
        self.0 = Some(text.to_string());
    }
}

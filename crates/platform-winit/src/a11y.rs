//! AccessKit action handler: queues assistive-technology requests for
//! the UI thread and wakes the event loop to drain them.

use std::sync::Arc;

use accesskit::{ActionHandler, ActionRequest};
use craie_ui::a11y::A11yShared;

use crate::Wake;

/// Queues action requests for the UI thread and pokes the event loop.
pub struct ActionSink {
    pub shared: Arc<A11yShared>,
    pub wake: Wake,
}

impl ActionHandler for ActionSink {
    fn do_action(&mut self, request: ActionRequest) {
        self.shared.actions.lock().unwrap().push(request);
        self.wake.wake();
    }
}

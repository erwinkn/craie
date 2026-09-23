//! Revisions: monotonically increasing stamps that prove a derived
//! value is current. A derived copy stores the revision it was built
//! from; it is valid while that revision is unchanged.

/// A revision stamp. `Rev::ZERO` is "never built".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rev(pub u64);

impl Rev {
    pub const ZERO: Rev = Rev(0);

    /// Advances the stamp and returns the new value.
    pub fn bump(&mut self) -> Rev {
        self.0 += 1;
        *self
    }
}

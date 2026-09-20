/// Interned style handle. Style *storage* lands with the Taffy milestone:
/// the plan is a compact common style + side tables for sparse fields,
/// not `taffy::Style` per node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct StyleId(pub u32);

impl StyleId {
    pub const NIL: StyleId = StyleId(u32::MAX);
}

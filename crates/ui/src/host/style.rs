/// Style handle — the dense id JS already assigns on the wire. The wire
/// id IS the index into `Layouts::styles`; no second interning pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct StyleId(pub u32);

impl StyleId {
    pub const NIL: StyleId = StyleId(u32::MAX);
}

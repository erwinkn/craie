//! Custom elements: the escape hatch for content the built-in node kinds
//! don't express. A `CUSTOM` node is a real retained node — it lays out,
//! hit-tests, scrolls, and dispatches events like any other — but its
//! paint comes from a registered painter instead of the built-in quad
//! path.
//!
//! Painters are registered per tag on `Ui` (`Ui::register_painter`) and
//! emit a list of logical-space quads per node per paint. `Ui` scales
//! them to physical pixels and stamps the node's clip. The N-API runtime
//! wraps JS callbacks into this signature; a Rust host crate can register
//! closures directly.

use crate::geom::Rect;

/// Wire payload for one custom node (CUSTOM op): a painter tag plus a
/// small fixed payload — enough for sparklines, meters, charts. Larger
/// data belongs in `text` or a JS-side projection.
#[derive(Clone, Debug, Default)]
pub struct CustomData {
    /// Which registered painter renders this node.
    pub tag: u32,
    /// Author-defined floats.
    pub data: [f32; 4],
    /// Author-defined string payload.
    pub text: String,
}

/// A logical-space filled rect a painter emits. Maps 1:1 onto
/// `Instance::rect` after scaling and clip stamping.
#[derive(Clone, Copy, Debug, Default)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Fill, 0xRRGGBBAA.
    pub color: u32,
    /// Corner radius, logical points.
    pub radius: f32,
    /// Border ring width, logical points.
    pub border_w: f32,
    /// Border ring color, 0xRRGGBBAA.
    pub border_color: u32,
}

/// Renders one custom node: payload + logical content rect in, quads out.
/// Called on the UI thread during paint — keep it cheap.
pub type Painter = Box<dyn FnMut(&CustomData, Rect, &mut Vec<Quad>)>;

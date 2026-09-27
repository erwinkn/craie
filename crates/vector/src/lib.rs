//! Vector paths and paints (ARCHITECTURE.md section 9).
//!
//! A `Path` is a list of subpaths of lines and quadratic and cubic
//! Béziers. `fill` and `stroke` tessellate it with lyon into a `Mesh`
//! (triangles in the path's units) at a tolerance the caller picks from
//! the display scale: this is the `PathRecord` boundary, behind which a
//! coverage preparation may replace tessellation (E07). The scene draws
//! meshes; it never sees lyon. `dashed` cuts a path into a stroke's
//! dashes before it strokes; `svg` parses runtime SVG attribute strings.

use craie_core::geom::Affine;
use lyon_tessellation::geom::{CubicBezierSegment, LineSegment, QuadraticBezierSegment, point};
use lyon_tessellation::path::Path as LyonPath;
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, StrokeOptions, StrokeTessellator,
    StrokeVertex, VertexBuffers,
};

pub mod asset;
pub mod svg;

/// One path command. Points are in the path's own units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verb {
    MoveTo([f32; 2]),
    LineTo([f32; 2]),
    /// Control point, end point.
    QuadTo([f32; 2], [f32; 2]),
    /// Two control points, end point.
    CubicTo([f32; 2], [f32; 2], [f32; 2]),
    Close,
}

/// A path: subpaths each start with `MoveTo`; drawing commands before
/// any `MoveTo` start at the origin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub verbs: Vec<Verb>,
}

impl Path {
    pub fn new() -> Path {
        Path::default()
    }

    pub fn move_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.verbs.push(Verb::MoveTo([x, y]));
        self
    }

    pub fn line_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.verbs.push(Verb::LineTo([x, y]));
        self
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) -> &mut Self {
        self.verbs.push(Verb::QuadTo([cx, cy], [x, y]));
        self
    }

    pub fn cubic_to(&mut self, c1: [f32; 2], c2: [f32; 2], to: [f32; 2]) -> &mut Self {
        self.verbs.push(Verb::CubicTo(c1, c2, to));
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.verbs.push(Verb::Close);
        self
    }

    /// An axis-aligned rectangle.
    pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Path {
        let mut p = Path::new();
        p.move_to(x, y)
            .line_to(x + w, y)
            .line_to(x + w, y + h)
            .line_to(x, y + h)
            .close();
        p
    }

    /// A circle of four cubic arcs.
    pub fn circle(cx: f32, cy: f32, r: f32) -> Path {
        // Control distance for a quarter circle.
        let k = 0.552_284_8 * r;
        let mut p = Path::new();
        p.move_to(cx + r, cy)
            .cubic_to([cx + r, cy + k], [cx + k, cy + r], [cx, cy + r])
            .cubic_to([cx - k, cy + r], [cx - r, cy + k], [cx - r, cy])
            .cubic_to([cx - r, cy - k], [cx - k, cy - r], [cx, cy - r])
            .cubic_to([cx + k, cy - r], [cx + r, cy - k], [cx + r, cy])
            .close();
        p
    }

    /// The same path with every subpath's start explicit: drawing that
    /// begins before any `MoveTo` starts at the origin, and after a
    /// `Close` at the closed subpath's start (so a transform maps those
    /// points too).
    pub fn explicit(&self) -> Path {
        let mut out = Vec::with_capacity(self.verbs.len() + 1);
        let (mut open, mut at, mut start) = (false, [0.0f32; 2], [0.0f32; 2]);
        for &v in &self.verbs {
            let end = match v {
                Verb::MoveTo(p) => {
                    open = true;
                    at = p;
                    start = p;
                    out.push(v);
                    continue;
                }
                Verb::Close => {
                    out.push(v);
                    open = false;
                    at = start;
                    continue;
                }
                Verb::LineTo(p) | Verb::QuadTo(_, p) | Verb::CubicTo(_, _, p) => p,
            };
            if !open {
                out.push(Verb::MoveTo(at));
                start = at;
                open = true;
            }
            out.push(v);
            at = end;
        }
        Path { verbs: out }
    }

    /// The path with every point mapped by `m` (implicit subpath starts
    /// made explicit first).
    pub fn transformed(&self, m: &Affine) -> Path {
        let f = |[x, y]: [f32; 2]| {
            let p = m.apply(craie_core::geom::Point::new(x, y));
            [p.x, p.y]
        };
        Path {
            verbs: self
                .explicit()
                .verbs
                .iter()
                .map(|v| match *v {
                    Verb::MoveTo(p) => Verb::MoveTo(f(p)),
                    Verb::LineTo(p) => Verb::LineTo(f(p)),
                    Verb::QuadTo(c, p) => Verb::QuadTo(f(c), f(p)),
                    Verb::CubicTo(a, b, p) => Verb::CubicTo(f(a), f(b), f(p)),
                    Verb::Close => Verb::Close,
                })
                .collect(),
        }
    }

    /// Whether every coordinate is finite (tessellation needs it).
    pub fn is_finite(&self) -> bool {
        self.verbs.iter().all(|v| {
            let pts: &[[f32; 2]] = match v {
                Verb::MoveTo(p) | Verb::LineTo(p) => std::slice::from_ref(p),
                Verb::QuadTo(c, p) => &[*c, *p],
                Verb::CubicTo(a, b, p) => &[*a, *b, *p],
                Verb::Close => &[],
            };
            pts.iter().all(|p| p[0].is_finite() && p[1].is_finite())
        })
    }

    fn lyon(&self) -> LyonPath {
        let mut b = LyonPath::builder();
        let mut open = false;
        let mut at = [0.0f32; 2];
        let mut start = at;
        for v in &self.verbs {
            let ensure =
                |b: &mut lyon_tessellation::path::path::Builder, open: &mut bool, at: [f32; 2]| {
                    if !*open {
                        b.begin(point(at[0], at[1]));
                        *open = true;
                    }
                };
            match *v {
                Verb::MoveTo(p) => {
                    if open {
                        b.end(false);
                    }
                    b.begin(point(p[0], p[1]));
                    open = true;
                    at = p;
                    start = p;
                }
                Verb::LineTo(p) => {
                    ensure(&mut b, &mut open, at);
                    b.line_to(point(p[0], p[1]));
                    at = p;
                }
                Verb::QuadTo(c, p) => {
                    ensure(&mut b, &mut open, at);
                    b.quadratic_bezier_to(point(c[0], c[1]), point(p[0], p[1]));
                    at = p;
                }
                Verb::CubicTo(c1, c2, p) => {
                    ensure(&mut b, &mut open, at);
                    b.cubic_bezier_to(point(c1[0], c1[1]), point(c2[0], c2[1]), point(p[0], p[1]));
                    at = p;
                }
                Verb::Close => {
                    if open {
                        b.end(true);
                        open = false;
                        at = start;
                    }
                }
            }
        }
        if open {
            b.end(false);
        }
        b.build()
    }
}

/// How a fill decides inside (SVG `fill-rule`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

impl FillRule {
    /// The wire's and the asset's value (declaration order).
    pub fn from_u8(v: u8) -> Option<FillRule> {
        Some(match v {
            0 => FillRule::NonZero,
            1 => FillRule::EvenOdd,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

impl LineJoin {
    /// The wire's and the asset's value (declaration order).
    pub fn from_u8(v: u8) -> Option<LineJoin> {
        Some(match v {
            0 => LineJoin::Miter,
            1 => LineJoin::Round,
            2 => LineJoin::Bevel,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

impl LineCap {
    /// The wire's and the asset's value (declaration order).
    pub fn from_u8(v: u8) -> Option<LineCap> {
        Some(match v {
            0 => LineCap::Butt,
            1 => LineCap::Round,
            2 => LineCap::Square,
            _ => return None,
        })
    }
}

/// A stroke's geometry (SVG defaults: miter joins, limit 4, butt caps).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub width: f32,
    pub join: LineJoin,
    pub cap: LineCap,
    pub miter_limit: f32,
}

impl Default for Stroke {
    fn default() -> Stroke {
        Stroke {
            width: 1.0,
            join: LineJoin::Miter,
            cap: LineCap::Butt,
            miter_limit: 4.0,
        }
    }
}

/// A stroke's dash pattern (SVG `stroke-dasharray` and
/// `stroke-dashoffset`): alternating on and off lengths, in path units.
#[derive(Clone, Debug, PartialEq)]
pub struct Dash {
    pub array: Vec<f32>,
    pub offset: f32,
}

impl Dash {
    /// The pattern for the path drawn `k` times larger.
    pub fn scaled(&self, k: f32) -> Dash {
        Dash {
            array: self.array.iter().map(|v| v * k).collect(),
            offset: self.offset * k,
        }
    }
}

/// Dashes one path may be cut into; past it, the stroke draws solid.
pub const MAX_DASH_SEGMENTS: usize = 1 << 16;

/// The dashes of `path` as open subpaths of lines: curves flattened to
/// within `tolerance`, then cut by the pattern. Each subpath restarts
/// the pattern, as in SVG. `None` when the pattern is empty, not finite,
/// or would cut more than `MAX_DASH_SEGMENTS` dashes (bounded before
/// any cutting by the control polygon's length): stroke solid then.
pub fn dashed(path: &Path, dash: &Dash, tolerance: f32) -> Option<Path> {
    let n = dash.array.len();
    let period: f32 = dash.array.iter().sum();
    let ok = |v: f32| v.is_finite() && v >= 0.0;
    if n == 0 || n % 2 == 1 || !dash.array.iter().all(|&v| ok(v)) {
        return None;
    }
    if !(period > 0.0 && period.is_finite() && dash.offset.is_finite()) {
        return None;
    }
    // The control polygon is at least as long as the path.
    let path = path.explicit();
    let (mut at, mut reach) = ([0.0f32; 2], 0.0f64);
    let dist = |a: [f32; 2], b: [f32; 2]| ((b[0] - a[0]).hypot(b[1] - a[1])) as f64;
    for v in &path.verbs {
        let pts: &[[f32; 2]] = match v {
            Verb::MoveTo(p) => {
                at = *p;
                continue;
            }
            Verb::LineTo(p) => std::slice::from_ref(p),
            Verb::QuadTo(c, p) => &[*c, *p],
            Verb::CubicTo(a, b, p) => &[*a, *b, *p],
            // Closing lines are at most as long as their subpath.
            Verb::Close => continue,
        };
        for &p in pts {
            reach += dist(at, p);
            at = p;
        }
    }
    if !(reach * 2.0 / period as f64 * n as f64 <= MAX_DASH_SEGMENTS as f64) {
        return None;
    }
    // Where the offset puts the pattern: element `i`, `left` of it.
    let mut o = dash.offset.rem_euclid(period);
    let mut first = (0, dash.array[0]);
    for (i, &len) in dash.array.iter().enumerate() {
        if o < len {
            first = (i, len - o);
            break;
        }
        o -= len;
    }
    let mut w = Dasher {
        out: Path::new(),
        array: &dash.array,
        first,
        i: first.0,
        left: first.1,
    };
    let (mut at, mut start) = ([0.0f32; 2], [0.0f32; 2]);
    let tol = tolerance.max(1e-6);
    let pt = |p: [f32; 2]| point(p[0], p[1]);
    for v in &path.verbs {
        match *v {
            Verb::MoveTo(p) => {
                w.restart(p);
                at = p;
                start = p;
            }
            Verb::LineTo(p) => {
                w.line(at, p);
                at = p;
            }
            Verb::QuadTo(c, p) => {
                let q = QuadraticBezierSegment {
                    from: pt(at),
                    ctrl: pt(c),
                    to: pt(p),
                };
                q.for_each_flattened(tol, &mut |s: &LineSegment<f32>| {
                    w.line(s.from.to_array(), s.to.to_array())
                });
                at = p;
            }
            Verb::CubicTo(c1, c2, p) => {
                let c = CubicBezierSegment {
                    from: pt(at),
                    ctrl1: pt(c1),
                    ctrl2: pt(c2),
                    to: pt(p),
                };
                c.for_each_flattened(tol, &mut |s: &LineSegment<f32>| {
                    w.line(s.from.to_array(), s.to.to_array())
                });
                at = p;
            }
            Verb::Close => {
                w.line(at, start);
                at = start;
            }
        }
    }
    Some(w.out)
}

/// Walks segments through a dash pattern.
struct Dasher<'a> {
    out: Path,
    array: &'a [f32],
    /// Where each subpath starts: the element and what is left of it.
    first: (usize, f32),
    i: usize,
    left: f32,
}

impl Dasher<'_> {
    fn on(&self) -> bool {
        self.i % 2 == 0
    }

    fn restart(&mut self, p: [f32; 2]) {
        (self.i, self.left) = self.first;
        if self.on() {
            self.out.move_to(p[0], p[1]);
        }
    }

    fn line(&mut self, a: [f32; 2], b: [f32; 2]) {
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        let mut t = 0.0;
        while len - t > self.left {
            t += self.left;
            let f = t / len;
            let p = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
            if self.on() {
                self.out.line_to(p[0], p[1]);
            } else {
                self.out.move_to(p[0], p[1]);
            }
            self.i = (self.i + 1) % self.array.len();
            self.left = self.array[self.i];
        }
        self.left -= len - t;
        if self.on() {
            self.out.line_to(b[0], b[1]);
        }
    }
}

/// Triangles: `indices` (three per triangle) into `vertices`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl Mesh {
    /// Total triangle area (tests and budgets).
    pub fn area(&self) -> f64 {
        self.indices
            .chunks_exact(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|k| self.vertices[t[k] as usize]);
                let cross = (b[0] - a[0]) as f64 * (c[1] - a[1]) as f64
                    - (b[1] - a[1]) as f64 * (c[0] - a[0]) as f64;
                cross.abs() * 0.5
            })
            .sum()
    }
}

/// Tessellation failed (lyon reports it for degenerate input); the
/// caller draws nothing for that path.
#[derive(Debug)]
pub struct TessError;

/// The fill of `path` as triangles, curves flattened to within
/// `tolerance` (path units).
pub fn fill(path: &Path, rule: FillRule, tolerance: f32) -> Result<Mesh, TessError> {
    if !path.is_finite() {
        return Err(TessError);
    }
    let mut out: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let options = FillOptions::tolerance(tolerance.max(1e-9)).with_fill_rule(match rule {
        FillRule::NonZero => lyon_tessellation::FillRule::NonZero,
        FillRule::EvenOdd => lyon_tessellation::FillRule::EvenOdd,
    });
    FillTessellator::new()
        .tessellate_path(
            &path.lyon(),
            &options,
            &mut BuffersBuilder::new(&mut out, |v: FillVertex| v.position().to_array()),
        )
        .map_err(|_| TessError)?;
    finite_mesh(out)
}

/// A mesh whose vertices are all finite (huge finite input can overflow
/// inside the tessellator).
fn finite_mesh(out: VertexBuffers<[f32; 2], u32>) -> Result<Mesh, TessError> {
    if out
        .vertices
        .iter()
        .any(|v| !(v[0].is_finite() && v[1].is_finite()))
    {
        return Err(TessError);
    }
    Ok(Mesh {
        vertices: out.vertices,
        indices: out.indices,
    })
}

/// The stroke of `path` as triangles.
pub fn stroke(path: &Path, s: &Stroke, tolerance: f32) -> Result<Mesh, TessError> {
    if !path.is_finite() || !(s.width.is_finite() && s.width > 0.0) {
        return Err(TessError);
    }
    let mut out: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let options = StrokeOptions::tolerance(tolerance.max(1e-9))
        .with_line_width(s.width)
        .with_miter_limit(s.miter_limit.max(1.0))
        .with_line_join(match s.join {
            LineJoin::Miter => lyon_tessellation::LineJoin::Miter,
            LineJoin::Round => lyon_tessellation::LineJoin::Round,
            LineJoin::Bevel => lyon_tessellation::LineJoin::Bevel,
        })
        .with_line_cap(match s.cap {
            LineCap::Butt => lyon_tessellation::LineCap::Butt,
            LineCap::Round => lyon_tessellation::LineCap::Round,
            LineCap::Square => lyon_tessellation::LineCap::Square,
        });
    StrokeTessellator::new()
        .tessellate_path(
            &path.lyon(),
            &options,
            &mut BuffersBuilder::new(&mut out, |v: StrokeVertex| v.position().to_array()),
        )
        .map_err(|_| TessError)?;
    finite_mesh(out)
}

/// A gradient's color stops: (offset in [0, 1], 0xRRGGBBAA), ascending.
pub type Stops = Vec<(f32, u32)>;

/// How a path is painted. Gradient geometry is in gradient space;
/// `transform` maps gradient space to path space (SVG
/// `gradientTransform`, with bounding-box units resolved).
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(u32),
    Linear {
        start: [f32; 2],
        end: [f32; 2],
        stops: Stops,
        transform: Affine,
    },
    Radial {
        center: [f32; 2],
        radius: f32,
        stops: Stops,
        transform: Affine,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_cover_their_area() {
        let square = Path::rect(0.0, 0.0, 10.0, 10.0);
        assert!((fill(&square, FillRule::NonZero, 0.1).unwrap().area() - 100.0).abs() < 1e-6);
        // A hole: the inner square wound the same way fills under
        // nonzero, and is a hole under even-odd.
        let mut ring = Path::rect(0.0, 0.0, 10.0, 10.0);
        ring.verbs.extend(Path::rect(3.0, 3.0, 4.0, 4.0).verbs);
        assert!((fill(&ring, FillRule::NonZero, 0.1).unwrap().area() - 100.0).abs() < 1e-6);
        assert!((fill(&ring, FillRule::EvenOdd, 0.1).unwrap().area() - 84.0).abs() < 1e-6);
        // Curves flatten within the tolerance: a circle's area.
        let c = fill(&Path::circle(0.0, 0.0, 50.0), FillRule::NonZero, 0.05).unwrap();
        let exact = std::f64::consts::PI * 2500.0;
        assert!((c.area() - exact).abs() / exact < 0.005, "{}", c.area());
        // Transformed: scaled by 2 in x, the area doubles.
        let wide = square.transformed(&Affine::scale(2.0, 1.0));
        assert!((fill(&wide, FillRule::NonZero, 0.1).unwrap().area() - 200.0).abs() < 1e-6);
    }

    #[test]
    fn strokes_follow_width_joins_and_caps() {
        let mut line = Path::new();
        line.move_to(0.0, 0.0).line_to(10.0, 0.0);
        let s = |cap| Stroke {
            width: 2.0,
            cap,
            ..Stroke::default()
        };
        let area = |cap| stroke(&line, &s(cap), 0.01).unwrap().area();
        assert!((area(LineCap::Butt) - 20.0).abs() < 1e-4);
        // Square caps add half the width at each end.
        assert!((area(LineCap::Square) - 24.0).abs() < 1e-4);
        // Round caps add a circle of the width's diameter, flattened to
        // within the tolerance: it loses less than perimeter x tolerance.
        let round = area(LineCap::Round) - 20.0;
        let pi = std::f64::consts::PI;
        assert!(round < pi && pi - round < 2.0 * pi * 0.01, "{round}");
        // A closed square stroked 2 wide covers the outer square minus
        // the inner one (miter joins).
        let sq = stroke(&Path::rect(0.0, 0.0, 10.0, 10.0), &s(LineCap::Butt), 0.01).unwrap();
        assert!((sq.area() - (144.0 - 64.0)).abs() < 1e-3, "{}", sq.area());
    }

    /// Implicit subpath starts (before any MoveTo; after a Close) move
    /// with a transform (S5B-14).
    #[test]
    fn transforms_move_implicit_starts() {
        let mut p = Path::new();
        p.line_to(10.0, 0.0).line_to(0.0, 10.0).close();
        let t = p.transformed(&Affine::translate(10.0, 10.0));
        let m = fill(&t, FillRule::NonZero, 0.1).unwrap();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for v in &m.vertices {
            lo = [lo[0].min(v[0]), lo[1].min(v[1])];
            hi = [hi[0].max(v[0]), hi[1].max(v[1])];
        }
        assert_eq!((lo, hi), ([10.0, 10.0], [20.0, 20.0]));
        assert!((m.area() - 50.0).abs() < 1e-6);
        // After a Close, the next subpath starts at the closed one's
        // start.
        let mut q = Path::new();
        q.move_to(5.0, 5.0)
            .line_to(6.0, 5.0)
            .close()
            .line_to(5.0, 6.0);
        assert_eq!(q.explicit().verbs[3], Verb::MoveTo([5.0, 5.0]));
    }

    /// Dashes cut a path by length, across curves and closes, starting
    /// where the offset says; each subpath restarts the pattern.
    #[test]
    fn dashes_cut_by_length() {
        let mut line = Path::new();
        line.move_to(0.0, 0.0).line_to(10.0, 0.0);
        let d = |array: &[f32], offset: f32| Dash {
            array: array.to_vec(),
            offset,
        };
        let spans = |p: &Path| {
            let mut out = vec![];
            for v in &p.verbs {
                match *v {
                    Verb::MoveTo(a) => out.push([a[0], a[0]]),
                    Verb::LineTo(b) => out.last_mut().unwrap()[1] = b[0],
                    _ => {}
                }
            }
            out
        };
        let cut = dashed(&line, &d(&[3.0, 1.0], 0.0), 0.1).unwrap();
        assert_eq!(spans(&cut), [[0.0, 3.0], [4.0, 7.0], [8.0, 10.0]]);
        // An offset of 2 starts one unit into the first dash; a negative
        // one wraps.
        let cut = dashed(&line, &d(&[3.0, 1.0], 2.0), 0.1).unwrap();
        assert_eq!(spans(&cut)[0], [0.0, 1.0]);
        let cut = dashed(&line, &d(&[3.0, 1.0], -1.0), 0.1).unwrap();
        assert_eq!(spans(&cut)[0], [1.0, 4.0]);
        // A closed square of perimeter 40, half on: 20 units drawn,
        // stroked 1 wide with butt caps: area 20.
        let sq = Path::rect(0.0, 0.0, 10.0, 10.0);
        let cut = dashed(&sq, &d(&[5.0, 5.0], 0.0), 0.1).unwrap();
        let s = Stroke {
            width: 1.0,
            join: LineJoin::Bevel,
            ..Stroke::default()
        };
        let area = stroke(&cut, &s, 0.01).unwrap().area();
        assert!((area - 20.0).abs() < 1e-3, "{area}");
        // A circle half dashed draws about half its perimeter.
        let circle = Path::circle(0.0, 0.0, 10.0);
        let per = 2.0 * std::f32::consts::PI * 10.0;
        let cut = dashed(&circle, &d(&[per / 8.0, per / 8.0], 0.0), 0.01).unwrap();
        let area = stroke(&cut, &s, 0.01).unwrap().area();
        assert!((area - per as f64 / 2.0).abs() < 0.2, "{area}");
        // Two subpaths each start on a dash.
        let mut two = line.clone();
        two.move_to(0.0, 5.0).line_to(10.0, 5.0);
        let cut = dashed(&two, &d(&[3.0, 1.0], 0.0), 0.1).unwrap();
        assert_eq!(spans(&cut).len(), 6);
        // Bounded: a pattern cutting too many dashes, or an empty one,
        // gives no dashes (the caller strokes solid).
        let mut long = Path::new();
        long.move_to(0.0, 0.0).line_to(1e6, 0.0);
        assert!(dashed(&long, &d(&[0.1, 0.1], 0.0), 0.1).is_none());
        assert!(dashed(&line, &d(&[0.0, 0.0], 0.0), 0.1).is_none());
        assert!(dashed(&line, &d(&[1.0], 0.0), 0.1).is_none());
        assert!(dashed(&line, &d(&[f32::NAN, 1.0], 0.0), 0.1).is_none());
    }

    #[test]
    fn bad_input_fails_cleanly() {
        let mut p = Path::new();
        p.move_to(0.0, 0.0).line_to(f32::NAN, 1.0);
        assert!(fill(&p, FillRule::NonZero, 0.1).is_err());
        assert!(
            stroke(
                &Path::rect(0.0, 0.0, 1.0, 1.0),
                &Stroke {
                    width: 0.0,
                    ..Stroke::default()
                },
                0.1
            )
            .is_err()
        );
        // Finite input that overflows inside the tessellator (S5A-06).
        let mut huge = Path::new();
        huge.move_to(0.0, 3e38).line_to(10.0, 3e38);
        let wide = Stroke {
            width: 2e38,
            ..Stroke::default()
        };
        assert!(stroke(&huge, &wide, 0.1).is_err());
        // Nothing to fill is an empty mesh, not an error.
        assert!(
            fill(&Path::new(), FillRule::NonZero, 0.1)
                .unwrap()
                .indices
                .is_empty()
        );
    }
}

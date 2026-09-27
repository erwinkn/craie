//! Runtime SVG attribute strings (ARCHITECTURE.md section 9): path data
//! (`d`), `points`, the `transform` attribute, `viewBox` and
//! `stroke-dasharray`, parsed into paths, affines and numbers. This is
//! not a document parser: no elements, styles, units, text or
//! `currentColor` (colors arrive resolved).
//!
//! A `Drawing` (a view box and shapes, as the wire carries them) builds
//! the same `Asset` a `CRV1` payload decodes to, so layout, fitting and
//! tessellation serve both. Input is untrusted: every parse is one pass
//! over its string, and a drawing stops at `MAX_VERBS` commands in all.
//! Anything malformed is an error, not a partial drawing (the executor
//! rejects the transaction).

use std::borrow::Cow;

use craie_core::geom::Affine;

use crate::asset::{Asset, Item, ItemStyle};
use crate::{Dash, FillRule, Paint, Path, Stroke};

/// Shapes per drawing.
pub const MAX_SHAPES: usize = 1 << 12;
/// Path commands a drawing's shapes expand to together (an arc counts
/// up to four cubics).
pub const MAX_VERBS: usize = 1 << 20;
/// Numbers in one dash array.
pub const MAX_DASHES: usize = 64;

/// Why a string is not what its attribute takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SvgError(pub &'static str);

/// What a shape's geometry string is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ShapeKind {
    /// Path data (`d`).
    #[default]
    Path = 0,
    /// `points`, open.
    Polyline = 1,
    /// `points`, closed.
    Polygon = 2,
}

impl ShapeKind {
    pub fn from_u8(v: u8) -> Option<ShapeKind> {
        Some(match v {
            0 => ShapeKind::Path,
            1 => ShapeKind::Polyline,
            2 => ShapeKind::Polygon,
            _ => return None,
        })
    }
}

/// One shape: its geometry and SVG presentation attributes. Colors are
/// 0xRRGGBBAA; alpha 0 paints nothing (`none`). Empty strings are
/// absent attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape<'a> {
    pub kind: ShapeKind,
    pub geometry: Cow<'a, str>,
    pub transform: Cow<'a, str>,
    pub fill: u32,
    pub fill_rule: FillRule,
    pub stroke: u32,
    /// Stroke geometry; a width of 0 strokes nothing.
    pub line: Stroke,
    /// `stroke-dasharray` (empty or `none`: solid).
    pub dashes: Cow<'a, str>,
    pub dash_offset: f32,
    /// Multiplies the fill's and the stroke's alpha.
    pub opacity: f32,
}

impl Default for Shape<'_> {
    fn default() -> Self {
        Shape {
            kind: ShapeKind::Path,
            geometry: Cow::Borrowed(""),
            transform: Cow::Borrowed(""),
            fill: 0x0000_00FF,
            fill_rule: FillRule::NonZero,
            stroke: 0,
            line: Stroke::default(),
            dashes: Cow::Borrowed(""),
            dash_offset: 0.0,
            opacity: 1.0,
        }
    }
}

/// A runtime vector drawing: a view box and its shapes in paint order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawing<'a> {
    pub view_box: Cow<'a, str>,
    pub shapes: Vec<Shape<'a>>,
}

impl Drawing<'_> {
    /// Canonical bytes: equal drawings, equal keys (the host shares one
    /// parse and one tessellation between nodes drawing the same thing).
    /// Distinct from any `CRV1` payload (they start with "CRV1").
    pub fn key(&self) -> Vec<u8> {
        let strings = self.view_box.len()
            + self
                .shapes
                .iter()
                .map(|s| s.geometry.len() + s.transform.len() + s.dashes.len())
                .sum::<usize>();
        let mut out = Vec::with_capacity(8 + strings + self.shapes.len() * 48);
        out.extend_from_slice(b"CRVS");
        let put = |out: &mut Vec<u8>, s: &str| {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        };
        put(&mut out, &self.view_box);
        for s in &self.shapes {
            out.extend_from_slice(&[
                s.kind as u8,
                s.fill_rule as u8,
                s.line.join as u8,
                s.line.cap as u8,
            ]);
            for v in [s.fill, s.stroke] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in [s.line.width, s.line.miter_limit, s.dash_offset, s.opacity] {
                out.extend_from_slice(&v.to_bits().to_le_bytes());
            }
            put(&mut out, &s.geometry);
            put(&mut out, &s.transform);
            put(&mut out, &s.dashes);
        }
        out
    }

    /// Parses every string and builds the asset: per shape, its fill
    /// then its stroke (SVG paint order), each an item.
    pub fn build(&self) -> Result<Asset, SvgError> {
        if self.shapes.len() > MAX_SHAPES {
            return Err(SvgError("too many shapes"));
        }
        let view_box = view_box(&self.view_box)?;
        let mut paints = Vec::new();
        let mut items = Vec::new();
        let mut verbs = 0usize;
        for s in &self.shapes {
            let ok = |v: f32| v.is_finite();
            if !(ok(s.line.width) && s.line.width >= 0.0) {
                return Err(SvgError("stroke width"));
            }
            if !(ok(s.line.miter_limit) && s.line.miter_limit >= 1.0) {
                return Err(SvgError("miter limit"));
            }
            if !(0.0..=1.0).contains(&s.opacity) || !ok(s.dash_offset) {
                return Err(SvgError("opacity or dash offset"));
            }
            let budget = MAX_VERBS - verbs;
            let path = match s.kind {
                ShapeKind::Path => path_data(&s.geometry, budget)?,
                ShapeKind::Polyline => points(&s.geometry, false, budget)?,
                ShapeKind::Polygon => points(&s.geometry, true, budget)?,
            };
            verbs += path.verbs.len();
            let transform = transform(&s.transform)?;
            let dash = dash_array(&s.dashes)?.map(|array| Dash {
                array,
                offset: s.dash_offset,
            });
            let mut push = |style: ItemStyle, color: u32, dash: Option<Dash>| {
                paints.push(Paint::Solid(color));
                items.push(Item {
                    path: path.clone(),
                    style,
                    paint: paints.len() - 1,
                    opacity: s.opacity,
                    transform,
                    dash,
                });
            };
            if s.fill & 0xFF != 0 {
                push(ItemStyle::Fill(s.fill_rule), s.fill, None);
            }
            if s.stroke & 0xFF != 0 && s.line.width > 0.0 {
                push(ItemStyle::Stroke(s.line), s.stroke, dash);
            }
        }
        Ok(Asset {
            view_box,
            paints,
            items,
        })
    }
}

/// A cursor over an attribute string.
struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn new(s: &str) -> Scan<'_> {
        let mut sc = Scan {
            s: s.as_bytes(),
            i: 0,
        };
        sc.space();
        sc
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn done(&self) -> bool {
        self.i >= self.s.len()
    }

    /// Skips white space (SVG's: space, tab, line feed, form feed,
    /// carriage return).
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\x0C' | b'\r')) {
            self.i += 1;
        }
    }

    /// Skips white space with at most one comma in it.
    fn separator(&mut self) {
        self.space();
        if self.peek() == Some(b',') {
            self.i += 1;
            self.space();
        }
    }

    fn starts_number(&self) -> bool {
        matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'+' | b'-'))
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - start
    }

    /// A finite number (SVG's grammar: sign, digits, fraction, exponent;
    /// no `NaN` or `inf`), then a separator.
    fn number(&mut self) -> Result<f32, SvgError> {
        let start = self.i;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.i += 1;
        }
        let mut n = self.digits();
        if self.peek() == Some(b'.') {
            self.i += 1;
            n += self.digits();
        }
        if n == 0 {
            return Err(SvgError("expected a number"));
        }
        // An exponent only when digits follow ("1em" is not one).
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mark = self.i;
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                self.i = mark;
            }
        }
        // ASCII by construction.
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
        let v: f32 = text.parse().map_err(|_| SvgError("expected a number"))?;
        if !v.is_finite() {
            return Err(SvgError("number out of range"));
        }
        self.separator();
        Ok(v)
    }

    fn pair(&mut self) -> Result<[f32; 2], SvgError> {
        Ok([self.number()?, self.number()?])
    }

    /// An arc flag: one `0` or `1`, separators optional after it.
    fn flag(&mut self) -> Result<bool, SvgError> {
        let v = match self.peek() {
            Some(b'0') => false,
            Some(b'1') => true,
            _ => return Err(SvgError("expected an arc flag")),
        };
        self.i += 1;
        self.separator();
        Ok(v)
    }
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

/// Path data (SVG 2 `d`): every command, absolute and relative, arcs as
/// cubics. At most `limit` commands.
pub fn path_data(d: &str, limit: usize) -> Result<Path, SvgError> {
    let mut sc = Scan::new(d);
    let mut p = Path::new();
    let (mut cur, mut start) = ([0.0f32; 2], [0.0f32; 2]);
    // The last command's second control point, for S and T.
    let mut cubic_ctrl: Option<[f32; 2]> = None;
    let mut quad_ctrl: Option<[f32; 2]> = None;
    let mut cmd: Option<u8> = None;
    while !sc.done() {
        let c = sc.peek().unwrap_or(0);
        let letter = if c.is_ascii_alphabetic() {
            sc.i += 1;
            sc.space();
            c
        } else {
            // A repeated command; after a move, lines.
            match cmd {
                None => return Err(SvgError("path data starts without a command")),
                Some(b'Z' | b'z') => return Err(SvgError("number after a close")),
                Some(b'M') => b'L',
                Some(b'm') => b'l',
                Some(x) => x,
            }
        };
        if cmd.is_none() && !matches!(letter, b'M' | b'm') {
            return Err(SvgError("path data starts without a move"));
        }
        cmd = Some(letter);
        let base = if letter.is_ascii_lowercase() {
            cur
        } else {
            [0.0; 2]
        };
        let (mut next_cubic, mut next_quad) = (None, None);
        match letter.to_ascii_uppercase() {
            b'M' => {
                cur = add(base, sc.pair()?);
                start = cur;
                p.verbs.push(crate::Verb::MoveTo(cur));
            }
            b'L' => {
                cur = add(base, sc.pair()?);
                p.verbs.push(crate::Verb::LineTo(cur));
            }
            b'H' => {
                cur[0] = base[0] + sc.number()?;
                p.verbs.push(crate::Verb::LineTo(cur));
            }
            b'V' => {
                cur[1] = base[1] + sc.number()?;
                p.verbs.push(crate::Verb::LineTo(cur));
            }
            b'C' | b'S' => {
                let c1 = if letter.eq_ignore_ascii_case(&b'C') {
                    add(base, sc.pair()?)
                } else {
                    cubic_ctrl.map_or(cur, |c| [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]])
                };
                let c2 = add(base, sc.pair()?);
                cur = add(base, sc.pair()?);
                p.verbs.push(crate::Verb::CubicTo(c1, c2, cur));
                next_cubic = Some(c2);
            }
            b'Q' | b'T' => {
                let c = if letter.eq_ignore_ascii_case(&b'Q') {
                    add(base, sc.pair()?)
                } else {
                    quad_ctrl.map_or(cur, |c| [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]])
                };
                cur = add(base, sc.pair()?);
                p.verbs.push(crate::Verb::QuadTo(c, cur));
                next_quad = Some(c);
            }
            b'A' => {
                let [rx, ry] = sc.pair()?;
                let angle = sc.number()?;
                let (large, sweep) = (sc.flag()?, sc.flag()?);
                let to = add(base, sc.pair()?);
                arc(&mut p, cur, [rx, ry], angle, large, sweep, to);
                cur = to;
            }
            b'Z' => {
                p.verbs.push(crate::Verb::Close);
                cur = start;
            }
            _ => return Err(SvgError("unknown path command")),
        }
        cubic_ctrl = next_cubic;
        quad_ctrl = next_quad;
        if p.verbs.len() > limit {
            return Err(SvgError("too many path commands"));
        }
    }
    if !p.is_finite() {
        return Err(SvgError("path out of range"));
    }
    Ok(p)
}

/// An elliptical arc (SVG 2 appendix B.2.4, endpoint to center) as
/// cubics of at most a quarter turn each.
fn arc(
    p: &mut Path,
    from: [f32; 2],
    radii: [f32; 2],
    degrees: f32,
    large: bool,
    sweep: bool,
    to: [f32; 2],
) {
    if from == to {
        return;
    }
    let (mut rx, mut ry) = (radii[0].abs() as f64, radii[1].abs() as f64);
    if rx == 0.0 || ry == 0.0 {
        p.verbs.push(crate::Verb::LineTo(to));
        return;
    }
    let (x1, y1, x2, y2) = (from[0] as f64, from[1] as f64, to[0] as f64, to[1] as f64);
    let (sin, cos) = (degrees as f64).to_radians().sin_cos();
    let (hx, hy) = ((x1 - x2) * 0.5, (y1 - y2) * 0.5);
    let (x1p, y1p) = (cos * hx + sin * hy, -sin * hx + cos * hy);
    // Radii too small to span the endpoints grow to fit.
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let (cxp, cyp) = (coef * rx * y1p / ry, -coef * ry * x1p / rx);
    let cx = cos * cxp - sin * cyp + (x1 + x2) * 0.5;
    let cy = sin * cxp + cos * cyp + (y1 + y2) * 0.5;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let (ux, uy) = ((x1p - cxp) / rx, (y1p - cyp) / ry);
    let (vx, vy) = ((-x1p - cxp) / rx, (-y1p - cyp) / ry);
    let theta = angle(1.0, 0.0, ux, uy);
    let mut delta = angle(ux, uy, vx, vy);
    if !sweep && delta > 0.0 {
        delta -= std::f64::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f64::consts::TAU;
    }
    if !(theta.is_finite() && delta.is_finite()) {
        p.verbs.push(crate::Verb::LineTo(to));
        return;
    }
    let n = (delta.abs() / std::f64::consts::FRAC_PI_2)
        .ceil()
        .clamp(1.0, 4.0) as usize;
    let step = delta / n as f64;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    // A unit-circle point onto the ellipse.
    let map = |x: f64, y: f64| {
        let (x, y) = (x * rx, y * ry);
        [
            (cx + cos * x - sin * y) as f32,
            (cy + sin * x + cos * y) as f32,
        ]
    };
    for i in 0..n {
        let a1 = theta + step * i as f64;
        let a2 = a1 + step;
        let (s1, c1) = a1.sin_cos();
        let (s2, c2) = a2.sin_cos();
        let end = if i + 1 == n { to } else { map(c2, s2) };
        p.verbs.push(crate::Verb::CubicTo(
            map(c1 - k * s1, s1 + k * c1),
            map(c2 + k * s2, s2 - k * c2),
            end,
        ));
    }
}

/// `points` (polyline, polygon): coordinate pairs. An odd count is an
/// error.
pub fn points(s: &str, close: bool, limit: usize) -> Result<Path, SvgError> {
    let mut sc = Scan::new(s);
    let mut p = Path::new();
    while !sc.done() {
        let pt = sc.pair()?;
        p.verbs.push(if p.verbs.is_empty() {
            crate::Verb::MoveTo(pt)
        } else {
            crate::Verb::LineTo(pt)
        });
        if p.verbs.len() > limit {
            return Err(SvgError("too many points"));
        }
    }
    if close && !p.verbs.is_empty() {
        p.verbs.push(crate::Verb::Close);
    }
    Ok(p)
}

/// The `transform` attribute: a list of `matrix`, `translate`, `scale`,
/// `rotate` (degrees, optional center), `skewX` and `skewY`, applied
/// right to left as SVG composes them. Empty is the identity.
pub fn transform(s: &str) -> Result<Affine, SvgError> {
    let mut sc = Scan::new(s);
    let mut m = Affine::IDENTITY;
    while !sc.done() {
        let name_start = sc.i;
        while sc.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            sc.i += 1;
        }
        let name = &sc.s[name_start..sc.i];
        sc.space();
        if sc.peek() != Some(b'(') {
            return Err(SvgError("expected a transform function"));
        }
        sc.i += 1;
        sc.space();
        let mut args = [0.0f32; 6];
        let mut n = 0;
        while sc.starts_number() {
            if n == 6 {
                return Err(SvgError("too many transform arguments"));
            }
            args[n] = sc.number()?;
            n += 1;
        }
        if sc.peek() != Some(b')') {
            return Err(SvgError("unclosed transform function"));
        }
        sc.i += 1;
        let [a, b, c, ..] = args;
        let t = match (name, n) {
            (b"matrix", 6) => Affine(args),
            (b"translate", 1) => Affine::translate(a, 0.0),
            (b"translate", 2) => Affine::translate(a, b),
            (b"scale", 1) => Affine::scale(a, a),
            (b"scale", 2) => Affine::scale(a, b),
            (b"rotate", 1) => Affine::rotate(a.to_radians()),
            (b"rotate", 3) => Affine::translate(b, c)
                .mul(&Affine::rotate(a.to_radians()))
                .mul(&Affine::translate(-b, -c)),
            (b"skewX", 1) => Affine([1.0, 0.0, a.to_radians().tan(), 1.0, 0.0, 0.0]),
            (b"skewY", 1) => Affine([1.0, a.to_radians().tan(), 0.0, 1.0, 0.0, 0.0]),
            _ => return Err(SvgError("unknown transform or argument count")),
        };
        m = m.mul(&t);
        sc.separator();
    }
    if !m.0.iter().all(|v| v.is_finite()) {
        return Err(SvgError("transform out of range"));
    }
    Ok(m)
}

/// `viewBox`: x, y, width, height, with a positive width and height.
pub fn view_box(s: &str) -> Result<[f32; 4], SvgError> {
    let mut sc = Scan::new(s);
    let v = [sc.number()?, sc.number()?, sc.number()?, sc.number()?];
    if !sc.done() {
        return Err(SvgError("view box takes four numbers"));
    }
    if !(v[2] > 0.0 && v[3] > 0.0) {
        return Err(SvgError("empty view box"));
    }
    Ok(v)
}

/// `stroke-dasharray`: non-negative lengths (user units; no percents),
/// an odd list repeated to make it even. `None` for a solid stroke:
/// empty, `none`, or all zero.
pub fn dash_array(s: &str) -> Result<Option<Vec<f32>>, SvgError> {
    if s.trim() == "none" {
        return Ok(None);
    }
    let mut sc = Scan::new(s);
    let mut out = Vec::new();
    while !sc.done() {
        if out.len() == MAX_DASHES {
            return Err(SvgError("too many dashes"));
        }
        let v = sc.number()?;
        if v < 0.0 {
            return Err(SvgError("negative dash"));
        }
        out.push(v);
    }
    if out.iter().all(|&v| v == 0.0) {
        return Ok(None);
    }
    if out.len() % 2 == 1 {
        out.extend_from_within(..);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Verb::*;

    fn d(s: &str) -> Vec<crate::Verb> {
        path_data(s, MAX_VERBS).unwrap().verbs
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    #[test]
    fn path_commands_absolute_and_relative() {
        assert_eq!(
            d("M1 2L3 4h2v-1H0V0z"),
            [
                MoveTo([1.0, 2.0]),
                LineTo([3.0, 4.0]),
                LineTo([5.0, 4.0]),
                LineTo([5.0, 3.0]),
                LineTo([0.0, 3.0]),
                LineTo([0.0, 0.0]),
                Close
            ]
        );
        // Implicit repeats: after a move, lines; relative ones add up.
        assert_eq!(
            d("m1 1 2 0 0 2"),
            [MoveTo([1.0, 1.0]), LineTo([3.0, 1.0]), LineTo([3.0, 3.0])]
        );
        // After a close, relative commands start from the subpath start.
        assert_eq!(
            d("M5 5l1 0zl0 1"),
            [
                MoveTo([5.0, 5.0]),
                LineTo([6.0, 5.0]),
                Close,
                LineTo([5.0, 6.0])
            ]
        );
        // Compact numbers: signs and dots separate them.
        assert_eq!(
            d("M.5.5-1-1e1 2E-1,3"),
            [
                MoveTo([0.5, 0.5]),
                LineTo([-1.0, -10.0]),
                LineTo([0.2, 3.0])
            ]
        );
    }

    #[test]
    fn smooth_curves_reflect_their_control_points() {
        assert_eq!(
            d("M0 0C1 1 2 1 3 0S5-1 6 0"),
            [
                MoveTo([0.0, 0.0]),
                CubicTo([1.0, 1.0], [2.0, 1.0], [3.0, 0.0]),
                CubicTo([4.0, -1.0], [5.0, -1.0], [6.0, 0.0])
            ]
        );
        assert_eq!(
            d("M0 0Q1 1 2 0t2 0"),
            [
                MoveTo([0.0, 0.0]),
                QuadTo([1.0, 1.0], [2.0, 0.0]),
                QuadTo([3.0, -1.0], [4.0, 0.0])
            ]
        );
        // S after a non-curve: the first control point is the current one.
        assert_eq!(
            d("M0 0L1 0S2 1 3 0")[2],
            CubicTo([1.0, 0.0], [2.0, 1.0], [3.0, 0.0])
        );
    }

    #[test]
    fn arcs_become_cubics() {
        // Lucide's "a3 3 0 0 0 6 0": a half circle of radius 3 below
        // (sweep 0 from (10, 8) to (16, 8) runs through y 11).
        let v = d("M10 8a3 3 0 0 0 6 0");
        assert_eq!(v.len(), 3);
        let CubicTo(_, _, mid) = v[1] else { panic!() };
        assert!(close(mid, [13.0, 11.0]), "{mid:?}");
        let CubicTo(_, _, end) = v[2] else { panic!() };
        assert_eq!(end, [16.0, 8.0]);
        // Flags packed without separators ("a1 1 0 00-1 1").
        assert_eq!(d("M1 0a1 1 0 00-1 1").len(), 2);
        // Radii too small grow to span the endpoints; zero radii draw a
        // line; a zero-length arc draws nothing.
        assert_eq!(d("M0 0A1 1 0 0 1 10 0").len(), 3);
        assert_eq!(d("M0 0A0 1 0 0 1 10 0")[1], LineTo([10.0, 0.0]));
        assert_eq!(d("M0 0A1 1 0 0 1 0 0").len(), 1);
        // A full circle from two half arcs stays within its radius.
        let p = path_data("M22 12A10 10 0 0 1 2 12A10 10 0 0 1 22 12", 64).unwrap();
        let m = crate::fill(&p, FillRule::NonZero, 0.01).unwrap();
        let exact = std::f64::consts::PI * 100.0;
        assert!((m.area() - exact).abs() / exact < 0.01, "{}", m.area());
    }

    #[test]
    fn points_transforms_view_boxes_and_dashes() {
        assert_eq!(
            points("0,0 1,1 2 0", true, 16).unwrap().verbs,
            [
                MoveTo([0.0, 0.0]),
                LineTo([1.0, 1.0]),
                LineTo([2.0, 0.0]),
                Close
            ]
        );
        assert!(points("0,0 1", false, 16).is_err());
        // rotate(-90 12 12) maps (24, 12) to (12, 0).
        let m = transform("rotate(-90 12 12)").unwrap();
        let p = m.apply(craie_core::geom::Point::new(24.0, 12.0));
        assert!(close([p.x, p.y], [12.0, 0.0]), "{p:?}");
        // A list applies right to left: scale first, then translate.
        let m = transform("translate(10, 0) scale(2)").unwrap();
        assert_eq!(m, Affine([2.0, 0.0, 0.0, 2.0, 10.0, 0.0]));
        assert_eq!(transform("").unwrap(), Affine::IDENTITY);
        assert_eq!(
            transform("matrix(1 2 3 4 5 6)").unwrap(),
            Affine([1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
        );
        assert_eq!(view_box("0 0 24 24").unwrap(), [0.0, 0.0, 24.0, 24.0]);
        assert_eq!(view_box(" -1,-1 2,2 ").unwrap(), [-1.0, -1.0, 2.0, 2.0]);
        assert!(view_box("0 0 0 24").is_err());
        assert!(view_box("0 0 24").is_err());
        assert_eq!(dash_array("4 2").unwrap(), Some(vec![4.0, 2.0]));
        assert_eq!(
            dash_array("1,2,3").unwrap(),
            Some(vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0])
        );
        assert_eq!(dash_array("none").unwrap(), None);
        assert_eq!(dash_array("0 0").unwrap(), None);
        assert!(dash_array("4 -2").is_err());
        assert!(dash_array("50%").is_err());
    }

    /// Malformed input fails, and fails fast: truncated numbers, words
    /// for numbers, NaN and infinities, numbers past f32, bad commands,
    /// runaway repeats, and random bytes.
    #[test]
    fn hostile_strings_fail_cleanly() {
        for bad in [
            "M",
            "M1",
            "M1 2L",
            "M1 2L3",
            "L1 2",
            "1 2",
            "M1 2 z 3",
            "M1e",
            "M1 2X3 4",
            "M NaN 1",
            "M inf 1",
            "M1e39 0",
            "M-1e39 0",
            "M1 2a1 1 0 2 0 3 3",
            "M1 2a1 1 0 0",
            "M..1 2",
            "M+-1 2",
            "M1 2,,3 4",
            "M1,",
            "M1 2 L 3 4 %",
        ] {
            assert!(path_data(bad, MAX_VERBS).is_err(), "{bad:?}");
        }
        for bad in [
            "rotate(",
            "rotate(1",
            "rotate(1 2)",
            "scale()",
            "matrix(1 2 3 4 5)",
            "matrix(1 2 3 4 5 6 7)",
            "skewX(90) nope",
            "translate(1e39)",
            "((((((((",
            "rotate(1))",
        ] {
            assert!(transform(bad).is_err(), "{bad:?}");
        }
        // A huge count stops at the limit, not at the end of the input.
        let many = "M0 0".to_string() + &" 1 1".repeat(200_000);
        assert!(path_data(&many, 1000).is_err());
        let pts = "1 1 ".repeat(200_000);
        assert!(points(&pts, false, 1000).is_err());
        assert!(dash_array(&"1 ".repeat(1000)).is_err());
        // Long runs of digits parse or fail without trouble.
        let long = format!("M{} 0", "9".repeat(100_000));
        assert!(path_data(&long, MAX_VERBS).is_err());
        let long = format!("M0.{} 0", "1".repeat(100_000));
        assert_eq!(path_data(&long, MAX_VERBS).unwrap().verbs.len(), 1);
        // Pseudo-random bytes from the command alphabet never panic.
        let alphabet = b"MmLlHhVvCcSsQqTtAaZz0123456789.,-+eE \n()";
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..2000 {
            let n = (x % 48) as usize;
            let s: String = (0..n)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    alphabet[(x % alphabet.len() as u64) as usize] as char
                })
                .collect();
            let _ = path_data(&s, 64);
            let _ = transform(&s);
            let _ = points(&s, true, 64);
            let _ = dash_array(&s);
            let _ = view_box(&s);
        }
    }

    #[test]
    fn drawings_build_fill_then_stroke() {
        let drawing = Drawing {
            view_box: "0 0 24 24".into(),
            shapes: vec![
                Shape {
                    geometry: "M4 4h16v16H4z".into(),
                    fill: 0xFF00_00FF,
                    stroke: 0x0000_FFFF,
                    line: Stroke {
                        width: 2.0,
                        ..Stroke::default()
                    },
                    dashes: "4 2".into(),
                    dash_offset: 1.0,
                    ..Shape::default()
                },
                // Nothing painted: no items.
                Shape {
                    kind: ShapeKind::Polyline,
                    geometry: "0 0 24 24".into(),
                    fill: 0,
                    ..Shape::default()
                },
            ],
        };
        let a = drawing.build().unwrap();
        assert_eq!(a.view_box, [0.0, 0.0, 24.0, 24.0]);
        assert_eq!(a.items.len(), 2);
        assert!(matches!(a.items[0].style, ItemStyle::Fill(_)));
        assert!(matches!(a.items[1].style, ItemStyle::Stroke(_)));
        assert_eq!(
            a.items[1].dash,
            Some(Dash {
                array: vec![4.0, 2.0],
                offset: 1.0
            })
        );
        assert_eq!(a.paints[1], Paint::Solid(0x0000_FFFF));
        // Equal drawings have equal keys; any change changes the key.
        let mut other = drawing.clone();
        assert_eq!(other.key(), drawing.key());
        other.shapes[0].dash_offset = 2.0;
        assert_ne!(other.key(), drawing.key());
        // One bad string fails the drawing.
        other.shapes[1].geometry = "0 0 24".into();
        assert!(other.build().is_err());
        let wide = Drawing {
            view_box: "0 0 1 1".into(),
            shapes: vec![Shape::default(); MAX_SHAPES + 1],
        };
        assert!(wide.build().is_err());
    }
}

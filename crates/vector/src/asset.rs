//! Prepared vector assets (`CRV1`): the build-time SVG importer
//! (`tools/svg-import`, usvg) writes them; the runtime decodes them. SVG
//! never reaches the shipped binary (ARCHITECTURE.md section 9).
//!
//! Layout, little-endian:
//!
//! ```text
//! header:  magic "CRV1" u32 | version u16 | flags u16
//!          view box f32 x 4 (x, y, width, height)
//!          paint count u32 | item count u32 | verb count u32 | point count u32
//! paints:  kind u8, then
//!            solid:  color u32 (0xRRGGBBAA)
//!            current: tint u32 (kind 3: the inherited color times it)
//!            linear: x0, y0, x1, y1 f32 | transform f32 x 6 | stops
//!            radial: cx, cy, r f32       | transform f32 x 6 | stops
//!          stops: count u16, then (offset f32, color u32) per stop
//! items:   kind u8 (0 fill, 1 stroke) | paint u32 | opacity f32
//!          transform f32 x 6 | verb start u32 | verb count u32 | point start u32
//!          fill: rule u8 (0 nonzero, 1 even-odd)
//!          stroke: width f32 | join u8 | cap u8 | miter limit f32
//! verbs:   u8 each (0 move, 1 line, 2 quad, 3 cubic, 4 close)
//! points:  (x, y) f32 pairs, as the verbs consume them
//! ```
//!
//! Items draw in order. A gradient's transform maps gradient space to
//! the item's space; an item's transform maps its space to the view
//! box's. `decode` checks everything (counts against the bytes, ranges,
//! finite numbers) before it allocates for it.

use craie_core::geom::Affine;

use crate::{Dash, FillRule, LineCap, LineJoin, Paint, Path, Stroke, Verb};

pub const MAGIC: u32 = 0x3156_5243; // "CRV1"
pub const VERSION: u16 = 1;
/// Bounds that keep a hostile asset from asking for huge allocations.
pub const MAX_ITEMS: usize = 1 << 16;
pub const MAX_POINTS: usize = 1 << 22;
pub const MAX_STOPS: usize = 64;
/// Commands all items expand to together (items may share a verb
/// range, so this bounds the decoded paths, not the bytes).
pub const MAX_EXPANDED_VERBS: usize = 1 << 20;

/// How an item draws its path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemStyle {
    Fill(FillRule),
    Stroke(Stroke),
}

/// One drawn path.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: Path,
    pub style: ItemStyle,
    /// Index into `Asset::paints`.
    pub paint: usize,
    /// Multiplies the paint's alpha (fill or stroke opacity, and the
    /// opacity of every group around the item).
    pub opacity: f32,
    /// Item space to view-box space.
    pub transform: Affine,
    /// A stroke's dashes (runtime drawings only: `CRV1` has none).
    pub dash: Option<Dash>,
}

/// A decoded asset.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    /// x, y, width, height (width and height positive).
    pub view_box: [f32; 4],
    pub paints: Vec<Paint>,
    pub items: Vec<Item>,
}

impl Asset {
    /// Whether an item paints with the inherited color.
    pub fn inherits_color(&self) -> bool {
        self.items
            .iter()
            .any(|it| matches!(self.paints[it.paint], Paint::Current(_)))
    }
}

/// Why bytes are not an asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetError {
    Truncated,
    BadMagic,
    BadVersion(u16),
    /// A count, index, range, enum value, or number out of range.
    Invalid(&'static str),
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], AssetError> {
        if self.buf.len() - self.pos < n {
            return Err(AssetError::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, AssetError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, AssetError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, AssetError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// A finite f32.
    fn f32(&mut self) -> Result<f32, AssetError> {
        let v = f32::from_le_bytes(self.take(4)?.try_into().unwrap());
        if v.is_finite() {
            Ok(v)
        } else {
            Err(AssetError::Invalid("non-finite number"))
        }
    }
    fn affine(&mut self) -> Result<Affine, AssetError> {
        let mut m = [0.0f32; 6];
        for v in &mut m {
            *v = self.f32()?;
        }
        Ok(Affine(m))
    }
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

fn stops(r: &mut Reader<'_>) -> Result<Vec<(f32, u32)>, AssetError> {
    let n = r.u16()? as usize;
    if n > MAX_STOPS {
        return Err(AssetError::Invalid("too many stops"));
    }
    if r.remaining() < n * 8 {
        return Err(AssetError::Truncated);
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let offset = r.f32()?;
        if !(0.0..=1.0).contains(&offset) {
            return Err(AssetError::Invalid("stop offset outside [0, 1]"));
        }
        out.push((offset, r.u32()?));
    }
    Ok(out)
}

/// Decodes and checks an asset.
pub fn decode(buf: &[u8]) -> Result<Asset, AssetError> {
    let mut r = Reader { buf, pos: 0 };
    if r.u32()? != MAGIC {
        return Err(AssetError::BadMagic);
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(AssetError::BadVersion(version));
    }
    if r.u16()? != 0 {
        return Err(AssetError::Invalid("unknown flags"));
    }
    let view_box = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    if !(view_box[2] > 0.0 && view_box[3] > 0.0) {
        return Err(AssetError::Invalid("empty view box"));
    }
    let (np, ni, nv, npt) = (
        r.u32()? as usize,
        r.u32()? as usize,
        r.u32()? as usize,
        r.u32()? as usize,
    );
    // Each count against the smallest record it could be: nothing is
    // allocated for bytes that are not there.
    if ni > MAX_ITEMS || npt > MAX_POINTS {
        return Err(AssetError::Invalid("too many items or points"));
    }
    // Smallest paint: kind + color (5); smallest item: a fill (46).
    let least = np * 5 + ni * 46 + nv + npt * 8;
    if r.remaining() < least {
        return Err(AssetError::Truncated);
    }
    let mut paints = Vec::with_capacity(np);
    for _ in 0..np {
        paints.push(match r.u8()? {
            0 => Paint::Solid(r.u32()?),
            3 => Paint::Current(r.u32()?),
            1 => {
                let (x0, y0, x1, y1) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
                let transform = r.affine()?;
                Paint::Linear {
                    start: [x0, y0],
                    end: [x1, y1],
                    transform,
                    stops: stops(&mut r)?,
                }
            }
            2 => {
                let (cx, cy, radius) = (r.f32()?, r.f32()?, r.f32()?);
                if radius < 0.0 {
                    return Err(AssetError::Invalid("negative radius"));
                }
                let transform = r.affine()?;
                Paint::Radial {
                    center: [cx, cy],
                    radius,
                    transform,
                    stops: stops(&mut r)?,
                }
            }
            _ => return Err(AssetError::Invalid("paint kind")),
        });
    }
    struct Raw {
        style: ItemStyle,
        paint: usize,
        opacity: f32,
        transform: Affine,
        verbs: std::ops::Range<usize>,
        point: usize,
    }
    let mut raw = Vec::with_capacity(ni);
    let mut expanded = 0usize;
    for _ in 0..ni {
        let kind = r.u8()?;
        let paint = r.u32()? as usize;
        if paint >= np {
            return Err(AssetError::Invalid("paint index"));
        }
        let opacity = r.f32()?;
        if !(0.0..=1.0).contains(&opacity) {
            return Err(AssetError::Invalid("opacity outside [0, 1]"));
        }
        let transform = r.affine()?;
        let (vs, vn, ps) = (r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
        if vs.checked_add(vn).is_none_or(|e| e > nv) || ps > npt {
            return Err(AssetError::Invalid("path range"));
        }
        expanded += vn;
        if expanded > MAX_EXPANDED_VERBS {
            return Err(AssetError::Invalid("paths expand past the limit"));
        }
        let style = match kind {
            0 => ItemStyle::Fill(match r.u8()? {
                0 => FillRule::NonZero,
                1 => FillRule::EvenOdd,
                _ => return Err(AssetError::Invalid("fill rule")),
            }),
            1 => {
                let width = r.f32()?;
                let join = match r.u8()? {
                    0 => LineJoin::Miter,
                    1 => LineJoin::Round,
                    2 => LineJoin::Bevel,
                    _ => return Err(AssetError::Invalid("line join")),
                };
                let cap = match r.u8()? {
                    0 => LineCap::Butt,
                    1 => LineCap::Round,
                    2 => LineCap::Square,
                    _ => return Err(AssetError::Invalid("line cap")),
                };
                let miter_limit = r.f32()?;
                if width <= 0.0 || miter_limit < 1.0 {
                    return Err(AssetError::Invalid("stroke"));
                }
                ItemStyle::Stroke(Stroke {
                    width,
                    join,
                    cap,
                    miter_limit,
                })
            }
            _ => return Err(AssetError::Invalid("item kind")),
        };
        raw.push(Raw {
            style,
            paint,
            opacity,
            transform,
            verbs: vs..vs + vn,
            point: ps,
        });
    }
    let verbs = r.take(nv)?.to_vec();
    if r.remaining() != npt * 8 {
        return Err(AssetError::Invalid("point count"));
    }
    let mut points = Vec::with_capacity(npt);
    for _ in 0..npt {
        points.push([r.f32()?, r.f32()?]);
    }
    let mut items = Vec::with_capacity(ni);
    for it in raw {
        let mut path = Path::new();
        let mut at = it.point;
        let mut next = |n: usize| -> Result<&[[f32; 2]], AssetError> {
            if points.len() - at < n {
                return Err(AssetError::Invalid("path points"));
            }
            at += n;
            Ok(&points[at - n..at])
        };
        for &v in &verbs[it.verbs.clone()] {
            path.verbs.push(match v {
                0 => Verb::MoveTo(next(1)?[0]),
                1 => Verb::LineTo(next(1)?[0]),
                2 => {
                    let p = next(2)?;
                    Verb::QuadTo(p[0], p[1])
                }
                3 => {
                    let p = next(3)?;
                    Verb::CubicTo(p[0], p[1], p[2])
                }
                4 => Verb::Close,
                _ => return Err(AssetError::Invalid("verb")),
            });
        }
        items.push(Item {
            path,
            style: it.style,
            paint: it.paint,
            opacity: it.opacity,
            transform: it.transform,
            dash: None,
        });
    }
    Ok(Asset {
        view_box,
        paints,
        items,
    })
}

/// Encodes an asset (the importer's output; tests).
pub fn encode(a: &Asset) -> Vec<u8> {
    let mut out = Vec::new();
    let u16le = |o: &mut Vec<u8>, v: u16| o.extend_from_slice(&v.to_le_bytes());
    let u32le = |o: &mut Vec<u8>, v: u32| o.extend_from_slice(&v.to_le_bytes());
    let f32le = |o: &mut Vec<u8>, v: f32| o.extend_from_slice(&v.to_le_bytes());
    let mut verbs = Vec::new();
    let mut points: Vec<[f32; 2]> = Vec::new();
    let mut ranges = Vec::new();
    for it in &a.items {
        let (vs, ps) = (verbs.len(), points.len());
        for v in &it.path.verbs {
            match *v {
                Verb::MoveTo(p) => {
                    verbs.push(0);
                    points.push(p);
                }
                Verb::LineTo(p) => {
                    verbs.push(1);
                    points.push(p);
                }
                Verb::QuadTo(c, p) => {
                    verbs.push(2);
                    points.extend([c, p]);
                }
                Verb::CubicTo(c1, c2, p) => {
                    verbs.push(3);
                    points.extend([c1, c2, p]);
                }
                Verb::Close => verbs.push(4),
            }
        }
        ranges.push((vs, verbs.len() - vs, ps));
    }
    u32le(&mut out, MAGIC);
    u16le(&mut out, VERSION);
    u16le(&mut out, 0);
    for v in a.view_box {
        f32le(&mut out, v);
    }
    for n in [a.paints.len(), a.items.len(), verbs.len(), points.len()] {
        u32le(&mut out, n as u32);
    }
    let put_stops = |o: &mut Vec<u8>, stops: &[(f32, u32)]| {
        u16le(o, stops.len() as u16);
        for &(offset, color) in stops {
            f32le(o, offset);
            u32le(o, color);
        }
    };
    for p in &a.paints {
        match p {
            Paint::Solid(c) => {
                out.push(0);
                u32le(&mut out, *c);
            }
            Paint::Current(tint) => {
                out.push(3);
                u32le(&mut out, *tint);
            }
            Paint::Linear {
                start,
                end,
                stops,
                transform,
            } => {
                out.push(1);
                for v in [start[0], start[1], end[0], end[1]] {
                    f32le(&mut out, v);
                }
                transform.0.iter().for_each(|&v| f32le(&mut out, v));
                put_stops(&mut out, stops);
            }
            Paint::Radial {
                center,
                radius,
                stops,
                transform,
            } => {
                out.push(2);
                for v in [center[0], center[1], *radius] {
                    f32le(&mut out, v);
                }
                transform.0.iter().for_each(|&v| f32le(&mut out, v));
                put_stops(&mut out, stops);
            }
        }
    }
    for (it, &(vs, vn, ps)) in a.items.iter().zip(&ranges) {
        out.push(match it.style {
            ItemStyle::Fill(_) => 0,
            ItemStyle::Stroke(_) => 1,
        });
        u32le(&mut out, it.paint as u32);
        f32le(&mut out, it.opacity);
        it.transform.0.iter().for_each(|&v| f32le(&mut out, v));
        u32le(&mut out, vs as u32);
        u32le(&mut out, vn as u32);
        u32le(&mut out, ps as u32);
        match it.style {
            ItemStyle::Fill(rule) => out.push(rule as u8),
            ItemStyle::Stroke(s) => {
                f32le(&mut out, s.width);
                out.push(s.join as u8);
                out.push(s.cap as u8);
                f32le(&mut out, s.miter_limit);
            }
        }
    }
    out.extend_from_slice(&verbs);
    for p in points {
        f32le(&mut out, p[0]);
        f32le(&mut out, p[1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Asset {
        let mut tri = Path::new();
        tri.move_to(0.0, 0.0)
            .line_to(10.0, 0.0)
            .quad_to(10.0, 10.0, 0.0, 10.0)
            .close();
        Asset {
            view_box: [0.0, 0.0, 24.0, 24.0],
            paints: vec![
                Paint::Solid(0xFF00_00FF),
                Paint::Linear {
                    start: [0.0, 0.0],
                    end: [24.0, 0.0],
                    stops: vec![(0.0, 0xFFFF_FFFF), (1.0, 0x0000_00FF)],
                    transform: Affine::IDENTITY,
                },
                Paint::Radial {
                    center: [12.0, 12.0],
                    radius: 8.0,
                    stops: vec![(0.5, 0x00FF_00FF)],
                    transform: Affine::scale(2.0, 1.0),
                },
                Paint::Current(0xFFFF_FF80),
            ],
            items: vec![
                Item {
                    path: tri.clone(),
                    style: ItemStyle::Fill(FillRule::EvenOdd),
                    paint: 1,
                    opacity: 0.5,
                    transform: Affine::translate(2.0, 3.0),
                    dash: None,
                },
                Item {
                    path: Path::circle(12.0, 12.0, 5.0),
                    style: ItemStyle::Stroke(Stroke {
                        width: 2.0,
                        join: LineJoin::Round,
                        cap: LineCap::Square,
                        miter_limit: 4.0,
                    }),
                    paint: 0,
                    opacity: 1.0,
                    transform: Affine::IDENTITY,
                    dash: None,
                },
            ],
        }
    }

    #[test]
    fn assets_round_trip() {
        let a = sample();
        assert_eq!(decode(&encode(&a)).unwrap(), a);
        // The smallest records: one solid paint, one fill item, exactly.
        let small = Asset {
            view_box: [0.0, 0.0, 1.0, 1.0],
            paints: vec![Paint::Solid(1)],
            items: vec![Item {
                path: Path::rect(0.0, 0.0, 1.0, 1.0),
                style: ItemStyle::Fill(FillRule::NonZero),
                paint: 0,
                opacity: 1.0,
                transform: Affine::IDENTITY,
                dash: None,
            }],
        };
        assert_eq!(decode(&encode(&small)).unwrap(), small);
    }

    /// An asset inherits the node's color when an item paints with
    /// `Current`; an unused `Current` paint does not count.
    #[test]
    fn inherits_color_needs_an_item() {
        let mut a = sample();
        assert!(!a.inherits_color());
        a.items[1].paint = 3;
        assert!(a.inherits_color());
    }

    /// Every truncation and a sweep of single-byte corruptions decode to
    /// an error or a checked asset, never a panic or a huge allocation.
    #[test]
    fn malformed_assets_fail_cleanly() {
        let bytes = encode(&sample());
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err(), "truncated at {n}");
        }
        for i in 0..bytes.len() {
            for v in [0u8, 1, 0x7F, 0xFF] {
                let mut b = bytes.clone();
                b[i] = v;
                if let Ok(a) = decode(&b) {
                    assert!(a.items.iter().all(|it| it.paint < a.paints.len()));
                    assert!(a.items.iter().all(|it| it.path.is_finite()));
                }
            }
        }
        // Items sharing one verb range cannot expand past the limit (a
        // small asset must not decode into huge paths).
        let mut shared = sample();
        shared.items[0].path = Path::new();
        for _ in 0..1000 {
            shared.items[0].path.line_to(1.0, 1.0);
        }
        let one = encode(&Asset {
            items: vec![shared.items[0].clone()],
            ..shared.clone()
        });
        // Repoint many items at the same 1,000 verbs by hand: header,
        // then patch the item count and append copies of the item row.
        let item_at = {
            let mut r = Reader { buf: &one, pos: 0 };
            r.take(40).unwrap();
            for _ in 0..sample().paints.len() {
                match r.u8().unwrap() {
                    0 | 3 => {
                        r.u32().unwrap();
                    }
                    1 => {
                        r.take(40).unwrap();
                        let n = r.u16().unwrap() as usize;
                        r.take(n * 8).unwrap();
                    }
                    _ => {
                        r.take(36).unwrap();
                        let n = r.u16().unwrap() as usize;
                        r.take(n * 8).unwrap();
                    }
                }
            }
            r.pos
        };
        let row = &one[item_at..item_at + 46];
        let copies = MAX_EXPANDED_VERBS / 1000 + 2;
        let mut many = one[..item_at].to_vec();
        for _ in 0..copies {
            many.extend_from_slice(row);
        }
        many.extend_from_slice(&one[item_at + 46..]);
        many[28..32].copy_from_slice(&(copies as u32).to_le_bytes());
        assert_eq!(
            decode(&many),
            Err(AssetError::Invalid("paths expand past the limit"))
        );
        // A count claiming more than the bytes hold fails before
        // allocating for it.
        let mut b = bytes.clone();
        b[28..32].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert!(decode(&b).is_err());
        let mut b = bytes;
        b[0] = b'X';
        assert_eq!(decode(&b), Err(AssetError::BadMagic));
    }
}

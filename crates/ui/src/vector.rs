//! Vector nodes (ARCHITECTURE.md section 9, step 5b): a prepared asset
//! (`craie_vector::asset`) fitted into the content box, centered with its
//! aspect ratio kept (SVG `xMidYMid meet`). Each item tessellates in its
//! own space at a tolerance of a quarter device pixel, then maps into the
//! chunk; the meshes are cached per node until the fit or the display
//! scale changes. Opacity multiplies each paint's alpha.

use std::collections::HashMap;
use std::sync::Arc;

use craie_core::geom::{Affine, Rect};
use craie_scene::{ChunkWriter, GradientPaint, gradient};
use craie_vector::asset::{Asset, ItemStyle};
use craie_vector::{Mesh, Paint};

use crate::host::NodeId;
use crate::layout::LayoutData;
use crate::ui::Ui;

/// Device px of flattening error allowed.
const TOLERANCE_PX: f32 = 0.25;

/// A tessellated item in chunk-local units, and its paint.
struct Prepared {
    mesh: Mesh,
    paint: Prepaint,
}

enum Prepaint {
    Solid(u32),
    Gradient(GradientPaint),
}

/// A node's meshes and what they were made for.
pub(crate) struct CachedVector {
    /// (content box, display scale) bits and the asset they came from.
    key: ([u32; 5], usize),
    items: Vec<Prepared>,
}

pub(crate) type VectorCache = HashMap<u32, CachedVector>;

/// The view box fitted into `content`: scaled uniformly to fit, centered.
pub fn fit(view_box: [f32; 4], content: Rect) -> Affine {
    let [x, y, w, h] = view_box;
    let s = (content.size.width / w)
        .min(content.size.height / h)
        .max(0.0);
    let dx = content.origin.x + (content.size.width - w * s) * 0.5 - x * s;
    let dy = content.origin.y + (content.size.height - h * s) * 0.5 - y * s;
    Affine([s, 0.0, 0.0, s, dx, dy])
}

/// The intrinsic content size of a vector node: its view box, with a
/// known dimension scaling the other to keep the aspect ratio.
pub fn intrinsic(view_box: [f32; 4], known: [Option<f32>; 2]) -> [f32; 2] {
    let [_, _, w, h] = view_box;
    match known {
        [Some(kw), Some(kh)] => [kw, kh],
        [Some(kw), None] => [kw, kw * h / w],
        [None, Some(kh)] => [kh * w / h, kh],
        [None, None] => [w, h],
    }
}

/// The intrinsic content size under min and max constraints (content
/// box; `None`: unconstrained): a clamped width sets the height by the
/// aspect ratio unless the height is known, and a clamped height the
/// width.
pub fn constrained(
    view_box: [f32; 4],
    known: [Option<f32>; 2],
    min: [Option<f32>; 2],
    max: [Option<f32>; 2],
) -> [f32; 2] {
    let [w0, h0] = intrinsic(view_box, known);
    let ratio = view_box[3] / view_box[2];
    let clamp = |v: f32, lo: Option<f32>, hi: Option<f32>| {
        let v = hi.map_or(v, |h| v.min(h));
        lo.map_or(v, |l| v.max(l))
    };
    let (mut w, mut h) = (w0, h0);
    if known[0].is_none() {
        let c = clamp(w, min[0], max[0]);
        if c != w && known[1].is_none() {
            h = c * ratio;
        }
        w = c;
    }
    if known[1].is_none() {
        let c = clamp(h, min[1], max[1]);
        if c != h && known[0].is_none() {
            w = clamp(c / ratio, min[0], max[0]);
        }
        h = c;
    }
    [w, h]
}

/// `mesh` cut to `clip` (the drawing's viewport, as SVG clips an
/// embedded drawing): triangles inside stay, triangles across an edge
/// are cut (Sutherland-Hodgman) and re-fanned, triangles outside go.
fn clip_mesh(mesh: Mesh, clip: Rect) -> Mesh {
    let (x0, y0, x1, y1) = (clip.origin.x, clip.origin.y, clip.max_x(), clip.max_y());
    let inside = |v: &[f32; 2]| v[0] >= x0 && v[0] <= x1 && v[1] >= y0 && v[1] <= y1;
    if mesh.vertices.iter().all(inside) {
        return mesh;
    }
    let mut out = Mesh::default();
    let mut poly: Vec<[f32; 2]> = Vec::with_capacity(9);
    let mut next: Vec<[f32; 2]> = Vec::with_capacity(9);
    for t in mesh.indices.chunks_exact(3) {
        poly.clear();
        poly.extend(t.iter().map(|&i| mesh.vertices[i as usize]));
        // Each edge: (axis, bound, keep below the bound).
        for (axis, bound, below) in [(0, x0, false), (0, x1, true), (1, y0, false), (1, y1, true)] {
            let keep = |v: &[f32; 2]| {
                if below {
                    v[axis] <= bound
                } else {
                    v[axis] >= bound
                }
            };
            next.clear();
            for k in 0..poly.len() {
                let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
                let (ka, kb) = (keep(&a), keep(&b));
                if ka {
                    next.push(a);
                }
                if ka != kb {
                    let t = (bound - a[axis]) / (b[axis] - a[axis]);
                    next.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
                }
            }
            std::mem::swap(&mut poly, &mut next);
            if poly.is_empty() {
                break;
            }
        }
        if poly.len() < 3 {
            continue;
        }
        let base = out.vertices.len() as u32;
        out.vertices.extend_from_slice(&poly);
        for k in 1..poly.len() as u32 - 1 {
            out.indices.extend([base, base + k, base + k + 1]);
        }
    }
    out
}

/// The largest factor by which `m` stretches a length (its largest
/// singular value).
fn max_stretch(m: &Affine) -> f32 {
    let [a, b, c, d, _, _] = m.0;
    let half = (a * a + b * b + c * c + d * d) * 0.5;
    let det = a * d - b * c;
    (half + (half * half - det * det).max(0.0).sqrt()).sqrt()
}

/// `alpha` of a 0xRRGGBBAA color scaled by `opacity`.
fn fade(color: u32, opacity: f32) -> u32 {
    let a = ((color & 0xFF) as f32 * opacity).round().clamp(0.0, 255.0) as u32;
    (color & !0xFF) | a
}

fn prepare(asset: &Asset, content: Rect, scale: f32) -> Vec<Prepared> {
    let place = fit(asset.view_box, content);
    // The view box in chunk space: the drawing's viewport.
    let [vx, vy, vw, vh] = asset.view_box;
    let p0 = place.apply(craie_core::geom::Point::new(vx, vy));
    let p1 = place.apply(craie_core::geom::Point::new(vx + vw, vy + vh));
    let viewport = Rect::new(p0.x, p0.y, p1.x - p0.x, p1.y - p0.y);
    let mut out = Vec::with_capacity(asset.items.len());
    for it in &asset.items {
        let m = place.mul(&it.transform);
        let det = m.determinant().abs();
        if !(det > 1e-12 && det.is_finite()) {
            continue;
        }
        // Fills (and strokes under a similarity: equal axes, no shear)
        // tessellate in chunk space, where the tolerance is a quarter
        // device px whatever the asset's units. Other strokes must
        // tessellate in item space (a stroke's width follows the
        // transform): there the tolerance is a quarter device px over
        // the transform's largest stretch.
        let [a, b, c, d, _, _] = m.0;
        let k = (a * a + b * b).sqrt();
        let similar = (a - d).abs() <= 1e-4 * k && (b + c).abs() <= 1e-4 * k;
        let chunk_tolerance = TOLERANCE_PX / scale;
        let mesh = match it.style {
            ItemStyle::Fill(rule) => {
                craie_vector::fill(&it.path.transformed(&m), rule, chunk_tolerance)
            }
            ItemStyle::Stroke(s) if similar => {
                let s = craie_vector::Stroke {
                    width: s.width * k,
                    ..s
                };
                craie_vector::stroke(&it.path.transformed(&m), &s, chunk_tolerance)
            }
            ItemStyle::Stroke(s) => {
                let tolerance = chunk_tolerance / max_stretch(&m);
                craie_vector::stroke(&it.path, &s, tolerance).map(|mut mesh| {
                    for v in &mut mesh.vertices {
                        let p = m.apply(craie_core::geom::Point::new(v[0], v[1]));
                        *v = [p.x, p.y];
                    }
                    mesh
                })
            }
        };
        let Ok(mesh) = mesh else { continue };
        let mesh = clip_mesh(mesh, viewport);
        if mesh.indices.is_empty() {
            continue;
        }
        let paint = match &asset.paints[it.paint] {
            Paint::Solid(c) => Prepaint::Solid(fade(*c, it.opacity)),
            Paint::Linear {
                start,
                end,
                stops,
                transform,
            } => {
                let Some(to) = m.mul(transform).invert() else {
                    continue;
                };
                Prepaint::Gradient(GradientPaint {
                    kind: gradient::LINEAR,
                    geometry: [start[0], start[1], end[0], end[1]],
                    to_gradient: to.0,
                    stops: stops
                        .iter()
                        .map(|&(o, c)| (o, fade(c, it.opacity)))
                        .collect(),
                })
            }
            Paint::Radial {
                center,
                radius,
                stops,
                transform,
            } => {
                let Some(to) = m.mul(transform).invert() else {
                    continue;
                };
                Prepaint::Gradient(GradientPaint {
                    kind: gradient::RADIAL,
                    geometry: [center[0], center[1], *radius, 0.0],
                    to_gradient: to.0,
                    stops: stops
                        .iter()
                        .map(|&(o, c)| (o, fade(c, it.opacity)))
                        .collect(),
                })
            }
        };
        out.push(Prepared { mesh, paint });
    }
    out
}

impl Ui {
    /// Writes a vector node's meshes into its chunk (after its box).
    pub(crate) fn build_vector(&mut self, id: NodeId, data: &LayoutData, w: &mut ChunkWriter) {
        let Some(asset) = self.host.vectors.get(&id.0).and_then(|v| v.asset.clone()) else {
            return;
        };
        let content = Rect::new(
            data.content[0],
            data.content[1],
            (data.rect.size.width - data.insets[0]).max(0.0),
            (data.rect.size.height - data.insets[1]).max(0.0),
        );
        let key = (
            [
                content.origin.x.to_bits(),
                content.origin.y.to_bits(),
                content.size.width.to_bits(),
                content.size.height.to_bits(),
                self.scale.to_bits(),
            ],
            Arc::as_ptr(&asset) as usize,
        );
        let fresh = !matches!(self.vector_meshes.get(&id.0), Some(c) if c.key == key);
        if fresh {
            let items = prepare(&asset, content, self.scale);
            self.vector_meshes.insert(id.0, CachedVector { key, items });
        }
        for it in &self.vector_meshes[&id.0].items {
            let (slot, gradient) = match &it.paint {
                Prepaint::Solid(c) => (w.paint(*c), false),
                Prepaint::Gradient(g) => (w.gradient(g), true),
            };
            w.mesh(&it.mesh.vertices, &it.mesh.indices, slot, gradient);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_boxes_fit_centered() {
        // 24 x 12 into 48 x 48: scale 2, centered vertically.
        let m = fit([0.0, 0.0, 24.0, 12.0], Rect::new(10.0, 20.0, 48.0, 48.0));
        assert_eq!(m, Affine([2.0, 0.0, 0.0, 2.0, 10.0, 32.0]));
        // An offset view box moves its origin to the content's.
        let m = fit([5.0, 5.0, 10.0, 10.0], Rect::new(0.0, 0.0, 10.0, 10.0));
        assert_eq!(m, Affine([1.0, 0.0, 0.0, 1.0, -5.0, -5.0]));
        assert_eq!(
            intrinsic([0.0, 0.0, 24.0, 12.0], [None, None]),
            [24.0, 12.0]
        );
        assert_eq!(
            intrinsic([0.0, 0.0, 24.0, 12.0], [Some(48.0), None]),
            [48.0, 24.0]
        );
        assert_eq!(
            intrinsic([0.0, 0.0, 24.0, 12.0], [None, Some(6.0)]),
            [12.0, 6.0]
        );
    }
}

#[cfg(test)]
mod node_tests {
    use crate::geom::Size;
    use crate::host::NodeId;
    use crate::mutation::{NodeKind, Transaction};
    use crate::ui::Ui;
    use craie_core::geom::Affine;
    use craie_vector::asset::{Asset, Item, ItemStyle, encode};
    use craie_vector::{FillRule, Paint, Path};

    const NIL: u32 = u32::MAX;

    /// A 24 x 12 view box holding a red 24 x 12 rectangle.
    pub(crate) fn red_box() -> Vec<u8> {
        encode(&Asset {
            view_box: [0.0, 0.0, 24.0, 12.0],
            paints: vec![Paint::Solid(0xFF00_00FF)],
            items: vec![Item {
                path: Path::rect(0.0, 0.0, 24.0, 12.0),
                style: ItemStyle::Fill(FillRule::NonZero),
                paint: 0,
                opacity: 0.5,
                transform: Affine::IDENTITY,
            }],
        })
    }

    fn ui_with(style: taffy::Style, asset: &[u8]) -> Ui {
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        let column = taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::START),
            ..taffy::Style::default()
        };
        t.create(0, NodeKind::View)
            .layout(0, &column)
            .place(NIL, 0, NIL);
        t.create(1, NodeKind::Vector)
            .layout(1, &style)
            .payload(1, asset.to_vec())
            .place(0, 1, NIL);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 200.0));
        ui
    }

    /// Unsized, a vector node takes its view box's size; the mesh fills
    /// its content box, and opacity scales the paint's alpha.
    #[test]
    fn vectors_size_to_their_view_box() {
        let ui = ui_with(taffy::Style::default(), &red_box());
        let r = ui.layouts.data(NodeId(1)).rect;
        assert_eq!((r.size.width, r.size.height), (24.0, 12.0));
        let drawn: Vec<_> = crate::scene::Scene::resolve(ui.scene(), &|_| 0)
            .into_iter()
            .filter(|p| p.kind == 2)
            .collect();
        assert_eq!(drawn.len(), 1);
        assert_eq!(
            drawn[0].bounds,
            craie_core::geom::Rect::new(0.0, 0.0, 24.0, 12.0)
        );
        assert_eq!(drawn[0].color, 0xFF00_0080);
    }

    /// A set width scales the height (aspect kept); a box of another
    /// shape centers the drawing inside its content box (padding in).
    #[test]
    fn vectors_fit_centered_in_their_box() {
        let sized = |w: Option<f32>, h: Option<f32>, pad: f32| taffy::Style {
            size: taffy::Size {
                width: w.map_or(taffy::Dimension::auto(), taffy::Dimension::length),
                height: h.map_or(taffy::Dimension::auto(), taffy::Dimension::length),
            },
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(pad),
                right: taffy::LengthPercentage::length(pad),
                top: taffy::LengthPercentage::length(pad),
                bottom: taffy::LengthPercentage::length(pad),
            },
            ..taffy::Style::default()
        };
        let ui = ui_with(sized(Some(48.0), None, 0.0), &red_box());
        let r = ui.layouts.data(NodeId(1)).rect;
        assert_eq!((r.size.width, r.size.height), (48.0, 24.0));
        // 60 x 60 with 5 of padding: a 50 x 50 content box; the 2:1
        // drawing scales to 50 x 25 and centers (y 17.5..42.5).
        let ui = ui_with(sized(Some(60.0), Some(60.0), 5.0), &red_box());
        let drawn: Vec<_> = ui
            .scene()
            .resolve(&|_| 0)
            .into_iter()
            .filter(|p| p.kind == 2)
            .collect();
        assert_eq!(
            drawn[0].bounds,
            craie_core::geom::Rect::new(5.0, 17.5, 50.0, 25.0)
        );
    }

    /// Payloads that are not assets, and payloads on nodes that take
    /// none, reject the whole transaction.
    #[test]
    fn bad_vector_payloads_reject() {
        let mut ui = ui_with(taffy::Style::default(), &red_box());
        let seq = ui.seq;
        let mut bad = red_box();
        bad.truncate(bad.len() - 1);
        for (id, bytes) in [(1, bad), (1, b"nope".to_vec()), (0, red_box())] {
            let mut t = Transaction::new(seq + 1);
            t.payload(id, bytes);
            assert!(ui.apply_txn(&t).is_err());
            assert_eq!(ui.seq, seq);
        }
        assert!(ui.host.vectors[&1].asset.is_some());
    }

    /// Meshes are cached: a box color change rebuilds the chunk without
    /// tessellating again; a new asset or a new size does tessellate.
    #[test]
    fn vector_meshes_are_cached() {
        let mut ui = ui_with(taffy::Style::default(), &red_box());
        let items = |ui: &Ui| ui.vector_meshes[&1].items.as_ptr();
        let before = items(&ui);
        let mut t = Transaction::new(2);
        t.fill(1, 0x0000_FFFF);
        ui.apply_txn(&t).unwrap();
        let built = ui.counters().chunks_built;
        ui.render(Size::new(200.0, 200.0));
        assert!(ui.counters().chunks_built > built, "the chunk rebuilt");
        assert_eq!(items(&ui), before, "no new tessellation");
        let mut t = Transaction::new(3);
        let wide = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(96.0),
                height: taffy::Dimension::auto(),
            },
            ..taffy::Style::default()
        };
        t.layout(1, &wide);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 200.0));
        assert_ne!(items(&ui), before, "a new size tessellates");
        let r = ui.layouts.data(NodeId(1)).rect;
        assert_eq!((r.size.width, r.size.height), (96.0, 48.0));
    }
}

#[cfg(test)]
mod review_tests {
    use super::node_tests::red_box;
    use crate::geom::Size;
    use crate::host::NodeId;
    use crate::mutation::{NodeKind, Transaction};
    use crate::ui::Ui;
    use craie_core::geom::Affine;
    use craie_vector::asset::{Asset, Item, ItemStyle, encode};
    use craie_vector::{FillRule, Paint, Path};

    const NIL: u32 = u32::MAX;

    fn one(asset: Vec<u8>, style: taffy::Style, scale: f32) -> Ui {
        let mut ui = Ui::new(scale);
        let mut t = Transaction::new(1);
        let column = taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::START),
            ..taffy::Style::default()
        };
        t.create(0, NodeKind::View)
            .layout(0, &column)
            .place(NIL, 0, NIL);
        t.create(1, NodeKind::Vector)
            .layout(1, &style)
            .payload(1, asset)
            .place(0, 1, NIL);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(300.0, 300.0));
        ui
    }

    fn sized(w: f32, h: Option<f32>) -> taffy::Style {
        taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(w),
                height: h.map_or(taffy::Dimension::auto(), taffy::Dimension::length),
            },
            ..taffy::Style::default()
        }
    }

    fn circle(view: f32, style: ItemStyle, transform: Affine) -> Vec<u8> {
        encode(&Asset {
            view_box: [0.0, 0.0, view, view],
            paints: vec![Paint::Solid(0xFFFF_FFFF)],
            items: vec![Item {
                path: Path::circle(view / 2.0, view / 2.0, view / 2.0),
                style,
                paint: 0,
                opacity: 1.0,
                transform,
            }],
        })
    }

    /// Triangle area of the node's meshes, chunk units.
    fn area(ui: &Ui) -> f64 {
        ui.vector_meshes[&1]
            .items
            .iter()
            .map(|p| p.mesh.area())
            .sum()
    }

    /// S5B-05: a set width is the border box: padding comes off before
    /// the aspect ratio gives the height (24 x 12 at width 48, padding 5:
    /// content 38 x 19, box 48 x 29).
    #[test]
    fn padding_comes_off_before_the_aspect() {
        let mut s = sized(48.0, None);
        s.padding = taffy::Rect {
            left: taffy::LengthPercentage::length(5.0),
            right: taffy::LengthPercentage::length(5.0),
            top: taffy::LengthPercentage::length(5.0),
            bottom: taffy::LengthPercentage::length(5.0),
        };
        let ui = one(red_box(), s, 1.0);
        let r = ui.layouts.data(NodeId(1)).rect;
        assert_eq!((r.size.width, r.size.height), (48.0, 29.0));
    }

    /// S5B-06: the tolerance holds in device px whatever the asset's
    /// units (a circle in a 0.0002 view box shown 200 wide) and under a
    /// stretching item transform (a stroked circle scaled 100 x 1).
    #[test]
    fn tolerance_holds_in_device_pixels() {
        let ui = one(
            circle(0.0002, ItemStyle::Fill(FillRule::NonZero), Affine::IDENTITY),
            sized(200.0, Some(200.0)),
            1.0,
        );
        let exact = std::f64::consts::PI * 100.0 * 100.0;
        // Flattening loses less than perimeter x tolerance (0.25 px).
        let lost = exact - area(&ui);
        let bound = 2.0 * std::f64::consts::PI * 100.0 * 0.25;
        assert!((0.0..bound).contains(&lost), "{lost}");
        // Strokes under a stretching transform tessellate in item space
        // at a quarter device px over the largest stretch: that stretch
        // is the matrix's largest singular value.
        let close = |m: Affine, want: f32| (super::max_stretch(&m) - want).abs() < 1e-4 * want;
        assert!(close(Affine::scale(100.0, 1.0), 100.0));
        assert!(close(
            Affine::rotate(0.7).mul(&Affine::scale(3.0, 0.5)),
            3.0
        ));
        // A shear [1 1; 0 1]: the golden ratio.
        assert!(close(Affine([1.0, 0.0, 1.0, 1.0, 0.0, 0.0]), 1.618_034));
    }

    /// S5B-11: a drawing clips to its viewport, as an embedded SVG
    /// does: a rect running past the view box (x 30..70 in 40 x 40) is
    /// cut at its edge.
    #[test]
    fn drawings_clip_to_their_viewport() {
        let asset = encode(&Asset {
            view_box: [0.0, 0.0, 40.0, 40.0],
            paints: vec![Paint::Solid(0xFF00_00FF)],
            items: vec![Item {
                path: Path::rect(30.0, 5.0, 40.0, 10.0),
                style: ItemStyle::Fill(FillRule::NonZero),
                paint: 0,
                opacity: 1.0,
                transform: Affine::IDENTITY,
            }],
        });
        let ui = one(asset, taffy::Style::default(), 1.0);
        let mesh = &ui.vector_meshes[&1].items[0].mesh;
        assert!(mesh.vertices.iter().all(|v| v[0] <= 40.0));
        // The kept part: 10 x 10.
        assert!((mesh.area() - 100.0).abs() < 1e-3, "{}", mesh.area());
    }

    /// S5B-12: min and max sizes keep the aspect ratio (24 x 12: max
    /// width 12 gives 12 x 6; min width 48 gives 48 x 24; max height 3
    /// gives 6 x 3).
    #[test]
    fn size_limits_keep_the_aspect() {
        let limited = |f: &dyn Fn(&mut taffy::Style)| {
            let mut s = taffy::Style::default();
            f(&mut s);
            let ui = one(red_box(), s, 1.0);
            let r = ui.layouts.data(NodeId(1)).rect;
            (r.size.width, r.size.height)
        };
        let l = taffy::LengthPercentageAuto::length;
        assert_eq!(limited(&|s| s.max_size.width = l(12.0)), (12.0, 6.0));
        assert_eq!(limited(&|s| s.min_size.width = l(48.0)), (48.0, 24.0));
        assert_eq!(limited(&|s| s.max_size.height = l(3.0)), (6.0, 3.0));
    }

    /// S5B-04: a display-scale change re-tessellates vector chunks (the
    /// tolerance is in device px): the meshes equal a fresh build's.
    #[test]
    fn scale_changes_retessellate() {
        let asset = circle(24.0, ItemStyle::Fill(FillRule::NonZero), Affine::IDENTITY);
        let mut ui = one(asset.clone(), sized(96.0, None), 1.0);
        let at_1x = ui.vector_meshes[&1].items[0].mesh.vertices.len();
        ui.scale = 2.0;
        ui.render(Size::new(300.0, 300.0));
        let fresh = one(asset, sized(96.0, None), 2.0);
        let (now, want) = (
            ui.vector_meshes[&1].items[0].mesh.vertices.len(),
            fresh.vector_meshes[&1].items[0].mesh.vertices.len(),
        );
        assert!(want > at_1x);
        assert_eq!(now, want);
    }
}

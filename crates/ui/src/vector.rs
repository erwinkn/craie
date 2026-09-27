//! Vector nodes (ARCHITECTURE.md section 9): an asset fitted into the
//! content box, centered with its aspect ratio kept (SVG `xMidYMid
//! meet`). The asset is a prepared `CRV1` payload or a runtime drawing
//! (`craie_vector::svg`); nodes with the same source share one. Each
//! item tessellates in its own space at a tolerance of a quarter device
//! pixel, then maps into the chunk. Meshes are cached per asset, content
//! box and display scale, and shared by the nodes drawing that asset in
//! a box of that size (a list of rows with the same icon tessellates it
//! once). An item's opacity (a shape's, or a `G`'s folded in) multiplies
//! its paint's alpha; the node's own opacity is its layer's (group
//! opacity). Items outside the viewport are skipped before tessellating,
//! and flattening never goes finer than an item's size over
//! `MAX_DETAIL`, so a huge shape costs no more than a small one.
//!
//! An item painting with `currentColor` (`Paint::Current`) resolves to
//! the node's inherited color at paint (`Host::current_color`). Its
//! paint slot comes right after the box's two, so a color change
//! patches those slots and nothing is tessellated or rebuilt.

use std::collections::HashMap;
use std::sync::Arc;

use craie_core::geom::{Affine, Rect};
use craie_scene::{ChunkWriter, GradientPaint, PaintSlot, gradient};
use craie_vector::asset::{Asset, ItemStyle};
use craie_vector::{Mesh, Paint};

use crate::host::NodeId;
use crate::layout::LayoutData;
use crate::ui::Ui;

/// Device px of flattening error allowed.
const TOLERANCE_PX: f32 = 0.25;

/// The first `currentColor` slot: after the box's fill and border
/// (`build_chunk`).
const CURRENT_SLOTS: u32 = 2;

/// A tessellated item in chunk-local units, and its paint.
pub(crate) struct Prepared {
    pub(crate) mesh: Mesh,
    paint: Prepaint,
}

enum Prepaint {
    Solid(u32),
    /// The inherited color, times the item's tint in `Meshes::tints`.
    Current,
    Gradient(GradientPaint),
}

/// A drawing's prepared items, and the tints of its `currentColor`
/// items in item order (the item's opacity folded into the alpha), so
/// a color change walks only those.
pub(crate) struct Meshes {
    items: Vec<Prepared>,
    tints: Vec<u32>,
}

impl std::ops::Deref for Meshes {
    type Target = [Prepared];

    fn deref(&self) -> &[Prepared] {
        &self.items
    }
}

/// (content box, display scale) bits and the asset's address.
type MeshKey = ([u32; 5], usize);

/// Tessellated vector meshes.
#[derive(Default)]
pub(crate) struct VectorCache {
    /// Each node's meshes and the key they were made for.
    nodes: HashMap<u32, (MeshKey, Arc<Meshes>)>,
    /// Meshes by key, shared between nodes. An entry holds its asset,
    /// so the address in its key stays that asset's.
    shared: HashMap<MeshKey, (Arc<Asset>, Arc<Meshes>)>,
    /// `shared` entries after the last sweep of unused ones.
    swept: usize,
}

impl VectorCache {
    /// A node's meshes, if built (tests).
    #[cfg(test)]
    pub(crate) fn items(&self, id: u32) -> Option<&Arc<Meshes>> {
        self.nodes.get(&id).map(|(_, items)| items)
    }

    pub(crate) fn forget(&mut self, id: u32) {
        self.nodes.remove(&id);
    }

    /// The meshes of `asset` for `key`: the node's own, another node's,
    /// or new. Unused shared entries are swept when the table has doubled
    /// since the last sweep.
    fn get(
        &mut self,
        id: u32,
        key: MeshKey,
        asset: &Arc<Asset>,
        build: impl FnOnce() -> Meshes,
    ) -> Arc<Meshes> {
        if let Some((k, items)) = self.nodes.get(&id)
            && *k == key
        {
            return items.clone();
        }
        let items = match self.shared.get(&key) {
            Some((_, items)) => items.clone(),
            None => {
                let items = Arc::new(build());
                self.shared.insert(key, (asset.clone(), items.clone()));
                items
            }
        };
        self.nodes.insert(id, (key, items.clone()));
        if self.shared.len() > 2 * self.swept + 64 {
            self.shared
                .retain(|_, (_, items)| Arc::strong_count(items) > 1);
            self.swept = self.shared.len();
        }
        items
    }

    /// Shared entries (tests).
    #[cfg(test)]
    pub(crate) fn shared_len(&self) -> usize {
        self.shared.len()
    }
}

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

/// Two 0xRRGGBBAA colors multiplied channel by channel (white is the
/// identity).
fn tint(color: u32, by: u32) -> u32 {
    let mut out = 0;
    for shift in [24, 16, 8, 0] {
        let (a, b) = ((color >> shift) & 0xFF, (by >> shift) & 0xFF);
        out |= ((a * b + 127) / 255) << shift;
    }
    out
}

/// `alpha` of a 0xRRGGBBAA color scaled by `opacity`.
fn fade(color: u32, opacity: f32) -> u32 {
    let a = ((color & 0xFF) as f32 * opacity).round().clamp(0.0, 255.0) as u32;
    (color & !0xFF) | a
}

/// The bounds of `path`'s points and control points under `m` (they
/// hold the curve), `None` if empty or not finite.
fn bounds(path: &craie_vector::Path, m: &Affine) -> Option<Rect> {
    let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
    for v in &path.verbs {
        let pts: &[[f32; 2]] = match v {
            craie_vector::Verb::MoveTo(p) | craie_vector::Verb::LineTo(p) => {
                std::slice::from_ref(p)
            }
            craie_vector::Verb::QuadTo(c, p) => &[*c, *p],
            craie_vector::Verb::CubicTo(a, b, p) => &[*a, *b, *p],
            craie_vector::Verb::Close => &[],
        };
        for &[x, y] in pts {
            let p = m.apply(craie_core::geom::Point::new(x, y));
            lo = [lo[0].min(p.x), lo[1].min(p.y)];
            hi = [hi[0].max(p.x), hi[1].max(p.y)];
        }
    }
    let r = Rect::new(lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]);
    (r.size.width.is_finite() && r.size.height.is_finite()).then_some(r)
}

/// Finest flattening, as a fraction of an item's size: past it, a shape
/// far larger than its viewport gets coarser, not more vertices.
const MAX_DETAIL: f32 = 65_536.0;

/// The chunk-space tolerance for an item with chunk-space `bounds`: a
/// quarter device px, or its size over `MAX_DETAIL` if coarser.
fn chunk_tolerance(scale: f32, bounds: Rect) -> f32 {
    (TOLERANCE_PX / scale).max(bounds.size.width.hypot(bounds.size.height) / MAX_DETAIL)
}

fn prepare(asset: &Asset, content: Rect, scale: f32) -> Meshes {
    let place = fit(asset.view_box, content);
    // The view box in chunk space: the drawing's viewport.
    let [vx, vy, vw, vh] = asset.view_box;
    let p0 = place.apply(craie_core::geom::Point::new(vx, vy));
    let p1 = place.apply(craie_core::geom::Point::new(vx + vw, vy + vh));
    let viewport = Rect::new(p0.x, p0.y, p1.x - p0.x, p1.y - p0.y);
    let mut out = Vec::with_capacity(asset.items.len());
    let mut tints = Vec::new();
    // One dash budget for the whole drawing.
    let mut dash_budget = craie_vector::MAX_DASH_SEGMENTS;
    for it in &asset.items {
        if !(it.opacity > 0.0) {
            continue;
        }
        let m = place.mul(&it.transform);
        let det = m.determinant().abs();
        if !(det > 1e-12 && det.is_finite()) {
            continue;
        }
        // Items entirely outside the viewport are clipped away: skip
        // them before tessellating (a stroke reaches half its width out,
        // times the miter or a square cap's corner).
        let stretch = max_stretch(&m);
        let reach = match it.style {
            ItemStyle::Fill(_) => 0.0,
            ItemStyle::Stroke(s) => s.width * 0.5 * stretch * s.miter_limit.max(1.5),
        };
        let Some(b) = bounds(&it.path, &m) else {
            continue;
        };
        let b = Rect::new(
            b.origin.x - reach,
            b.origin.y - reach,
            b.size.width + 2.0 * reach,
            b.size.height + 2.0 * reach,
        );
        let outside = b.max_x() < viewport.origin.x
            || b.origin.x > viewport.max_x()
            || b.max_y() < viewport.origin.y
            || b.origin.y > viewport.max_y();
        if outside || !b.size.width.is_finite() || !b.size.height.is_finite() {
            continue;
        }
        // Fills (and strokes under a similarity: equal axes, no shear)
        // tessellate in chunk space, where the tolerance is a quarter
        // device px whatever the asset's units, or the item's size over
        // `MAX_DETAIL` if coarser. Other strokes must tessellate in item
        // space (a stroke's width follows the transform): there the
        // tolerance is that over the transform's largest stretch.
        let [a, b_, c, d, _, _] = m.0;
        let k = (a * a + b_ * b_).sqrt();
        let similar = (a - d).abs() <= 1e-4 * k && (b_ + c).abs() <= 1e-4 * k;
        let chunk_tolerance = chunk_tolerance(scale, b);
        let mesh = match it.style {
            ItemStyle::Fill(rule) => {
                craie_vector::fill(&it.path.transformed(&m), rule, chunk_tolerance)
            }
            // Dashes cut the path where it strokes, in that space's units
            // (a pattern that would cut too many draws solid).
            ItemStyle::Stroke(s) if similar => {
                let s = craie_vector::Stroke {
                    width: s.width * k,
                    ..s
                };
                let path = it.path.transformed(&m);
                let dashed = it.dash.as_ref().and_then(|d| {
                    craie_vector::dashed(&path, &d.scaled(k), chunk_tolerance, &mut dash_budget)
                });
                craie_vector::stroke(dashed.as_ref().unwrap_or(&path), &s, chunk_tolerance)
            }
            ItemStyle::Stroke(s) => {
                let tolerance = chunk_tolerance / stretch;
                let dashed = it
                    .dash
                    .as_ref()
                    .and_then(|d| craie_vector::dashed(&it.path, d, tolerance, &mut dash_budget));
                let path = dashed.as_ref().unwrap_or(&it.path);
                craie_vector::stroke(path, &s, tolerance).map(|mut mesh| {
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
            Paint::Current(t) => {
                tints.push(fade(*t, it.opacity));
                Prepaint::Current
            }
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
    Meshes { items: out, tints }
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
        let scale = self.scale;
        let items = self
            .vector_meshes
            .get(id.0, key, &asset, || prepare(&asset, content, scale));
        // The inherited color's slots first, in item order, after the
        // box's: `patch_vector_paint` finds them there.
        let current = self.host.current_color(id);
        for (i, &t) in items.tints.iter().enumerate() {
            let slot = w.paint(tint(current, t));
            debug_assert_eq!(slot, PaintSlot(CURRENT_SLOTS + i as u32));
        }
        let mut next = CURRENT_SLOTS;
        for it in items.iter() {
            let (slot, gradient) = match &it.paint {
                Prepaint::Solid(c) => (w.paint(*c), false),
                Prepaint::Current => {
                    next += 1;
                    (PaintSlot(next - 1), false)
                }
                Prepaint::Gradient(g) => (w.gradient(g), true),
            };
            w.mesh(&it.mesh.vertices, &it.mesh.indices, slot, gradient);
        }
    }

    /// Repaints a vector node's `currentColor` items with its inherited
    /// color: one paint record each, no tessellation, no chunk rebuild.
    pub(crate) fn patch_vector_paint(&mut self, id: NodeId) {
        let Some(items) = self.vector_meshes.nodes.get(&id.0).map(|(_, i)| i) else {
            return;
        };
        let current = self.host.current_color(id);
        for (i, &t) in items.tints.iter().enumerate() {
            let slot = PaintSlot(CURRENT_SLOTS + i as u32);
            self.scene.set_paint(id.0, slot, tint(current, t));
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
                dash: None,
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
        let items = |ui: &Ui| ui.vector_meshes.items(1).unwrap().as_ptr();
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
                dash: None,
            }],
        })
    }

    /// Triangle area of the node's meshes, chunk units.
    fn area(ui: &Ui) -> f64 {
        ui.vector_meshes
            .items(1)
            .unwrap()
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
                dash: None,
            }],
        });
        let ui = one(asset, taffy::Style::default(), 1.0);
        let mesh = &ui.vector_meshes.items(1).unwrap()[0].mesh;
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

    /// S5B-16: limits resolve against the containing block (percent
    /// sizes and padding) and follow box sizing, in a 100-wide parent:
    /// max width 10% gives 10 x 5; min width 48 with 5% padding gives 48
    /// x 29; content-box min width 48 with padding 5 gives 58 x 34.
    #[test]
    fn size_limits_follow_percentages_and_box_sizing() {
        let limited = |f: &dyn Fn(&mut taffy::Style)| {
            let mut ui = Ui::new(1.0);
            let mut t = Transaction::new(1);
            let parent = taffy::Style {
                flex_direction: taffy::FlexDirection::Column,
                align_items: Some(taffy::AlignItems::START),
                size: taffy::Size {
                    width: taffy::Dimension::length(100.0),
                    height: taffy::Dimension::length(100.0),
                },
                ..taffy::Style::default()
            };
            let mut s = taffy::Style::default();
            f(&mut s);
            t.create(0, NodeKind::View)
                .layout(0, &parent)
                .place(NIL, 0, NIL);
            t.create(1, NodeKind::Vector)
                .layout(1, &s)
                .payload(1, red_box())
                .place(0, 1, NIL);
            ui.apply_txn(&t).unwrap();
            ui.render(Size::new(300.0, 300.0));
            let r = ui.layouts.data(NodeId(1)).rect;
            (r.size.width, r.size.height)
        };
        let pad = |p: taffy::LengthPercentage| taffy::Rect {
            left: p,
            right: p,
            top: p,
            bottom: p,
        };
        let l = taffy::LengthPercentageAuto::length;
        assert_eq!(
            limited(&|s| s.max_size.width = taffy::LengthPercentageAuto::percent(0.1)),
            (10.0, 5.0)
        );
        assert_eq!(
            limited(&|s| {
                s.min_size.width = l(48.0);
                s.padding = pad(taffy::LengthPercentage::percent(0.05));
            }),
            (48.0, 29.0)
        );
        assert_eq!(
            limited(&|s| {
                s.min_size.width = l(48.0);
                s.padding = pad(taffy::LengthPercentage::length(5.0));
                s.box_sizing = taffy::BoxSizing::ContentBox;
            }),
            (58.0, 34.0)
        );
    }

    /// S5B-04: a display-scale change re-tessellates vector chunks (the
    /// tolerance is in device px): the meshes equal a fresh build's.
    #[test]
    fn scale_changes_retessellate() {
        let asset = circle(24.0, ItemStyle::Fill(FillRule::NonZero), Affine::IDENTITY);
        let mut ui = one(asset.clone(), sized(96.0, None), 1.0);
        let at_1x = ui.vector_meshes.items(1).unwrap()[0].mesh.vertices.len();
        ui.scale = 2.0;
        ui.render(Size::new(300.0, 300.0));
        let fresh = one(asset, sized(96.0, None), 2.0);
        let (now, want) = (
            ui.vector_meshes.items(1).unwrap()[0].mesh.vertices.len(),
            fresh.vector_meshes.items(1).unwrap()[0].mesh.vertices.len(),
        );
        assert!(want > at_1x);
        assert_eq!(now, want);
    }
}

#[cfg(test)]
mod drawing_tests {
    use super::node_tests::red_box;
    use crate::geom::Size;
    use crate::host::NodeId;
    use crate::mutation::{NodeKind, Transaction};
    use crate::ui::Ui;
    use crate::wire;
    use craie_vector::Stroke;
    use craie_vector::svg::{Drawing, Shape, ShapeKind};

    const NIL: u32 = u32::MAX;

    /// A 24-unit ring (Lucide's circle, r 10) stroked 2 wide, optionally
    /// dashed.
    fn ring(dashes: &str) -> Drawing<'static> {
        Drawing {
            view_box: "0 0 24 24".into(),
            shapes: vec![Shape {
                geometry: "M22 12A10 10 0 0 1 2 12A10 10 0 0 1 22 12Z".into(),
                fill: 0,
                stroke: 0xFFFF_FFFF,
                line: Stroke {
                    width: 2.0,
                    ..Stroke::default()
                },
                dashes: dashes.to_string().into(),
                ..Shape::default()
            }],
        }
    }

    /// `count` vector nodes of 48 x 48 in a column, each drawing
    /// `drawing`.
    fn ui_with(drawing: &Drawing<'static>, count: u32) -> Ui {
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        let column = taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::START),
            ..taffy::Style::default()
        };
        let sized = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(48.0),
                height: taffy::Dimension::length(48.0),
            },
            ..taffy::Style::default()
        };
        t.create(0, NodeKind::View)
            .layout(0, &column)
            .place(NIL, 0, NIL);
        for id in 1..=count {
            t.create(id, NodeKind::Vector)
                .layout(id, &sized)
                .drawing(id, drawing.clone())
                .place(0, id, NIL);
        }
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 400.0));
        ui
    }

    fn area(ui: &Ui, id: u32) -> f64 {
        ui.vector_meshes
            .items(id)
            .unwrap()
            .iter()
            .map(|p| p.mesh.area())
            .sum()
    }

    /// A ring drawn at 48 px (scale 2 from its 24 view box): 4 px wide
    /// around radius 20; half dashed ("π·5 π·5" per 20π·... units), it
    /// covers half as much.
    #[test]
    fn drawings_render_and_dash() {
        let solid = ui_with(&ring(""), 1);
        let r = solid.layouts.data(NodeId(1)).rect;
        assert_eq!((r.size.width, r.size.height), (48.0, 48.0));
        let want = std::f64::consts::PI * (22.0f64.powi(2) - 18.0f64.powi(2));
        let full = area(&solid, 1);
        assert!((full - want).abs() / want < 0.01, "{full} vs {want}");
        // The circumference in view-box units is 20π; eight dashes.
        let d = std::f32::consts::PI * 20.0 / 16.0;
        let dashed = ui_with(&ring(&format!("{d} {d}")), 1);
        let half = area(&dashed, 1);
        assert!((half - full / 2.0).abs() / full < 0.02, "{half} vs {full}");
    }

    /// Nodes drawing the same shapes share one parse and one
    /// tessellation; a new drawing on one of them parses and tessellates
    /// its own.
    #[test]
    fn same_drawings_share_assets_and_meshes() {
        let mut ui = ui_with(&ring("4 2"), 3);
        let asset = |ui: &Ui, id: u32| ui.host.vectors[&id].asset.clone().unwrap();
        assert!(std::sync::Arc::ptr_eq(&asset(&ui, 1), &asset(&ui, 3)));
        let items = |ui: &Ui, id: u32| ui.vector_meshes.items(id).unwrap().as_ptr();
        assert_eq!(items(&ui, 1), items(&ui, 2));
        assert_eq!(items(&ui, 1), items(&ui, 3));
        assert_eq!(ui.vector_meshes.shared_len(), 1);
        let mut t = Transaction::new(2);
        t.drawing(2, ring(""));
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 400.0));
        assert_ne!(items(&ui, 1), items(&ui, 2));
        assert!(!std::sync::Arc::ptr_eq(&asset(&ui, 1), &asset(&ui, 2)));
        assert!(area(&ui, 2) > area(&ui, 1));
    }

    /// A drawing replaces an asset payload and the other way around.
    #[test]
    fn drawings_and_payloads_replace_each_other() {
        let mut ui = ui_with(&ring(""), 1);
        let mut t = Transaction::new(2);
        t.payload(1, red_box());
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 400.0));
        assert_eq!(
            ui.host.vectors[&1].asset.as_ref().unwrap().view_box[2],
            24.0
        );
        assert_eq!(
            ui.host.vectors[&1].asset.as_ref().unwrap().view_box[3],
            12.0
        );
        let mut t = Transaction::new(3);
        t.drawing(1, ring(""));
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 400.0));
        assert_eq!(
            ui.host.vectors[&1].asset.as_ref().unwrap().view_box[3],
            24.0
        );
    }

    /// PR5-01: a drawing whose strings or values do not parse draws
    /// nothing, on the wire as in the direct API; the transaction
    /// applies and the session goes on. A good drawing then draws again.
    #[test]
    fn bad_values_draw_nothing() {
        let mut ui = ui_with(&ring(""), 1);
        let with = |f: &dyn Fn(&mut Shape<'static>)| {
            let mut d = ring("");
            f(&mut d.shapes[0]);
            d
        };
        let cases = [
            with(&|s| s.geometry = "M1 2L".into()),
            // A chart point that is NaN, and `<Circle r={undefined}>`.
            with(&|s| s.geometry = "M0 0 L1 NaN".into()),
            with(&|s| s.geometry = "MNaN 12ANaN NaN 0 1 1 NaN 12Z".into()),
            with(&|s| s.transform = "rotate(".into()),
            with(&|s| s.dashes = "4 -2".into()),
            with(&|s| {
                s.kind = ShapeKind::Polyline;
                s.geometry = "0 0 1".into();
            }),
            with(&|s| s.line.width = f32::INFINITY),
            with(&|s| s.line.width = -1.0),
            with(&|s| s.opacity = 1.2),
            with(&|s| s.line.miter_limit = 0.5),
            // A chart whose values are all equal: an empty view box.
            Drawing {
                view_box: "0 0 600 0".into(),
                ..ring("")
            },
        ];
        for (k, d) in cases.into_iter().enumerate() {
            let mut t = Transaction::new(ui.seq + 1);
            t.drawing(1, d);
            assert!(ui.apply(&wire::encode(&t)).is_ok(), "case {k} on the wire");
            ui.render(Size::new(200.0, 400.0));
            assert!(ui.host.vectors[&1].asset.is_none(), "case {k}");
            let mut t = Transaction::new(ui.seq + 1);
            t.drawing(1, ring(""));
            ui.apply_txn(&t).unwrap();
            ui.render(Size::new(200.0, 400.0));
            assert!(ui.vector_meshes.items(1).is_some_and(|i| !i.is_empty()));
        }
    }

    /// PR5-01, PR5-02: structural errors still reject: a drawing on
    /// another kind of node, more than `MAX_SHAPES` shapes, and strings
    /// adding up past `MAX_BYTES` (4,096 shapes sharing one 2 KiB path,
    /// which the wire's string table sends once).
    #[test]
    fn structural_errors_reject() {
        use craie_vector::svg::{MAX_BYTES, MAX_SHAPES};
        let mut ui = ui_with(&ring(""), 1);
        let seq = ui.seq;
        let wide = Drawing {
            view_box: "0 0 1 1".into(),
            shapes: vec![ring("").shapes[0].clone(); MAX_SHAPES + 1],
        };
        let path = "M0 0".to_string() + &" ".repeat(2044);
        let long = Drawing {
            view_box: "0 0 1 1".into(),
            shapes: vec![
                Shape {
                    geometry: path.into(),
                    ..Shape::default()
                };
                MAX_SHAPES
            ],
        };
        const { assert!(MAX_SHAPES * 2048 > MAX_BYTES) };
        let buf = {
            let mut t = Transaction::new(seq + 1);
            t.drawing(1, long.clone());
            wire::encode(&t)
        };
        assert!(buf.len() < 256 * 1024, "{} wire bytes", buf.len());
        for (id, d) in [(1, wide), (1, long), (0, ring(""))] {
            let mut t = Transaction::new(seq + 1);
            t.drawing(id, d);
            assert!(ui.apply(&wire::encode(&t)).is_err());
            assert!(ui.apply_txn(&t).is_err());
            assert_eq!(ui.seq, seq);
        }
        assert!(ui.host.vectors[&1].asset.is_some());
    }

    /// PR5-02: a drawing's key is built once and shared: nodes drawing
    /// the same shapes hold one copy, in the same batch or a later one.
    #[test]
    fn equal_drawings_share_one_key() {
        let mut ui = ui_with(&ring("4 2"), 3);
        let bytes = |ui: &Ui, id: u32| ui.host.vectors[&id].bytes.clone();
        assert!(std::sync::Arc::ptr_eq(&bytes(&ui, 1), &bytes(&ui, 3)));
        let mut t = Transaction::new(2);
        t.create(4, NodeKind::Vector)
            .drawing(4, ring("4 2"))
            .place(0, 4, NIL);
        ui.apply_txn(&t).unwrap();
        assert!(std::sync::Arc::ptr_eq(&bytes(&ui, 1), &bytes(&ui, 4)));
    }

    /// PR5-10: a payload is tagged by its magic in the source table: one
    /// whose bytes equal a live drawing's key does not take its asset.
    #[test]
    fn payloads_cannot_pose_as_drawings() {
        let mut ui = ui_with(&ring(""), 2);
        let key = ui.host.vectors[&1].bytes.to_vec();
        assert!(key.starts_with(b"CRVS"));
        let mut t = Transaction::new(ui.seq + 1);
        t.payload(2, key);
        assert!(ui.apply_txn(&t).is_err());
    }

    /// PR5-09: churn sweeps both tables: 1,000 drawings in turn on one
    /// node leave each table bounded by its sweep threshold (twice what
    /// the last sweep kept, plus 64; meshes keep their asset alive until
    /// their own sweep), not by the churn.
    #[test]
    fn churn_sweeps_the_tables() {
        let mut ui = ui_with(&ring(""), 1);
        for i in 0..1_000 {
            let mut t = Transaction::new(ui.seq + 1);
            t.drawing(1, ring(&format!("{} 1", i + 1)));
            ui.apply_txn(&t).unwrap();
            ui.render(Size::new(200.0, 400.0));
        }
        let (sources, meshes) = (ui.host.vector_sources_len(), ui.vector_meshes.shared_len());
        assert!(
            sources < 200 && meshes < 200,
            "{sources} sources, {meshes} meshes"
        );
    }

    /// PR5-09: a display-scale change re-tessellates a shared drawing,
    /// and its nodes still share the new meshes.
    #[test]
    fn scale_changes_retessellate_shared_drawings() {
        let mut ui = ui_with(&ring(""), 2);
        let items = |ui: &Ui, id: u32| ui.vector_meshes.items(id).unwrap().clone();
        let before = items(&ui, 1);
        ui.scale = 2.0;
        ui.render(Size::new(200.0, 400.0));
        let (a, b) = (items(&ui, 1), items(&ui, 2));
        assert!(!std::sync::Arc::ptr_eq(&a, &before));
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        assert!(a[0].mesh.vertices.len() > before[0].mesh.vertices.len());
    }

    /// PR5-09: when the only node holding a source is removed earlier in
    /// the batch, a new node with that source builds it again.
    #[test]
    fn a_removed_source_rebuilds_for_a_new_node() {
        let mut ui = ui_with(&ring("4 2"), 1);
        let mut t = Transaction::new(2);
        t.remove(1)
            .create(2, NodeKind::Vector)
            .drawing(2, ring("4 2"))
            .place(0, 2, NIL);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 400.0));
        assert!(ui.host.vectors[&2].asset.is_some());
        assert!(ui.vector_meshes.items(2).is_some_and(|i| !i.is_empty()));
    }

    /// PR5-09: an id recycled while its meshes were shared starts clean:
    /// the new node draws its own drawing, the other keeps the old one.
    #[test]
    fn recycled_ids_start_clean() {
        let mut ui = ui_with(&ring(""), 2);
        let before = ui.vector_meshes.items(1).unwrap().clone();
        let mut t = Transaction::new(2);
        t.remove(2);
        ui.apply_txn(&t).unwrap();
        let mut t = Transaction::new(3);
        t.create(2, NodeKind::Vector)
            .drawing(2, ring("4 2"))
            .place(0, 2, NIL);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(200.0, 400.0));
        let (a, b) = (
            ui.vector_meshes.items(1).unwrap().clone(),
            ui.vector_meshes.items(2).unwrap().clone(),
        );
        assert!(std::sync::Arc::ptr_eq(&a, &before));
        assert!(!std::sync::Arc::ptr_eq(&a, &b));
        assert!(area(&ui, 2) < area(&ui, 1));
    }

    /// PR5-05: a shape far larger than its viewport flattens coarser
    /// rather than into more vertices: a circle of radius 1e6 or 4e6
    /// tessellates to over 5,000 vertices at the pixel tolerance for
    /// the larger, and to under 2,000 for both at its size over 2^16. The
    /// mesh is clipped to the viewport after, so this counts before the
    /// clip. One through the view box draws; one outside is skipped.
    #[test]
    fn huge_shapes_stay_cheap() {
        let geometry = |x: f32, r: f32| {
            format!(
                "M{x} 12A{r} {r} 0 1 1 {} 12A{r} {r} 0 1 1 {x} 12Z",
                x - 2.0 * r
            )
        };
        for r in [1e6, 4e6] {
            let path = craie_vector::svg::path_data(&geometry(18.0, r), 64).unwrap();
            let b = super::bounds(&path, &craie_core::geom::Affine::IDENTITY).unwrap();
            let fill =
                |tol| craie_vector::fill(&path, craie_vector::FillRule::NonZero, tol).unwrap();
            let n = fill(super::chunk_tolerance(1.0, b)).vertices.len();
            assert!((100..2_000).contains(&n), "r {r}: {n} vertices");
            if r == 4e6 {
                assert!(fill(super::TOLERANCE_PX).vertices.len() > 5_000);
            }
        }
        let circle = |x: f32, r: f32| Drawing {
            view_box: "0 0 24 24".into(),
            shapes: vec![Shape {
                geometry: geometry(x, r).into(),
                ..Shape::default()
            }],
        };
        let ui = ui_with(&circle(18.0, 4e6), 1);
        assert_eq!(ui.vector_meshes.items(1).unwrap().len(), 1);
        let ui = ui_with(&circle(1e6, 10.0), 1);
        assert!(ui.vector_meshes.items(1).unwrap().is_empty());
    }

    /// PR5-05: a shape at opacity 0 does not tessellate.
    #[test]
    fn transparent_shapes_do_not_tessellate() {
        let mut d = ring("");
        d.shapes[0].opacity = 0.0;
        let ui = ui_with(&d, 1);
        assert!(ui.vector_meshes.items(1).unwrap().is_empty());
    }

    /// DF-24: a `currentColor` paint is the node's inherited color: its
    /// own `COLOR`, else the nearest ancestor's, else white, times the
    /// shape's tint (its opacity here). Its slots come first, after the
    /// box's two; solid paints stay as drawn. Clearing a color falls back.
    #[test]
    fn current_color_resolves_nearest() {
        use craie_scene::PaintSlot;
        use craie_vector::svg::{CURRENT_FILL, CURRENT_STROKE};
        // A currentColor stroke, a solid square, then a shape whose fill
        // and stroke both inherit, with different tints: slots 2, 3, 4
        // in item order, the solid after them.
        let mut d = ring("");
        d.shapes[0].stroke = 0xFFFF_FF80;
        d.shapes[0].current = CURRENT_STROKE;
        d.shapes.push(Shape {
            geometry: "M4 4H8V8Z".into(),
            fill: 0x0000_FFFF,
            ..Shape::default()
        });
        d.shapes.push(Shape {
            geometry: "M12 12H20V20H12Z".into(),
            fill: 0xFFFF_FF40,
            stroke: 0xFFFF_FFFF,
            current: CURRENT_FILL | CURRENT_STROKE,
            line: Stroke {
                width: 1.0,
                ..Stroke::default()
            },
            ..Shape::default()
        });
        let mut ui = ui_with(&d, 1);
        let paints = |ui: &Ui| [2, 3, 4, 5].map(|s| ui.scene().paint(1, PaintSlot(s)).unwrap());
        assert_eq!(
            paints(&ui),
            [0xFFFF_FF80, 0xFFFF_FF40, 0xFFFF_FFFF, 0x0000_FFFF],
            "white, as a span"
        );
        let step = |ui: &mut Ui, id: u32, color: Option<u32>| {
            let mut t = Transaction::new(2);
            t.color(id, color);
            ui.apply_txn(&t).unwrap();
            ui.render(Size::new(200.0, 400.0));
            paints(ui)
        };
        assert_eq!(
            step(&mut ui, 0, Some(0x9AA0_AAFF)),
            [0x9AA0_AA80, 0x9AA0_AA40, 0x9AA0_AAFF, 0x0000_FFFF],
            "the parent's"
        );
        assert_eq!(
            step(&mut ui, 1, Some(0xFF00_00FF)),
            [0xFF00_0080, 0xFF00_0040, 0xFF00_00FF, 0x0000_FFFF],
            "its own wins"
        );
        assert_eq!(
            step(&mut ui, 1, None)[..3],
            [0x9AA0_AA80, 0x9AA0_AA40, 0x9AA0_AAFF]
        );
        assert_eq!(
            step(&mut ui, 0, None)[..3],
            [0xFFFF_FF80, 0xFFFF_FF40, 0xFFFF_FFFF]
        );
    }

    /// A drawing that starts using `currentColor` after the inheritors of
    /// a color were listed joins them, and one that stops leaves them.
    #[test]
    fn drawings_join_and_leave_inheritors() {
        use craie_scene::PaintSlot;
        use craie_vector::svg::CURRENT_STROKE;
        let mut ui = ui_with(&ring(""), 1);
        let apply = |ui: &mut Ui, f: &dyn Fn(&mut Transaction<'static>)| {
            let mut t = Transaction::new(2);
            f(&mut t);
            ui.apply_txn(&t).unwrap();
            ui.render(Size::new(200.0, 400.0));
        };
        apply(&mut ui, &|t| {
            t.color(0, Some(0x1111_11FF));
        });
        let mut current = ring("");
        current.shapes[0].current = CURRENT_STROKE;
        apply(&mut ui, &|t| {
            t.drawing(1, current.clone());
        });
        assert_eq!(ui.scene().paint(1, PaintSlot(2)), Some(0x1111_11FF));
        apply(&mut ui, &|t| {
            t.color(0, Some(0x2222_22FF));
        });
        assert_eq!(ui.scene().paint(1, PaintSlot(2)), Some(0x2222_22FF));
        apply(&mut ui, &|t| {
            t.drawing(1, ring(""));
        });
        let before = ui.counters();
        apply(&mut ui, &|t| {
            t.color(0, Some(0x3333_33FF));
        });
        let spent = ui.counters().since(&before);
        assert_eq!((spent.paints_patched, spent.chunks_built), (0, 0));
        assert_eq!(ui.scene().paint(1, PaintSlot(2)), Some(0xFFFF_FFFF));
    }

    /// The wire carries every field of every shape.
    #[test]
    fn drawings_round_trip_the_wire() {
        let mut d = ring("1 2 3");
        d.shapes.push(Shape {
            kind: ShapeKind::Polyline,
            geometry: "0,0 4,4".into(),
            transform: "translate(1 2)".into(),
            fill: 0x1122_3344,
            fill_rule: craie_vector::FillRule::EvenOdd,
            current: craie_vector::svg::CURRENT_FILL,
            line: Stroke {
                width: 1.5,
                join: craie_vector::LineJoin::Round,
                cap: craie_vector::LineCap::Square,
                miter_limit: 8.0,
            },
            dash_offset: -3.0,
            opacity: 0.25,
            ..Shape::default()
        });
        let mut t = Transaction::new(7);
        t.drawing(5, d);
        let buf = wire::encode(&t);
        let back = wire::decode(&buf).unwrap();
        assert_eq!(back.mutations, t.mutations);
        // Truncated inside the op, it fails to decode.
        for n in 0..buf.len() {
            let cut = wire::decode(&buf[..n]);
            assert!(cut.map_or(true, |c| c.mutations.is_empty()), "{n}");
        }
        // PR5-09: unknown kind, rule, join, cap or current bytes, and a
        // string ref past the table, fail to decode. The first shape
        // follows the op byte, the id, the view box ref and the count.
        let op = buf
            .windows(5)
            .position(|w| w == [wire::op::DRAWING, 5, 0, 0, 0])
            .unwrap();
        let shape = op + 1 + 4 + 4 + 2;
        for at in [shape, shape + 1, shape + 2, shape + 3, shape + 4] {
            let mut bad = buf.clone();
            bad[at] = 9;
            assert!(wire::decode(&bad).is_err(), "byte {}", at - shape);
        }
        let mut bad = buf.clone();
        bad[shape + 5..shape + 9].copy_from_slice(&0xFFFFu32.to_le_bytes());
        assert!(wire::decode(&bad).is_err());
    }
}

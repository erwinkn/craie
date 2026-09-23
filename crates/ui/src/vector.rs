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

/// `alpha` of a 0xRRGGBBAA color scaled by `opacity`.
fn fade(color: u32, opacity: f32) -> u32 {
    let a = ((color & 0xFF) as f32 * opacity).round().clamp(0.0, 255.0) as u32;
    (color & !0xFF) | a
}

fn prepare(asset: &Asset, content: Rect, scale: f32) -> Vec<Prepared> {
    let place = fit(asset.view_box, content);
    let mut out = Vec::with_capacity(asset.items.len());
    for it in &asset.items {
        let m = place.mul(&it.transform);
        let det = m.determinant().abs();
        if !(det > 1e-12 && det.is_finite()) {
            continue;
        }
        // Device px per item unit (geometric mean of the axes).
        let px = det.sqrt() * scale;
        let tolerance = (TOLERANCE_PX / px).max(1e-4);
        let mesh = match it.style {
            ItemStyle::Fill(rule) => craie_vector::fill(&it.path, rule, tolerance),
            ItemStyle::Stroke(s) => craie_vector::stroke(&it.path, &s, tolerance),
        };
        let Ok(mut mesh) = mesh else { continue };
        if mesh.indices.is_empty() {
            continue;
        }
        for v in &mut mesh.vertices {
            let p = m.apply(craie_core::geom::Point::new(v[0], v[1]));
            *v = [p.x, p.y];
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

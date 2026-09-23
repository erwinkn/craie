//! Build-time SVG import (ARCHITECTURE.md section 9): usvg parses and
//! normalizes the SVG (styles, `use`, units, the view box), and this
//! crate writes the result as a Craie vector asset (`craie_vector::asset`).
//! usvg stays out of the shipped binary (release graph test).
//!
//! Imported: paths with fills (nonzero, even-odd) and strokes (width,
//! joins, caps, miter limit), solid colors, linear and radial gradients
//! (pad spread, no focal point), transforms, and opacity (fill, stroke,
//! and group, folded into each item). Everything else is reported (see
//! `Report`): clip paths, masks, filters, patterns, images, text, dashes,
//! non-pad spreads, focal radial gradients, and group opacity over
//! overlapping children (folding it into each item is then an
//! approximation).

use craie_core::geom::Affine;
use craie_vector::asset::{Asset, Item, ItemStyle};
use craie_vector::{FillRule, LineCap, LineJoin, Paint, Path, Stroke};

/// What the import could not represent exactly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// One line per unsupported feature met (deduplicated, in order).
    pub unsupported: Vec<String>,
}

impl Report {
    fn note(&mut self, what: &str) {
        if !self.unsupported.iter().any(|u| u == what) {
            self.unsupported.push(what.to_string());
        }
    }
}

/// Imports SVG (or SVGZ) bytes.
pub fn import(svg: &[u8]) -> Result<(Asset, Report), String> {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).map_err(|e| e.to_string())?;
    let size = tree.size();
    let mut asset = Asset {
        view_box: [0.0, 0.0, size.width(), size.height()],
        paints: Vec::new(),
        items: Vec::new(),
    };
    let mut report = Report::default();
    group(tree.root(), 1.0, &mut asset, &mut report);
    Ok((asset, report))
}

fn affine(t: usvg::Transform) -> Affine {
    Affine([t.sx, t.ky, t.kx, t.sy, t.tx, t.ty])
}

fn group(g: &usvg::Group, opacity: f32, asset: &mut Asset, report: &mut Report) {
    if g.clip_path().is_some() {
        report.note("clip-path (drawn unclipped)");
    }
    if g.mask().is_some() {
        report.note("mask (drawn unmasked)");
    }
    if !g.filters().is_empty() {
        report.note("filter (drawn unfiltered)");
    }
    if g.blend_mode() != usvg::BlendMode::Normal {
        report.note("blend mode (drawn normal)");
    }
    let own = g.opacity().get();
    if own < 1.0 && g.children().len() > 1 {
        report.note("group opacity over several children (folded into each)");
    }
    let opacity = opacity * own;
    for child in g.children() {
        match child {
            usvg::Node::Group(g) => group(g, opacity, asset, report),
            usvg::Node::Path(p) => path(p, opacity, asset, report),
            usvg::Node::Image(_) => report.note("image (skipped)"),
            usvg::Node::Text(_) => report.note("text (skipped; outline it first)"),
        }
    }
}

fn path(p: &usvg::Path, opacity: f32, asset: &mut Asset, report: &mut Report) {
    if !p.is_visible() {
        return;
    }
    let mut data = Path::new();
    for seg in p.data().segments() {
        use usvg::tiny_skia_path::PathSegment as S;
        match seg {
            S::MoveTo(a) => {
                data.move_to(a.x, a.y);
            }
            S::LineTo(a) => {
                data.line_to(a.x, a.y);
            }
            S::QuadTo(c, a) => {
                data.quad_to(c.x, c.y, a.x, a.y);
            }
            S::CubicTo(c1, c2, a) => {
                data.cubic_to([c1.x, c1.y], [c2.x, c2.y], [a.x, a.y]);
            }
            S::Close => {
                data.close();
            }
        }
    }
    let transform = affine(p.abs_transform());
    let fill = p.fill().and_then(|f| {
        let paint = paint(f.paint(), report)?;
        let rule = match f.rule() {
            usvg::FillRule::NonZero => FillRule::NonZero,
            usvg::FillRule::EvenOdd => FillRule::EvenOdd,
        };
        Some((paint, ItemStyle::Fill(rule), f.opacity().get()))
    });
    let stroke = p.stroke().and_then(|s| {
        if s.dasharray().is_some() {
            report.note("stroke-dasharray (drawn solid)");
        }
        let paint = paint(s.paint(), report)?;
        let style = ItemStyle::Stroke(Stroke {
            width: s.width().get(),
            join: match s.linejoin() {
                usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => LineJoin::Miter,
                usvg::LineJoin::Round => LineJoin::Round,
                usvg::LineJoin::Bevel => LineJoin::Bevel,
            },
            cap: match s.linecap() {
                usvg::LineCap::Butt => LineCap::Butt,
                usvg::LineCap::Round => LineCap::Round,
                usvg::LineCap::Square => LineCap::Square,
            },
            miter_limit: s.miterlimit().get().max(1.0),
        });
        Some((paint, style, s.opacity().get()))
    });
    let parts = match p.paint_order() {
        usvg::PaintOrder::FillAndStroke => [fill, stroke],
        usvg::PaintOrder::StrokeAndFill => [stroke, fill],
    };
    for (paint, style, own) in parts.into_iter().flatten() {
        asset.paints.push(paint);
        asset.items.push(Item {
            path: data.clone(),
            style,
            paint: asset.paints.len() - 1,
            opacity: (opacity * own).clamp(0.0, 1.0),
            transform,
        });
    }
}

fn rgba(c: usvg::Color, alpha: f32) -> u32 {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    (c.red as u32) << 24 | (c.green as u32) << 16 | (c.blue as u32) << 8 | a
}

fn paint(p: &usvg::Paint, report: &mut Report) -> Option<Paint> {
    let stops = |g: &usvg::BaseGradient, report: &mut Report| {
        if g.spread_method() != usvg::SpreadMethod::Pad {
            report.note("gradient spread other than pad (drawn padded)");
        }
        g.stops()
            .iter()
            .take(craie_vector::asset::MAX_STOPS)
            .map(|s| (s.offset().get(), rgba(s.color(), s.opacity().get())))
            .collect::<Vec<_>>()
    };
    Some(match p {
        usvg::Paint::Color(c) => Paint::Solid(rgba(*c, 1.0)),
        usvg::Paint::LinearGradient(g) => Paint::Linear {
            start: [g.x1(), g.y1()],
            end: [g.x2(), g.y2()],
            stops: stops(g, report),
            transform: affine(g.transform()),
        },
        usvg::Paint::RadialGradient(g) => {
            if g.fx() != g.cx() || g.fy() != g.cy() {
                report.note("radial gradient focal point (drawn centered)");
            }
            Paint::Radial {
                center: [g.cx(), g.cy()],
                radius: g.r().get(),
                stops: stops(g, report),
                transform: affine(g.transform()),
            }
        }
        usvg::Paint::Pattern(_) => {
            report.note("pattern paint (skipped)");
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use craie_vector::Verb;

    fn load(svg: &str) -> (Asset, Report) {
        let (a, r) = import(svg.as_bytes()).unwrap();
        // What the importer writes decodes back to itself.
        let bytes = craie_vector::asset::encode(&a);
        assert_eq!(craie_vector::asset::decode(&bytes).unwrap(), a);
        (a, r)
    }

    #[test]
    fn fills_strokes_and_the_view_box() {
        let (a, r) = load(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="24" viewBox="0 0 24 12">
                <rect x="1" y="2" width="10" height="4" fill="#ff0000" fill-opacity="0.5"/>
                <path d="M 2 2 L 10 2 L 6 9 Z" fill="none" stroke="#00ff00" stroke-width="2"
                      stroke-linejoin="round" stroke-linecap="square" stroke-miterlimit="7"/>
                <path d="M0 0H10V10H0Z M3 3H7V7H3Z" fill-rule="evenodd" fill="blue"/>
            </svg>"##,
        );
        assert!(r.unsupported.is_empty(), "{r:?}");
        assert_eq!(a.view_box, [0.0, 0.0, 48.0, 24.0]);
        assert_eq!(a.items.len(), 3);
        // The view box scales everything by 2 into the asset's space.
        assert_eq!(a.items[0].transform, Affine::scale(2.0, 2.0));
        assert_eq!(a.paints[a.items[0].paint], Paint::Solid(0xFF00_00FF));
        assert_eq!(a.items[0].opacity, 0.5);
        assert_eq!(a.items[0].style, ItemStyle::Fill(FillRule::NonZero));
        let ItemStyle::Stroke(s) = a.items[1].style else {
            panic!()
        };
        assert_eq!(
            (s.width, s.join, s.cap, s.miter_limit),
            (2.0, LineJoin::Round, LineCap::Square, 7.0)
        );
        assert_eq!(a.paints[a.items[1].paint], Paint::Solid(0x00FF_00FF));
        assert_eq!(a.items[2].style, ItemStyle::Fill(FillRule::EvenOdd));
        assert_eq!(
            a.items[2]
                .path
                .verbs
                .iter()
                .filter(|v| matches!(v, Verb::MoveTo(_)))
                .count(),
            2
        );
    }

    #[test]
    fn gradients_groups_and_opacity() {
        let (a, r) = load(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
                <defs>
                  <linearGradient id="l" x1="0" y1="0" x2="1" y2="0">
                    <stop offset="0" stop-color="#ffffff"/>
                    <stop offset="1" stop-color="#000000" stop-opacity="0.5"/>
                  </linearGradient>
                  <radialGradient id="r" cx="10" cy="10" r="5" gradientUnits="userSpaceOnUse">
                    <stop offset="0.25" stop-color="red"/>
                    <stop offset="1" stop-color="blue"/>
                  </radialGradient>
                </defs>
                <g transform="translate(3 4)" opacity="0.5">
                  <rect x="0" y="0" width="10" height="4" fill="url(#l)"/>
                </g>
                <circle cx="10" cy="10" r="5" fill="url(#r)"/>
            </svg>"##,
        );
        assert!(r.unsupported.is_empty(), "{r:?}");
        assert_eq!(a.items.len(), 2);
        let first = &a.items[0];
        assert_eq!(first.transform, Affine::translate(3.0, 4.0));
        assert_eq!(first.opacity, 0.5);
        // objectBoundingBox units resolve into the rect's space: 0..1
        // across its 10 x 4 box.
        let Paint::Linear {
            start,
            end,
            stops,
            transform,
        } = &a.paints[first.paint]
        else {
            panic!("{:?}", a.paints[first.paint])
        };
        let s = transform.apply(craie_core::geom::Point::new(start[0], start[1]));
        let e = transform.apply(craie_core::geom::Point::new(end[0], end[1]));
        assert_eq!((s.x, s.y, e.x, e.y), (0.0, 0.0, 10.0, 0.0));
        assert_eq!(stops, &vec![(0.0, 0xFFFF_FFFF), (1.0, 0x0000_0080)]);
        assert!(matches!(
            &a.paints[a.items[1].paint],
            Paint::Radial {
                center: [10.0, 10.0],
                radius: 5.0,
                ..
            }
        ));
    }

    #[test]
    fn unsupported_features_are_reported() {
        let (a, r) = load(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
                <defs><clipPath id="c"><rect width="5" height="5"/></clipPath></defs>
                <g clip-path="url(#c)" opacity="0.5">
                  <rect width="10" height="10" fill="red"/>
                  <rect x="5" width="10" height="10" fill="blue"/>
                </g>
                <line x1="0" y1="0" x2="10" y2="10" stroke="black" stroke-dasharray="2 2"/>
            </svg>"##,
        );
        // Both rects, the line's stroke, and its default fill (no area:
        // it draws nothing).
        assert_eq!(a.items.len(), 4, "still drawn");
        for want in ["clip-path", "group opacity", "stroke-dasharray"] {
            assert!(
                r.unsupported.iter().any(|u| u.starts_with(want)),
                "{want}: {r:?}"
            );
        }
        assert!(import(b"<not svg").is_err());
    }
}

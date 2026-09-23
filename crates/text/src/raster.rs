//! Swash rasterization wrapper.
//!
//! Parley owns font resolution, shaping, bidi and line breaking. Swash owns
//! rasterization only. This module is the single funnel from Parley's
//! `Run` data (font, size, variation coords) to a glyph bitmap.

use swash::FontRef;
use swash::scale::image::{Content, Image};
use swash::scale::{Render, ScaleContext, Scaler, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform, Vector};

use parley::FontData;

pub struct Rasterizer {
    cx: ScaleContext,
}

pub struct Rastered {
    pub image: Image,
    pub color: bool,
}

impl Rasterizer {
    pub fn new() -> Rasterizer {
        Rasterizer {
            cx: ScaleContext::new(),
        }
    }

    /// Builds a scaler for a whole run — font properties are constant across
    /// the run, so callers build one scaler and rasterize many glyphs.
    pub fn scaler<'a>(
        &'a mut self,
        font: &'a FontData,
        size: f32,
        coords: &[i16],
    ) -> Option<Scaler<'a>> {
        let font_ref = FontRef::from_index(font.data.as_ref(), font.index as usize)?;
        let builder = self.cx.builder(font_ref).size(size).hint(true);
        Some(if coords.is_empty() {
            builder.build()
        } else {
            builder.normalized_coords(coords.iter().copied()).build()
        })
    }

    /// Rasterizes one glyph at a subpixel offset (fraction of a pixel,
    /// already quantized by the cache layer). `embolden` applies a
    /// faux-bold strength and `skew` a faux-italic angle in degrees;
    /// pass 0 / None for neither.
    pub fn render(
        scaler: &mut Scaler<'_>,
        glyph: u16,
        offset: Vector,
        embolden: f32,
        skew: Option<f32>,
    ) -> Option<Rastered> {
        let mut render = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ]);
        render
            .format(Format::Alpha)
            .embolden(embolden)
            .offset(offset);
        if let Some(deg) = skew {
            render.transform(Some(Transform::skew(Angle::from_degrees(deg), Angle::ZERO)));
        }
        let image = render.render(scaler, glyph)?;
        let color = matches!(image.content, Content::Color);
        Some(Rastered { image, color })
    }
}

impl Default for Rasterizer {
    fn default() -> Rasterizer {
        Rasterizer::new()
    }
}

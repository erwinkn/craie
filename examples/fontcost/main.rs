//! First-draw font cost breakdown: the one-time work a fresh text engine
//! does for framebench's row text (14 pt, 800 pt wide, 2x), per family,
//! in a fresh process so every cache starts cold: source load
//! (`SystemFonts::new`), family resolution, HarfRust shaping data, the
//! first shape (plan compile + shaping) against a warm shape, raster
//! (first emit against a warm emit), fallback probes (an ASCII row
//! covered by its primary needs none), and 1,000 more rows. (Step 3a
//! also ran Parley here; Parley left craie-text in step 3b.)
//!
//!   cargo run --release --example fontcost -- <family>

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::Point;
use craie_platform_winit::fonts::SystemFonts;
use craie_scene::{ChunkWriter, RasterAtlas};
use craie_text::TextEngine;
use craie_text::fonts::{FontAttrs, FontBlob, FontSource, ScriptTag};
use craie_text::paragraph::{Resolve, SpanStyle, TextSpec, TextStyle};
use craie_text::swash::{FontRef, StringId};

const WIDTH: f32 = 800.0;
const SCALE: f32 = 2.0;

fn row(ix: usize) -> String {
    format!("Row {ix:05}: retained native content for the frame comparison")
}

/// Counts fallback queries.
struct Counting {
    inner: SystemFonts,
    fallbacks: Arc<AtomicUsize>,
}

impl FontSource for Counting {
    fn select(&mut self, family: &str, attrs: FontAttrs) -> Option<FontBlob> {
        self.inner.select(family, attrs)
    }
    fn fallback(
        &mut self,
        cluster: &str,
        script: ScriptTag,
        attrs: FontAttrs,
        emoji: bool,
    ) -> Vec<FontBlob> {
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
        self.inner.fallback(cluster, script, attrs, emoji)
    }
}

fn family_name(bytes: &[u8], index: u32) -> String {
    FontRef::from_index(bytes, index as usize)
        .and_then(|f| f.localized_strings().find_by_id(StringId::Family, None))
        .map(|s| s.chars().collect())
        .unwrap_or_default()
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn ours(family: &str) {
    let t = Instant::now();
    let source = SystemFonts::new();
    let load = ms(t);
    let fallbacks = Arc::new(AtomicUsize::new(0));
    let mut e = TextEngine::with_source(Box::new(Counting {
        inner: source,
        fallbacks: fallbacks.clone(),
    }));
    let attrs = FontAttrs {
        weight: 400,
        italic: false,
    };
    let t = Instant::now();
    let font = e.fonts.primary(family, attrs).expect("no font");
    let resolve = ms(t);
    let face = e.fonts.store.instance_data(font).face;
    let t = Instant::now();
    e.fonts.store.shaper_data(face);
    let data = ms(t);
    let spans = [SpanStyle {
        start: 0,
        style: TextStyle {
            size: 14.0,
            ..TextStyle::default()
        },
    }];
    let (a, b) = (row(0), row(1));
    let (sa, sb) = (
        TextSpec {
            text: &a,
            family,
            spans: &spans,
        },
        TextSpec {
            text: &b,
            family,
            spans: &spans,
        },
    );
    let t = Instant::now();
    let p = e.layout_text(&sa, Some(WIDTH));
    let first = ms(t);
    let t = Instant::now();
    let q = e.layout_text(&sb, Some(WIDTH));
    let warm = ms(t);
    let mut atlas = RasterAtlas::new();
    let t = Instant::now();
    e.emit_paragraph(
        &p,
        Point::ZERO,
        SCALE,
        None,
        &mut atlas,
        &mut ChunkWriter::new(),
    );
    let raster = ms(t);
    let t = Instant::now();
    e.emit_paragraph(
        &q,
        Point::ZERO,
        SCALE,
        None,
        &mut atlas,
        &mut ChunkWriter::new(),
    );
    let emit_warm = ms(t);
    // Steady per-row cost: 1,000 more rows, shaped, then emitted (new
    // subpixel positions still rasterize).
    let texts: Vec<String> = (2..1002).map(row).collect();
    let t = Instant::now();
    let ps: Vec<_> = texts
        .iter()
        .map(|text| {
            e.layout_text(
                &TextSpec {
                    text,
                    family,
                    spans: &spans,
                },
                Some(WIDTH),
            )
        })
        .collect();
    let shape_1k = ms(t);
    let t = Instant::now();
    for (i, p) in ps.iter().enumerate() {
        let origin = Point::new(0.0, i as f32 * 20.0);
        e.emit_paragraph(p, origin, SCALE, None, &mut atlas, &mut ChunkWriter::new());
    }
    let emit_1k = ms(t);
    let rasters = e.cache.stats.rasters;
    let f = e.fonts.store.face_data(face);
    println!("ours {family:10} 1k rows: shape {shape_1k:6.2} emit {emit_1k:6.2} rasters {rasters}");
    println!(
        "ours {family:10} face {:22} load {load:6.2} resolve {resolve:6.2} shaperdata {data:6.2} \
         first-shape {first:6.3} warm-shape {warm:6.3} first-emit {raster:6.3} warm-emit {emit_warm:6.3} \
         fallback-queries {}",
        format!("{:?}", family_name(f.bytes.as_ref().as_ref(), f.index)),
        fallbacks.load(Ordering::Relaxed)
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let family = args.get(1).map_or("system-ui", String::as_str);
    ours(family);
}

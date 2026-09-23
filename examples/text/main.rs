//! Text demo: owned paragraphs (HarfRust + skrifa) -> Swash -> raster
//! atlas -> scene chunks -> wgpu, in a real native window. One chunk per
//! paragraph; a span's index is its paint slot, so each chunk's paint
//! records hold its spans' colors in order.
//!
//!   cargo run --example text                 open the window
//!   cargo run --example text -- --screenshot out.png [w h scale]
//!                                            render one frame offscreen
//!
//! The window path is deliberately idle: a frame is produced only when the
//! OS asks (expose) or the app requests one after a resize/scale change.

use craie_core::geom::{Affine, Point, Rect, Size};
use craie_platform_winit::{self as platform, Window};
use craie_render::{Gpu, Renderer, WindowSurface};
use craie_scene::{ChunkWriter, Color, NONE, OrderItem, Placement, Scene};
use craie_text::TextEngine;
use craie_text::paragraph::{SpanStyle, TextSpec, TextStyle};

const MARGIN: f32 = 48.0;
const GAP: f32 = 20.0;

const BG: Color = Color::rgb(0x14, 0x15, 0x18);
const FG: Color = Color::rgb(0xec, 0xec, 0xf0);
const DIM: Color = Color::rgb(0x9a, 0xa0, 0xae);
const ACCENT: Color = Color::rgb(0x6d, 0xc7, 0xff);
const CODE_BG: Color = Color::rgb(0x22, 0x24, 0x2b);

/// One paragraph of demo content: text, family, and spans with their
/// colors (span i paints with paint slot i).
struct Para {
    text: String,
    family: &'static str,
    spans: Vec<SpanStyle>,
    colors: Vec<Color>,
    /// Give this paragraph a background quad (exercises the quad pipeline).
    backing: bool,
}

/// A styled stretch of a paragraph: the first occurrence of `needle`.
struct Mark {
    needle: &'static str,
    weight: u16,
    italic: bool,
    color: Color,
}

fn mark(needle: &'static str, weight: u16, italic: bool, color: Color) -> Mark {
    Mark {
        needle,
        weight,
        italic,
        color,
    }
}

/// A paragraph in `FG` at `size`, with `marks` (not overlapping) as
/// spans; the text between them keeps the default style.
fn para(text: &str, size: f32, family: &'static str, marks: &[Mark]) -> Para {
    let base = TextStyle {
        size,
        weight: 400,
        italic: false,
    };
    let mut ranges: Vec<(usize, &Mark)> = marks
        .iter()
        .map(|m| (text.find(m.needle).expect("mark needle not in text"), m))
        .collect();
    ranges.sort_by_key(|r| r.0);
    let (mut spans, mut colors) = (Vec::new(), Vec::new());
    let mut push = |start: usize, style: TextStyle, color: Color| {
        spans.push(SpanStyle {
            start: start as u32,
            style,
        });
        colors.push(color);
    };
    push(0, base, FG);
    for (start, m) in ranges {
        let style = TextStyle {
            size,
            weight: m.weight,
            italic: m.italic,
        };
        push(start, style, m.color);
        let end = start + m.needle.len();
        if end < text.len() {
            push(end, base, FG);
        }
    }
    // A mark at 0 replaces the default span.
    if spans.len() > 1 && spans[1].start == 0 {
        spans.remove(0);
        colors.remove(0);
    }
    Para {
        text: text.to_string(),
        family,
        spans,
        colors,
        backing: false,
    }
}

/// The demo content: the milestone-required strings, mixed styling in one
/// paragraph, a monospace paragraph, and an emoji probe.
fn paragraphs() -> Vec<Para> {
    let mut code = para(
        "let inst = Instance::quad(x, y, w, h, color); // monospace",
        14.0,
        "monospace",
        &[mark("// monospace", 400, false, DIM)],
    );
    code.backing = true;
    vec![
        para("Craie", 44.0, "", &[mark("Craie", 650, false, FG)]),
        para(
            "React → retained native state → native pixels",
            17.0,
            "",
            &[mark("retained native state", 400, false, ACCENT)],
        ),
        para(
            "The quick brown fox jumps over the lazy dog.  ffi AV To",
            17.0,
            "",
            &[],
        ),
        para(
            "A normal run, a bold run, an italic run, and `code → atlas` inline.",
            16.0,
            "",
            &[
                mark("a bold run", 700, false, FG),
                mark("an italic run", 400, true, FG),
                mark("code → atlas", 400, false, ACCENT),
            ],
        ),
        para("English — 日本語 — مرحبا بالعالم", 20.0, "", &[]),
        para("Emoji probe: 🎨 🚀 👩‍💻 🦀 (color atlas)", 16.0, "", &[]),
        para(
            "Wrapping probe: the retained host should do no work while idle. This \
             paragraph exists to exercise line breaking at the current width; \
             resizing the window must reflow it without re-rasterizing glyphs \
             that are already in the atlas.",
            14.0,
            "",
            &[mark("no work while idle", 400, false, DIM)],
        ),
        code,
    ]
}

/// Lays out every paragraph at the current logical size and rebuilds
/// one chunk per paragraph in `scene` (kept across rebuilds so the glyph
/// cache's raster ids stay valid). Returns (runs, glyph instances,
/// rasters performed this pass).
fn build_scene(
    scene: &mut Scene,
    text: &mut TextEngine,
    paras: &[Para],
    size: Size,
    scale: f32,
) -> (u32, u32, u64) {
    scene.clear = BG;
    scene.scale = scale;
    if scene.transforms.is_empty() {
        let root = scene.transforms.alloc(Affine::IDENTITY, NONE);
        scene.transforms.set_order(vec![root]);
    }
    scene.transforms.set_local(0, Affine::scale(scale, scale));
    let wrap = size.width - 2.0 * MARGIN;
    let rasters_before = text.cache.stats.rasters;
    let (mut runs, mut glyphs) = (0, 0);
    let mut w = ChunkWriter::new();
    let mut order = Vec::new();
    let mut y = MARGIN;
    for (i, para) in paras.iter().enumerate() {
        let layout = text.layout_text(
            &TextSpec {
                text: &para.text,
                family: para.family,
                spans: &para.spans,
            },
            Some(wrap),
        );
        for c in &para.colors {
            w.paint(c.0);
        }
        // The backing rect paints under this paragraph's glyphs.
        if para.backing {
            let slot = w.paint(CODE_BG.0);
            w.rect(
                Rect::new(-8.0, -4.0, layout.width + 16.0, layout.height + 8.0),
                0.0,
                slot,
            );
        }
        let emitted =
            text.emit_paragraph(&layout, Point::ZERO, scale, None, &mut scene.atlas, &mut w);
        runs += emitted.glyph_runs;
        glyphs += emitted.glyphs;
        let id = i as u32;
        scene.commit_chunk(id, &mut w);
        scene.set_placement(
            id,
            Placement {
                offset: [MARGIN, y],
                transform: 0,
                clip: NONE,
            },
        );
        order.push(OrderItem::Chunk(id));
        y += layout.height + GAP;
    }
    scene.set_order(order, Vec::new());
    let mut missing = Vec::new();
    scene.prepare(
        Size::new(size.width * scale, size.height * scale),
        &mut missing,
    );
    text.ensure_resident(&missing, &mut scene.atlas);
    (runs, glyphs, text.cache.stats.rasters - rasters_before)
}

// ---------------------------------------------------------------- window

/// Lazily initialized so GPU + text state live inside the app callback.
struct Demo {
    inner: Option<Inner>,
}

struct Inner {
    gpu: Gpu,
    surface: WindowSurface,
    renderer: Renderer,
    text: TextEngine,
    paras: Vec<Para>,
    scene: Scene,
    frames: u64,
}

impl Inner {
    /// Relayout + re-emit at the current size/scale, upload what changed,
    /// and request one frame. The glyph cache survives relayout, so this
    /// performs rasterization only for genuinely new glyphs.
    fn rebuild(&mut self, window: &Window) {
        let (w, h) = window.size();
        let scale = window.scale_factor() as f32;
        self.surface.resize(&self.gpu, w, h);
        let (runs, glyphs, rasters) = build_scene(
            &mut self.scene,
            &mut self.text,
            &self.paras,
            Size::new(w as f32 / scale, h as f32 / scale),
            scale,
        );
        self.renderer.prepare(&self.gpu, &mut self.scene);
        eprintln!(
            "[craie] layout {w}x{h} @{scale:.2}x — {runs} runs, {glyphs} glyph instances, \
             {rasters} rasters, cache {} hits / {} misses",
            self.text.cache.stats.hits, self.text.cache.stats.misses,
        );
        window.request_redraw();
    }
}

impl platform::App for Demo {
    fn ready(&mut self, window: &Window, _wake: &platform::Wake) {
        let (w, h) = window.size();
        let (gpu, surface) = Gpu::for_window(window.surface_target());
        let surface = WindowSurface::new(&gpu, surface, w, h);
        let renderer = Renderer::new(&gpu, surface.config.format);
        let mut text = TextEngine::new();
        let paras = paragraphs();
        let scale = window.scale_factor() as f32;
        let mut scene = Scene::new();
        let (runs, glyphs, rasters) = build_scene(
            &mut scene,
            &mut text,
            &paras,
            Size::new(w as f32 / scale, h as f32 / scale),
            scale,
        );
        let mut inner = Inner {
            gpu,
            surface,
            renderer,
            text,
            paras,
            scene,
            frames: 0,
        };
        inner.renderer.prepare(&inner.gpu, &mut inner.scene);
        eprintln!(
            "[craie] ready {w}x{h} @{scale:.2}x — {runs} runs, {glyphs} glyph instances, {rasters} rasters"
        );
        self.inner = Some(inner);
    }

    fn resized(&mut self, window: &Window) {
        if let Some(inner) = &mut self.inner {
            inner.rebuild(window);
        }
    }

    fn redraw(&mut self, window: &Window) {
        let Some(inner) = &mut self.inner else { return };
        let (w, h) = window.size();
        if w == 0 || h == 0 {
            return; // minimized / zero-sized surface
        }
        let frame = match inner.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                inner.rebuild(window);
                return;
            }
            // Occluded/Timeout: skip; un-occlusion requests a repaint.
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        inner
            .renderer
            .draw(&inner.gpu, &view, w, h, &mut inner.scene);
        window.pre_present_notify();
        inner.gpu.queue.present(frame);
        inner.frames += 1;
    }
}

// -------------------------------------------------------------- headless

fn run_screenshot(path: &str, w: u32, h: u32, scale: f32) {
    let gpu = Gpu::headless();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(&gpu, format);
    let mut text = TextEngine::new();
    let paras = paragraphs();
    let mut scene = Scene::new();
    let (runs, glyphs, rasters) = build_scene(
        &mut scene,
        &mut text,
        &paras,
        Size::new(w as f32 / scale, h as f32 / scale),
        scale,
    );
    eprintln!(
        "[craie] headless {w}x{h} @{scale}x — {runs} runs, {glyphs} glyph instances, {rasters} rasters"
    );
    renderer.prepare(&gpu, &mut scene);
    eprintln!(
        "[craie] atlas: {} alpha pages, {} color pages, {} allocations, {} bytes uploaded",
        scene.atlas.stats.alpha_pages,
        scene.atlas.stats.color_pages,
        scene.atlas.stats.allocations,
        renderer.atlas_upload_bytes,
    );

    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    renderer.draw(&gpu, &view, w, h, &mut scene);

    // Readback: texture -> 256-aligned buffer -> PNG.
    let row_bytes = w * 4;
    let padded = row_bytes.next_multiple_of(256);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);

    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
    let rgba;
    {
        let mapped = slice.get_mapped_range().unwrap();
        rgba = (0..h as usize)
            .flat_map(|row| {
                mapped[row * padded as usize..row * padded as usize + row_bytes as usize]
                    .iter()
                    .copied()
            })
            .collect::<Vec<u8>>();
    }
    buf.unmap();

    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(file, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgba)
        .unwrap();
    eprintln!("[craie] wrote {path}");
}

fn main() {
    // Text lays out on the system's fonts.
    craie_platform_winit::fonts::install();
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--screenshot") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("text.png");
        let w = args.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(1800);
        let h = args.get(i + 3).and_then(|s| s.parse().ok()).unwrap_or(1200);
        let scale = args.get(i + 4).and_then(|s| s.parse().ok()).unwrap_or(2.0);
        run_screenshot(path, w, h, scale);
        return;
    }
    platform::run(
        "craie — text",
        Size::new(900.0, 600.0),
        Demo { inner: None },
    );
}

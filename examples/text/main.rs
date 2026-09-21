//! Milestone 0 demo: Parley -> Swash -> Craie atlas -> wgpu, in a real
//! native window.
//!
//!   cargo run --example text                 open the window
//!   cargo run --example text -- --screenshot out.png [w h scale]
//!                                            render one frame offscreen
//!
//! The window path is deliberately idle: a frame is produced only when the
//! OS asks (expose) or the app requests one after a resize/scale change.

use craie::geom::{Point, Size};
use craie::gpu::{Gpu, Renderer, WindowSurface};
use craie::platform::{self, Window};
use craie::scene::{Color, QuadInstance, Scene};
use craie::text::parley::style::{FontStyle, FontWeight, GenericFamily, LineHeight, StyleProperty};
use craie::text::{ParagraphSpec, TextEngine, TextSpan};

const MARGIN: f32 = 48.0;
const GAP: f32 = 20.0;

const BG: Color = Color::rgb(0x14, 0x15, 0x18);
const FG: Color = Color::rgb(0xec, 0xec, 0xf0);
const DIM: Color = Color::rgb(0x9a, 0xa0, 0xae);
const ACCENT: Color = Color::rgb(0x6d, 0xc7, 0xff);
const CODE_BG: Color = Color::rgb(0x22, 0x24, 0x2b);

/// One paragraph of demo content: text plus its default style and ranged
/// overrides.
struct Para {
    text: String,
    defaults: Vec<StyleProperty<'static, Color>>,
    spans: Vec<TextSpan>,
    /// Give this paragraph a background quad (exercises the quad pipeline).
    backing: bool,
}

fn para(text: &str, size: f32, spans: Vec<TextSpan>) -> Para {
    Para {
        text: text.to_string(),
        defaults: vec![
            StyleProperty::Brush(FG),
            StyleProperty::FontFamily(GenericFamily::SansSerif.into()),
            StyleProperty::FontSize(size),
            StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.25)),
        ],
        spans,
        backing: false,
    }
}

fn span(text: &str, needle: &str, style: StyleProperty<'static, Color>) -> TextSpan {
    let start = text.find(needle).expect("span needle not in text");
    TextSpan {
        range: start..start + needle.len(),
        style,
    }
}

/// The demo content: the milestone-required strings, mixed styling in one
/// paragraph, a monospace run, and an emoji probe.
fn paragraphs() -> Vec<Para> {
    let mut out = Vec::new();

    out.push(para(
        "Craie",
        44.0,
        vec![span(
            "Craie",
            "Craie",
            StyleProperty::FontWeight(FontWeight::new(650.0)),
        )],
    ));

    out.push(para(
        "React → retained native state → native pixels",
        17.0,
        vec![span(
            "React → retained native state → native pixels",
            "retained native state",
            StyleProperty::Brush(ACCENT),
        )],
    ));

    out.push(para(
        "The quick brown fox jumps over the lazy dog.  ffi AV To",
        17.0,
        vec![],
    ));

    let mixed = "A normal run, a bold run, an italic run, and `code → atlas` inline.";
    out.push(para(
        mixed,
        16.0,
        vec![
            span(
                mixed,
                "a bold run",
                StyleProperty::FontWeight(FontWeight::new(700.0)),
            ),
            span(
                mixed,
                "an italic run",
                StyleProperty::FontStyle(FontStyle::Italic),
            ),
            span(
                mixed,
                "`code → atlas`",
                StyleProperty::FontFamily(GenericFamily::Monospace.into()),
            ),
            span(mixed, "code → atlas", StyleProperty::Brush(ACCENT)),
        ],
    ));

    out.push(para("English — 日本語 — مرحبا بالعالم", 20.0, vec![]));

    out.push(para("Emoji probe: 🎨 🚀 👩‍💻 🦀 (color atlas)", 16.0, vec![]));

    let wrap_text = "Wrapping probe: the retained host should do no work while idle. This \
                     paragraph exists to exercise break_all_lines at the current width; \
                     resizing the window must reflow it without re-rasterizing glyphs \
                     that are already in the atlas.";
    out.push(para(
        wrap_text,
        14.0,
        vec![span(
            wrap_text,
            "no work while idle",
            StyleProperty::Brush(DIM),
        )],
    ));

    let code = "let quad = QuadInstance { pos, size, color }; // monospace";
    let mut code_para = para(
        code,
        14.0,
        vec![
            span(
                code,
                code,
                StyleProperty::FontFamily(GenericFamily::Monospace.into()),
            ),
            span(code, "// monospace", StyleProperty::Brush(DIM)),
        ],
    );
    code_para.backing = true;
    out.push(code_para);

    out
}

/// Lays out every paragraph at the current width and emits the flat scene.
/// Returns (scene, runs, glyph instances, rasters performed this pass).
fn build_scene(
    text: &mut TextEngine,
    paras: &[Para],
    width_px: f32,
    scale: f32,
) -> (Scene, u32, u32, u64) {
    let wrap = width_px - 2.0 * MARGIN * scale;
    let mut scene = Scene {
        clear: Some(BG),
        quads: Vec::new(),
        glyphs: Vec::new(),
    };
    let rasters_before = text.cache.stats.rasters;
    let mut runs = 0;
    let mut y = MARGIN * scale;
    for para in paras {
        let layout = text.layout_paragraph(
            &ParagraphSpec {
                text: &para.text,
                defaults: para.defaults.clone(),
                spans: para.spans.clone(),
            },
            scale,
            Some(wrap),
        );
        let emitted = text.emit(&layout, Point::new(MARGIN * scale, y), &mut scene.glyphs);
        runs += emitted.glyph_runs;
        if para.backing {
            scene.quads.push(QuadInstance {
                position: [MARGIN * scale - 8.0 * scale, y - 4.0 * scale],
                size: [layout.width() + 16.0 * scale, layout.height() + 8.0 * scale],
                color: CODE_BG.0,
            });
        }
        y += layout.height() + GAP * scale;
    }
    let rasters = text.cache.stats.rasters - rasters_before;
    let glyphs = scene.glyphs.len() as u32;
    (scene, runs, glyphs, rasters)
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
        let (scene, runs, glyphs, rasters) =
            build_scene(&mut self.text, &self.paras, w as f32, scale);
        self.scene = scene;
        self.renderer.sync_atlas(&self.gpu, &mut self.text.atlas);
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
        let (scene, runs, glyphs, rasters) = build_scene(&mut text, &paras, w as f32, scale);
        let mut inner = Inner {
            gpu,
            surface,
            renderer,
            text,
            paras,
            scene,
            frames: 0,
        };
        inner.renderer.sync_atlas(&inner.gpu, &mut inner.text.atlas);
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
        inner.renderer.draw(&inner.gpu, &view, w, h, &inner.scene);
        window.pre_present_notify();
        inner.gpu.queue.present(frame);
        inner.frames += 1;
    }
}

// -------------------------------------------------------------- headless

fn run_screenshot(path: &str, w: u32, h: u32, scale: f32) {
    let gpu = Gpu::headless();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(&gpu, format);
    let mut text = TextEngine::new();
    let paras = paragraphs();
    let (scene, runs, glyphs, rasters) = build_scene(&mut text, &paras, w as f32, scale);
    eprintln!(
        "[craie] headless {w}x{h} @{scale}x — {runs} runs, {glyphs} glyph instances, {rasters} rasters"
    );
    renderer.sync_atlas(&gpu, &mut text.atlas);
    eprintln!(
        "[craie] atlas: {} alpha pages, {} color pages, {} allocations, {} bytes uploaded",
        text.atlas.stats.alpha_pages,
        text.atlas.stats.color_pages,
        text.atlas.stats.allocations,
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
    renderer.draw(&gpu, &view, w, h, &scene);

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

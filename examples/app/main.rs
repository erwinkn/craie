//! Milestone 1 demo: a native window whose entire UI arrives as binary
//! wire transactions — no demo content is compiled into the render path.
//!
//!   cargo run --example app                 window + bridge socket
//!   cargo run --example app -- --port 9471  override listen port
//!   cargo run --example app -- --screenshot out.png [w h scale]
//!                                           render a built-in demo txn
//!
//! Protocol: connect, then send `u32 len | wire txn` frames. Each frame is
//! one React commit. The app applies, layouts, and repaints once.

use craie::bridge;
use craie::geom::Size;
use craie::gpu::{Gpu, Renderer, WindowSurface};
use craie::platform::{self, Wake, Window};
use craie::ui::Ui;
use craie::wire::Encoder;
use taffy::{AlignContent, AlignItems, Dimension, FlexDirection, LengthPercentage, Rect, Style};

const KIND_VIEW: u8 = 0;
const KIND_TEXT: u8 = 1;
const NIL: u32 = u32::MAX;

fn style(f: impl FnOnce(&mut Style)) -> Style {
    let mut s = Style::default();
    f(&mut s);
    s
}

/// The built-in demo, expressed as one transaction. The JS bridge produces
/// exactly this byte shape from a React tree.
fn demo_txn() -> Vec<u8> {
    let mut enc = Encoder::new();

    enc.style(
        1,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.flex_direction = FlexDirection::Column;
            s.size = taffy::Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            };
            s.padding = Rect {
                left: LengthPercentage::length(32.0),
                right: LengthPercentage::length(32.0),
                top: LengthPercentage::length(32.0),
                bottom: LengthPercentage::length(32.0),
            };
            s.gap = taffy::Size {
                width: LengthPercentage::length(16.0),
                height: LengthPercentage::length(16.0),
            };
        }),
    );
    enc.style(
        2,
        &style(|s| {
            s.align_items = Some(AlignItems::CENTER);
            s.justify_content = Some(AlignContent::CENTER);
            s.size = taffy::Size {
                width: Dimension::length(220.0),
                height: Dimension::length(80.0),
            };
        }),
    );

    // Root column
    enc.create(0, KIND_VIEW);
    enc.set_style(0, 1);
    enc.view_paint(0, 0x1B1D_24FF);
    enc.place(NIL, 0, NIL);

    // Title + body text
    enc.create(1, KIND_TEXT);
    enc.set_text(1, "Craie — driven entirely over the wire");
    enc.text_props(1, 28.0, 0xECEC_F0FF);
    enc.place(0, 1, NIL);

    enc.create(2, KIND_TEXT);
    enc.set_text(
        2,
        "This frame was decoded from a flat binary transaction: no per-node \
         objects, no diffing, just ops applied to a retained host.",
    );
    enc.text_props(2, 15.0, 0x9AA0_AEFF);
    enc.place(0, 2, NIL);

    // A row of colored boxes
    enc.style(
        3,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.gap = taffy::Size {
                width: LengthPercentage::length(12.0),
                height: LengthPercentage::length(12.0),
            };
            s.padding = Rect {
                left: LengthPercentage::length(0.0),
                right: LengthPercentage::length(0.0),
                top: LengthPercentage::length(8.0),
                bottom: LengthPercentage::length(8.0),
            };
        }),
    );
    enc.create(3, KIND_VIEW);
    enc.set_style(3, 3);
    enc.place(0, 3, NIL);

    let colors = [0x6DC7_FFFF, 0xB1E1_8AFF, 0xFFB4_6DFF];
    for (i, c) in colors.iter().enumerate() {
        let id = 10 + i as u32;
        enc.create(id, KIND_VIEW);
        enc.set_style(id, 2);
        enc.view_paint(id, *c);
        enc.place(3, id, NIL);
        let tid = 20 + i as u32;
        enc.create(tid, KIND_TEXT);
        enc.set_text(tid, &format!("box {i}"));
        enc.text_props(tid, 14.0, 0x1415_18FF);
        enc.place(id, tid, NIL);
    }

    enc.finish(1)
}

// ---------------------------------------------------------------- window

struct BridgeApp {
    inner: Option<Inner>,
    port: u16,
}

struct Inner {
    gpu: Gpu,
    surface: WindowSurface,
    renderer: Renderer,
    ui: Ui,
    inbox: bridge::Inbox,
    frames: u64,
}

impl Inner {
    /// Applies pending mutations, layouts, rebuilds the scene, asks for a
    /// frame. Called when the socket delivered bytes or the surface changed.
    fn sync(&mut self, window: &Window, drain: bool) {
        if drain {
            let mut applied = 0u32;
            for buf in self.inbox.drain() {
                match self.ui.apply(&buf) {
                    Ok(seq) => {
                        applied += 1;
                        self.inbox.acks.ack(seq);
                    }
                    Err(e) => eprintln!("[craie] bad txn: {e:?}"),
                }
            }
            if applied > 0 {
                eprintln!(
                    "[craie] applied {applied} txn(s), seq={}, {} live nodes",
                    self.ui.seq,
                    self.ui.host.len()
                );
            }
        }
        if !self.ui.needs_paint() {
            return;
        }
        let (w, h) = window.size();
        self.ui.scale = window.scale_factor() as f32;
        let scene = self.ui.render(Size::new(w as f32, h as f32));
        let (quads, glyphs) = (scene.quads.len(), scene.glyphs.len());
        self.renderer.sync_atlas(&self.gpu, &mut self.ui.text.atlas);
        eprintln!("[craie] render {w}x{h} — {quads} quads, {glyphs} glyphs");
        window.request_redraw();
    }
}

impl platform::App for BridgeApp {
    fn ready(&mut self, window: &Window, wake: &Wake) {
        let (w, h) = window.size();
        let (gpu, surface) = Gpu::for_window(window.surface_target());
        let surface = WindowSurface::new(&gpu, surface, w, h);
        let renderer = Renderer::new(&gpu, surface.config.format);
        let mut ui = Ui::new(window.scale_factor() as f32);
        // Boot with the demo txn so the window isn't blank until JS connects.
        ui.apply(&demo_txn()).unwrap();

        let inbox = bridge::listen(&format!("127.0.0.1:{}", self.port), wake.clone())
            .expect("bridge listen failed");
        eprintln!("[craie] bridge listening on 127.0.0.1:{}", inbox.port);

        let mut inner = Inner {
            gpu,
            surface,
            renderer,
            ui,
            inbox,
            frames: 0,
        };
        inner.sync(window, false);
        self.inner = Some(inner);
    }

    fn woke(&mut self, window: &Window) {
        if let Some(inner) = &mut self.inner {
            inner.sync(window, true);
        }
    }

    fn resized(&mut self, window: &Window) {
        if let Some(inner) = &mut self.inner {
            let (w, h) = window.size();
            inner.surface.resize(&inner.gpu, w, h);
            // Size change invalidates wrap widths: every cache goes.
            inner.ui.invalidate_layout();
            inner.sync(window, false);
        }
    }

    fn redraw(&mut self, window: &Window) {
        let Some(inner) = &mut self.inner else { return };
        let frame = match inner.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                let (w, h) = window.size();
                inner.surface.resize(&inner.gpu, w, h);
                window.request_redraw();
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let (w, h) = window.size();
        inner
            .renderer
            .draw(&inner.gpu, &view, w, h, inner.ui.scene());
        drop(frame);
        inner.frames += 1;
    }
}

// -------------------------------------------------------------- headless

fn run_screenshot(path: &str, w: u32, h: u32, scale: f32) {
    let gpu = Gpu::headless();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(&gpu, format);
    let mut ui = Ui::new(scale);
    ui.apply(&demo_txn()).expect("demo txn");
    let scene = ui.render(Size::new(w as f32, h as f32));
    eprintln!(
        "[craie] wire render — {} quads, {} glyphs, {} nodes",
        scene.quads.len(),
        scene.glyphs.len(),
        ui.host.len()
    );
    renderer.sync_atlas(&gpu, &mut ui.text.atlas);
    eprintln!(
        "[craie] atlas upload: {} bytes",
        renderer.atlas_upload_bytes
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
    renderer.draw(&gpu, &view, w, h, ui.scene());

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
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
    let rgba = {
        let mapped = slice.get_mapped_range().unwrap();
        (0..h as usize)
            .flat_map(|row| {
                mapped[row * padded as usize..row * padded as usize + row_bytes as usize]
                    .iter()
                    .copied()
            })
            .collect::<Vec<u8>>()
    };
    buf.unmap();

    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    eprintln!("[craie] wrote {path}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--screenshot") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("app.png");
        let w = args.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(1600);
        let h = args.get(i + 3).and_then(|s| s.parse().ok()).unwrap_or(1000);
        let scale = args.get(i + 4).and_then(|s| s.parse().ok()).unwrap_or(2.0);
        run_screenshot(path, w, h, scale);
        return;
    }
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .unwrap_or(9470);
    platform::run(
        "craie — app",
        Size::new(800.0, 500.0),
        BridgeApp { inner: None, port },
    );
}

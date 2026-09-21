//! Native window driven by wire transactions through the in-process
//! bridge — the same `Session` the N-API host uses, fed here by a local
//! ticker thread standing in for the React worker.
//!
//!   cargo run --example app                 window + ticking text update
//!   cargo run --example app -- --screenshot out.png [w h scale]
//!                                           render a built-in demo txn
//!
//! The ticker submits the demo transaction, then one update per second —
//! exercising submit → wake → apply → ack → repaint without a JS runtime.

use craie::app::HostApp;
use craie::bridge;
use craie::geom::Size;
use craie::gpu::{Gpu, Renderer};
use craie::platform;
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

// -------------------------------------------------------------- headless

/// Renders `scene` with `renderer` to an offscreen `format` texture and
/// saves a PNG of the result.
fn dump_scene(
    gpu: &Gpu,
    renderer: &mut Renderer,
    scene: &craie::scene::Scene,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    path: &str,
) {
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
    renderer.draw(gpu, &view, w, h, scene);

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

fn run_screenshot(path: &str, w: u32, h: u32, scale: f32) {
    let gpu = Gpu::headless();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(&gpu, format);
    let mut ui = Ui::new(scale);
    ui.apply(&demo_txn()).expect("demo txn");
    let items = {
        let scene = ui.render(Size::new(w as f32 / scale, h as f32 / scale));
        scene.items.len()
    };
    let nodes = ui.host.len();
    eprintln!("[craie] wire render — {items} instances, {nodes} nodes");
    renderer.sync_atlas(&gpu, &mut ui.text.atlas);
    eprintln!("[craie] atlas upload: {} bytes", renderer.atlas_upload_bytes);
    let scene = ui.scene();
    dump_scene(&gpu, &mut renderer, scene, w, h, format, path);
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

    let session = bridge::Session::new();
    // Stand-in for the React worker: submit the demo transaction, then one
    // text update per second. Exercises submit → wake → apply → ack →
    // repaint through the same path craie-node drives from JS.
    let ticker = session.clone();
    std::thread::Builder::new()
        .name("craie-ticker".into())
        .spawn(move || {
            if ticker.submit(demo_txn()).is_err() {
                return;
            }
            for tick in 1u64.. {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let mut enc = Encoder::new();
                enc.set_text(
                    2,
                    &format!(
                        "In-process bridge: this line was submitted by a ticker \
                         thread {tick}s ago — one transaction per tick."
                    ),
                );
                if ticker.submit(enc.finish(1 + tick)).is_err() {
                    return;
                }
            }
        })
        .expect("ticker thread");

    platform::run(
        "craie — app",
        Size::new(800.0, 500.0),
        HostApp::new(session),
    );
}

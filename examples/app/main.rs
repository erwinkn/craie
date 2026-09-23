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

use craie_core::geom::Size;
use craie_platform_winit as platform;
use craie_platform_winit::app::HostApp;
use craie_render::{Gpu, Renderer};
use craie_ui::bridge;
use craie_ui::mutation::{Mutation, NodeKind, Transaction};
use craie_ui::ui::Ui;
use taffy::{AlignContent, AlignItems, Dimension, FlexDirection, LengthPercentage, Rect, Style};

const NIL: u32 = u32::MAX;

/// Seals a transaction as CRW2 bytes.
fn finish(mut t: Transaction<'_>, seq: u64) -> Vec<u8> {
    t.seq = seq;
    craie_ui::wire::encode(&t)
}

fn style(f: impl FnOnce(&mut Style)) -> Style {
    let mut s = Style::default();
    f(&mut s);
    s
}

/// The built-in demo, expressed as one transaction. The JS bridge produces
/// exactly this byte shape from a React tree.
fn demo_txn() -> Vec<u8> {
    let mut enc = Transaction::new(0);

    enc.style(&style(|s| {
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
    }));
    enc.style(&style(|s| {
        s.align_items = Some(AlignItems::CENTER);
        s.justify_content = Some(AlignContent::CENTER);
        s.size = taffy::Size {
            width: Dimension::length(220.0),
            height: Dimension::length(80.0),
        };
    }));

    // Root column
    enc.create(0, NodeKind::View);
    enc.push(Mutation::Layout { id: 0, style: 0 });
    enc.fill(0, 0x1B1D_24FF);
    enc.place(NIL, 0, NIL);

    // Title + body text
    enc.create(1, NodeKind::Text);
    enc.text(
        1,
        "Craie — driven entirely over the wire",
        28.0,
        0xECEC_F0FF,
    );
    enc.place(0, 1, NIL);

    enc.create(2, NodeKind::Text);
    enc.text(
        2,
        "This frame was decoded from a flat binary transaction: no per-node \
         objects, no diffing, just ops applied to a retained host.",
        15.0,
        0x9AA0_AEFF,
    );
    enc.place(0, 2, NIL);

    // A row of colored boxes
    enc.style(&style(|s| {
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
    }));
    enc.create(3, NodeKind::View);
    enc.push(Mutation::Layout { id: 3, style: 2 });
    enc.place(0, 3, NIL);

    let colors = [0x6DC7_FFFF, 0xB1E1_8AFF, 0xFFB4_6DFF];
    for (i, c) in colors.iter().enumerate() {
        let id = 10 + i as u32;
        enc.create(id, NodeKind::View);
        enc.push(Mutation::Layout { id: id, style: 1 });
        enc.fill(id, *c);
        enc.place(3, id, NIL);
        let tid = 20 + i as u32;
        enc.create(tid, NodeKind::Text);
        enc.text(tid, format!("box {i}"), 14.0, 0x1415_18FF);
        enc.place(id, tid, NIL);
    }

    // Spatial and clipping: a rotated card, an opacity group whose
    // children overlap (an isolated layer), a rounded clip whose content
    // is scrolled, and a bars surface fed by payload bytes.
    let row2 = 40;
    enc.create(row2, NodeKind::View);
    enc.layout(
        row2,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.gap = taffy::Size {
                width: LengthPercentage::length(24.0),
                height: LengthPercentage::length(0.0),
            };
            s.padding = Rect {
                left: LengthPercentage::length(8.0),
                right: LengthPercentage::length(0.0),
                top: LengthPercentage::length(24.0),
                bottom: LengthPercentage::length(0.0),
            };
        }),
    );
    enc.place(0, row2, NIL);
    let card = style(|s| {
        s.size = taffy::Size {
            width: Dimension::length(120.0),
            height: Dimension::length(90.0),
        };
        s.flex_shrink = 0.0;
    });

    // Rotated card with a label.
    enc.create(41, NodeKind::View);
    enc.layout(41, &card);
    enc.paint(41, Some(0x6DC7_FFFF), Some(10.0), Some((0xECEC_F0FF, 2.0)));
    enc.transform(41, craie_core::Affine::rotate(0.26));
    enc.place(row2, 41, NIL);
    enc.create(42, NodeKind::Text);
    enc.text(42, "rotated", 16.0, 0x1415_18FF);
    enc.layout(
        42,
        &style(|s| {
            s.margin = Rect {
                left: taffy::LengthPercentageAuto::length(12.0),
                right: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(12.0),
                bottom: taffy::LengthPercentageAuto::length(0.0),
            };
        }),
    );
    enc.place(41, 42, NIL);

    // Opacity group: two overlapping squares at 50%. Isolated, the
    // overlap is not darker than either square.
    enc.create(43, NodeKind::View);
    enc.layout(43, &card);
    enc.opacity(43, 0.5);
    enc.place(row2, 43, NIL);
    let square = |x: f32, y: f32| {
        style(|s| {
            s.position = taffy::Position::Absolute;
            s.inset = Rect {
                left: taffy::LengthPercentageAuto::length(x),
                right: taffy::LengthPercentageAuto::auto(),
                top: taffy::LengthPercentageAuto::length(y),
                bottom: taffy::LengthPercentageAuto::auto(),
            };
            s.size = taffy::Size {
                width: Dimension::length(70.0),
                height: Dimension::length(60.0),
            };
        })
    };
    for (id, x, y, c) in [
        (44u32, 0.0, 0.0, 0xFF6B_6BFFu32),
        (45, 45.0, 28.0, 0xFF6B_6BFF),
    ] {
        enc.create(id, NodeKind::View);
        enc.layout(id, &square(x, y));
        enc.fill(id, c);
        enc.place(43, id, NIL);
    }

    // Rounded clip with scrolled content: a tall column of stripes.
    enc.create(46, NodeKind::View);
    enc.layout(
        46,
        &style(|s| {
            s.size = taffy::Size {
                width: Dimension::length(120.0),
                height: Dimension::length(90.0),
            };
            s.flex_shrink = 0.0;
            s.flex_direction = FlexDirection::Column;
            s.overflow = taffy::Point {
                x: taffy::Overflow::Scroll,
                y: taffy::Overflow::Scroll,
            };
        }),
    );
    enc.paint(46, Some(0x2A2D_38FF), Some(18.0), None);
    enc.place(row2, 46, NIL);
    for i in 0..8u32 {
        let id = 50 + i;
        enc.create(id, NodeKind::View);
        enc.layout(
            id,
            &style(|s| {
                s.size = taffy::Size {
                    width: Dimension::length(120.0),
                    height: Dimension::length(22.0),
                };
                s.flex_shrink = 0.0;
            }),
        );
        enc.fill(id, if i % 2 == 0 { 0xB1E1_8AFF } else { 0x3A3D_4AFF });
        enc.place(46, id, NIL);
    }
    enc.command(46, craie_ui::mutation::Command::ScrollTo(0.0, 33.0));

    // Bars surface.
    let values: Vec<u8> = [0.3f32, 0.7, 0.45, 1.0, 0.6, 0.85, 0.2, 0.5]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    enc.create(47, NodeKind::Surface);
    enc.layout(47, &card);
    enc.paint(47, Some(0x2A2D_38FF), Some(6.0), None);
    enc.surface(
        47,
        craie_ui::surface::kind::BARS,
        [0x6DC7_C8FF, 0x6DC7_FFFF, 0, 0],
    );
    enc.payload(47, values);
    enc.place(row2, 47, NIL);

    finish(enc, 1)
}

// -------------------------------------------------------------- headless

/// Renders `scene` with `renderer` to an offscreen `format` texture and
/// saves a PNG of the result.
fn dump_scene(
    gpu: &Gpu,
    renderer: &mut Renderer,
    scene: &mut craie_scene::Scene,
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
    let view = Size::new(w as f32 / scale, h as f32 / scale);
    ui.render(view);
    // Scroll the clipped column after layout: a spatial-only change that
    // patches one transform record and rebuilds no chunk.
    let built = ui.counters().chunks_built;
    ui.scroll_to(craie_ui::host::NodeId(46), 0.0, 33.0);
    ui.render(view);
    assert_eq!(ui.counters().chunks_built, built, "scroll rebuilt a chunk");
    let chunks = ui.counters().chunks_built;
    let nodes = ui.host.len();
    eprintln!("[craie] wire render — {chunks} chunks, {nodes} nodes");
    renderer.prepare(&gpu, ui.scene_mut());
    eprintln!(
        "[craie] upload: {} bytes ({} atlas)",
        renderer.stats.upload_bytes, renderer.atlas_upload_bytes
    );
    dump_scene(&gpu, &mut renderer, ui.scene_mut(), w, h, format, path);
}

fn main() {
    // Text lays out on the system's fonts.
    craie_platform_winit::fonts::install();
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
                let mut enc = Transaction::new(0);
                enc.text(
                    2,
                    format!(
                        "In-process bridge: this line was submitted by a ticker \
                         thread {tick}s ago — one transaction per tick."
                    ),
                    15.0,
                    0x9AA0_AEFF,
                );
                if ticker.submit(finish(enc, 1 + tick)).is_err() {
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

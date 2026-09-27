//! Image nodes end to end on the GPU (headless adapter; skipped without
//! one): payload, decode by the platform decoder, and the textured quad
//! of each fit.

use craie_core::Size;
use craie_platform_winit::images;
use craie_render::{Gpu, Renderer};
use craie_ui::image::Fit;
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

const W: u32 = 140;
const H: u32 = 40;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255; 4];
const CLEAR: [u8; 4] = [0, 0, 0, 255];

/// 40 x 20: four 10 px vertical stripes, red, green, blue, white.
fn stripes() -> Vec<u8> {
    let rgba: Vec<u8> = (0..20)
        .flat_map(|_| (0..40).flat_map(|x| [RED, GREEN, BLUE, WHITE][x / 10]))
        .collect();
    craie_platform_winit::capture::encode_png(40, 20, &rgba)
}

fn px(v: f32) -> taffy::LengthPercentageAuto {
    taffy::LengthPercentageAuto::length(v)
}

/// Three 40 x 40 image nodes 50 pt apart: cover, contain, fill.
fn ui() -> Ui {
    let mut ui = Ui::new(1.0);
    ui.clear = 0x0000_00FF;
    let png = stripes();
    let mut t = Transaction::new(1);
    let row = taffy::Style {
        flex_direction: taffy::FlexDirection::Row,
        ..Default::default()
    };
    t.create(0, NodeKind::View)
        .layout(0, &row)
        .place(NIL, 0, NIL);
    for (i, fit) in [Fit::Cover, Fit::Contain, Fit::Fill]
        .into_iter()
        .enumerate()
    {
        let id = i as u32 + 1;
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(40.0),
                height: taffy::Dimension::length(40.0),
            },
            margin: taffy::Rect {
                left: px(if i == 0 { 0.0 } else { 10.0 }),
                right: px(0.0),
                top: px(0.0),
                bottom: px(0.0),
            },
            ..Default::default()
        };
        t.create(id, NodeKind::Image)
            .layout(id, &style)
            .payload(id, png.as_slice())
            .image_config(id, fit)
            .place(0, id, NIL);
    }
    ui.apply_txn(&t).unwrap();
    ui
}

/// Renders frames, answering decode requests in between, until none
/// are left.
fn settle(ui: &mut Ui) {
    for _ in 0..8 {
        ui.render(Size::new(W as f32, H as f32));
        let reqs = ui.take_image_requests();
        if reqs.is_empty() {
            return;
        }
        for r in &reqs {
            ui.image_result(images::run(r));
        }
    }
    panic!("image requests never settled");
}

/// Draws the prepared scene of `ui` into a W x H sRGB target and reads
/// it back (RGBA rows).
fn draw(gpu: &Gpu, r: &mut Renderer, ui: &mut Ui) -> Vec<[u8; 4]> {
    r.prepare(gpu, ui.scene_mut());
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    r.draw(gpu, &view, W, H, ui.scene_mut());
    let row = (W * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
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
                bytes_per_row: Some(row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((W * H) as usize);
    for y in 0..H as usize {
        for x in 0..W as usize {
            let at = y * row as usize + x * 4;
            out.push([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        }
    }
    out
}

/// Each stripe's color is exact at its center and along the box's
/// edges: the atlas gutter repeats the bitmap's edge pixels, so the
/// linear filter never mixes in black. Inside, stripes blend where they
/// meet.
#[test]
fn each_fit_draws_its_part_of_the_image() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut r = Renderer::new(&gpu, FORMAT);
    let mut ui = ui();
    settle(&mut ui);
    let img = draw(&gpu, &mut r, &mut ui);
    let at = |x: u32, y: u32| img[(y * W + x) as usize];

    // Cover: the middle 20 x 20 of the image (green, blue) fills the box,
    // edges and corners included.
    for y in [0, 2, 20, 37, 39] {
        assert_eq!((at(0, y), at(5, y)), (GREEN, GREEN), "cover row {y}");
        assert_eq!((at(34, y), at(39, y)), (BLUE, BLUE), "cover row {y}");
    }
    // Contain: the whole image, letterboxed to rows 10 to 30.
    let x0 = 50;
    for (k, c) in [RED, GREEN, BLUE, WHITE].into_iter().enumerate() {
        assert_eq!(at(x0 + 5 + 10 * k as u32, 20), c, "contain stripe {k}");
    }
    assert_eq!((at(x0 + 20, 4), at(x0 + 20, 35)), (CLEAR, CLEAR));
    // Fill: the whole image stretched to the box.
    let x0 = 100;
    for (k, c) in [RED, GREEN, BLUE, WHITE].into_iter().enumerate() {
        for y in [0, 2, 37, 39] {
            assert_eq!(at(x0 + 5 + 10 * k as u32, y), c, "fill stripe {k} row {y}");
        }
    }
    assert_eq!((at(x0, 0), at(x0 + 39, 39)), (RED, WHITE), "fill corners");
    // Between the boxes: nothing.
    assert_eq!(at(45, 20), CLEAR);

    // Three textures of the decoded sizes: 20 x 20 (the cover crop at
    // 1x), 40 x 20, 40 x 20.
    assert_eq!(ui.image_bytes(), (20 * 20 + 2 * 40 * 20) * 4);
    // Load events, one per node, with the natural size.
    let loads: Vec<_> = ui
        .take_events()
        .into_iter()
        .filter(|e| e.kind == craie_ui::events::out_kind::IMAGE)
        .map(|e| (e.node, e.key, e.x, e.y))
        .collect();
    assert_eq!(
        loads,
        [(1, 0, 40.0, 20.0), (2, 0, 40.0, 20.0), (3, 0, 40.0, 20.0)]
    );
}

/// Images do not clip to their own radius (`LEDGER.md` DF-31); a round
/// avatar clips them in a parent: radius and `overflow: hidden`.
#[test]
fn a_rounded_parent_clips_an_image() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut r = Renderer::new(&gpu, FORMAT);
    let mut ui = Ui::new(1.0);
    ui.clear = 0x0000_00FF;
    let side = taffy::Dimension::length(40.0);
    let frame = taffy::Style {
        size: taffy::Size {
            width: side,
            height: side,
        },
        overflow: taffy::Point {
            x: taffy::Overflow::Hidden,
            y: taffy::Overflow::Hidden,
        },
        ..Default::default()
    };
    let full = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        ..Default::default()
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &frame)
        .paint(0, None, Some(20.0), None)
        .place(NIL, 0, NIL)
        .create(1, NodeKind::Image)
        .layout(1, &full)
        .payload(1, stripes())
        .image_config(1, Fit::Fill)
        .place(0, 1, NIL);
    ui.apply_txn(&t).unwrap();
    settle(&mut ui);
    let img = draw(&gpu, &mut r, &mut ui);
    let at = |x: u32, y: u32| img[(y * W + x) as usize];
    assert_eq!(at(25, 20), BLUE);
    assert_eq!(at(15, 38), GREEN);
    assert_eq!(
        (at(1, 1), at(38, 1), at(1, 38), at(38, 38)),
        (CLEAR, CLEAR, CLEAR, CLEAR)
    );
}

/// A red square, its outer ring transparent black (a common export),
/// stretched 4x on white. The ring's color never shows: the decoder
/// gives transparent pixels their neighbours' color, so the filtered
/// edge fades from red to white with no darker pixel between.
#[test]
fn transparent_edges_have_no_dark_rim() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut r = Renderer::new(&gpu, FORMAT);
    let mut ui = Ui::new(1.0);
    ui.clear = 0xFFFF_FFFF;
    let rgba: Vec<u8> = (0..10)
        .flat_map(|y| (0..10).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let ring = x == 0 || y == 0 || x == 9 || y == 9;
            if ring { [0; 4] } else { RED }
        })
        .collect();
    let png = craie_platform_winit::capture::encode_png(10, 10, &rgba);
    let side = taffy::Dimension::length(40.0);
    let style = taffy::Style {
        size: taffy::Size {
            width: side,
            height: side,
        },
        ..Default::default()
    };
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::Image)
        .layout(1, &style)
        .payload(1, png)
        .image_config(1, Fit::Fill)
        .place(NIL, 1, NIL);
    ui.apply_txn(&t).unwrap();
    settle(&mut ui);
    let img = draw(&gpu, &mut r, &mut ui);
    let at = |x: u32, y: u32| img[(y * W + x) as usize];
    assert_eq!((at(0, 20), at(20, 20)), (WHITE, RED));
    // Along a row across the left edge: green and blue stay equal (a
    // mix of red and white, never of black), and red never dims.
    for x in 0..12 {
        let [r, g, b, _] = at(x, 20);
        assert!(r == 255 && g == b, "x {x}: {:?}", at(x, 20));
    }
}

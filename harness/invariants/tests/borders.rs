//! Borders per side through `Ui`, on the GPU (headless adapter;
//! skipped without one): a divider is one crisp line at the bottom, one
//! color makes a ring that follows the corner radius, mixed colors paint
//! each side in its own.

use craie_core::geom::Size;
use craie_render::{Gpu, Renderer};
use craie_scene::{Color, Scene};
use craie_ui::{
    border::BorderSides,
    mutation::{NodeKind, Transaction},
    ui::Ui,
};

const W: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Renders `s` into a W x W sRGB target and reads it back (RGBA rows).
fn render(gpu: &Gpu, r: &mut Renderer, s: &mut Scene) -> Vec<[u8; 4]> {
    let mut missing = Vec::new();
    s.prepare(Size::new(W as f32, W as f32), &mut missing);
    r.prepare(gpu, s);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: W,
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
    r.draw(gpu, &view, W, W, s);
    let row = (W * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * W) as u64,
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
                rows_per_image: Some(W),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: W,
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
    let mut out = Vec::with_capacity((W * W) as usize);
    for y in 0..W as usize {
        for x in 0..W as usize {
            let at = y * row as usize + x * 4;
            out.push([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        }
    }
    out
}

fn px(img: &[[u8; 4]], x: u32, y: u32) -> [u8; 4] {
    img[(y * W + x) as usize]
}

/// One device for the whole binary: the Vulkan validation layer crashes
/// when test threads create and destroy devices concurrently.
fn gpu() -> Option<(&'static Gpu, Renderer)> {
    static GPU: std::sync::OnceLock<Option<Gpu>> = std::sync::OnceLock::new();
    let gpu = GPU.get_or_init(Gpu::try_headless).as_ref()?;
    let r = Renderer::new(gpu, FORMAT);
    Some((gpu, r))
}

fn sized(width: f32, height: f32) -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(width),
            height: taffy::Dimension::length(height),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// A 32 × 32 white box at (16, 16) with `sides` and corner `radius`, on
/// black.
fn boxed(sides: BorderSides, radius: f32) -> Vec<[u8; 4]> {
    let (gpu, mut r) = match gpu() {
        Some(g) => g,
        None => return Vec::new(),
    };
    let mut ui = Ui::new(1.0);
    let mut root = sized(64.0, 64.0);
    root.padding = taffy::Rect::length(16.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &root)
        .append(u32::MAX, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(32.0, 32.0))
        .paint(1, Some(0xffff_ffff), Some(radius), None)
        .border_sides(1, sides)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    ui.scene_mut().clear = Color(0x0000_00ff);
    render(gpu, &mut r, ui.scene_mut())
}

const RED: u32 = 0xff00_00ff;

/// `borderBottomWidth: 1` in one color: the bottom row of the box is
/// that color, the rest stays the fill.
#[test]
fn a_divider_is_one_crisp_line() {
    let img = boxed(
        BorderSides {
            widths: [0.0, 0.0, 1.0, 0.0],
            colors: [RED; 4],
        },
        0.0,
    );
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert_eq!(px(&img, 32, 47), [255, 0, 0, 255], "the bottom row");
    assert_eq!(px(&img, 32, 46), [255, 255, 255, 255], "above it, the fill");
    assert_eq!(px(&img, 16, 47), [255, 0, 0, 255], "to the corner");
    assert_eq!(px(&img, 32, 16), [255, 255, 255, 255], "no top border");
}

/// One color on rounded corners: the ring follows the radius, so the
/// corner outside the curve stays clear.
#[test]
fn one_color_follows_the_radius() {
    let img = boxed(
        BorderSides {
            widths: [2.0; 4],
            colors: [RED; 4],
        },
        8.0,
    );
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert_eq!(px(&img, 32, 16), [255, 0, 0, 255], "the top edge");
    assert_eq!(px(&img, 32, 18), [255, 255, 255, 255], "inside the ring");
    assert_eq!(
        px(&img, 16, 16),
        [0, 0, 0, 255],
        "outside the rounded corner"
    );
}

/// Mixed colors: each side in its own; top and bottom cover the corners.
#[test]
fn mixed_colors_paint_each_side() {
    let blue = 0x0000_ffff;
    let img = boxed(
        BorderSides {
            widths: [2.0, 0.0, 0.0, 2.0],
            colors: [RED, 0, 0, blue],
        },
        0.0,
    );
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert_eq!(px(&img, 32, 16), [255, 0, 0, 255], "top");
    assert_eq!(px(&img, 16, 32), [0, 0, 255, 255], "left");
    assert_eq!(px(&img, 16, 16), [255, 0, 0, 255], "top covers the corner");
    assert_eq!(px(&img, 47, 32), [255, 255, 255, 255], "no right border");
}

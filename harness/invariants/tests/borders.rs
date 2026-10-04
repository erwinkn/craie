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

/// A white box `w` x `h` at (16, 16) with `sides`, a uniform border and
/// corner `radius`, on black; `then` runs a second commit before the
/// frame. Empty without a GPU.
fn scene(
    w: f32,
    h: f32,
    sides: Option<BorderSides>,
    uniform: Option<(u32, f32)>,
    radius: f32,
    then: impl FnOnce(&mut Ui),
) -> Vec<[u8; 4]> {
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
    t.create(1, NodeKind::View).layout(1, &sized(w, h)).paint(
        1,
        Some(0xffff_ffff),
        Some(radius),
        uniform,
    );
    if let Some(s) = sides {
        t.border_sides(1, s);
    }
    t.append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    then(&mut ui);
    ui.render(Size::new(64.0, 64.0));
    ui.scene_mut().clear = Color(0x0000_00ff);
    render(gpu, &mut r, ui.scene_mut())
}

/// A 32 × 32 box with `sides` (their own widths and colors) and corner
/// `radius`.
fn boxed(widths: [f32; 4], colors: [u32; 4], radius: f32) -> Vec<[u8; 4]> {
    let sides = BorderSides {
        widths,
        colors,
        fallback: 0,
    };
    scene(32.0, 32.0, Some(sides), None, radius, |_| {})
}

const RED: u32 = 0xff00_00ff;
const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED_PX: [u8; 4] = [255, 0, 0, 255];

/// `borderBottomWidth: 1` in one color: the bottom row of the box is
/// that color, the rest stays the fill.
#[test]
fn a_divider_is_one_crisp_line() {
    let img = boxed([0.0, 0.0, 1.0, 0.0], [RED; 4], 0.0);
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert_eq!(px(&img, 32, 47), RED_PX, "the bottom row");
    assert_eq!(px(&img, 32, 46), WHITE, "above it, the fill");
    assert_eq!(px(&img, 16, 47), RED_PX, "to the corner");
    assert_eq!(px(&img, 32, 16), WHITE, "no top border");
}

/// One color on rounded corners: the ring follows the radius, so the
/// corner outside the curve stays clear.
#[test]
fn one_color_follows_the_radius() {
    let img = boxed([2.0; 4], [RED; 4], 8.0);
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert_eq!(px(&img, 32, 16), RED_PX, "the top edge");
    assert_eq!(px(&img, 32, 18), WHITE, "inside the ring");
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
    let img = boxed([2.0, 0.0, 0.0, 2.0], [RED, 0, 0, blue], 0.0);
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert_eq!(px(&img, 32, 16), RED_PX, "top");
    assert_eq!(px(&img, 16, 32), [0, 0, 255, 255], "left");
    assert_eq!(px(&img, 16, 16), RED_PX, "top covers the corner");
    assert_eq!(px(&img, 47, 32), WHITE, "no right border");
}

/// A side of its own transparent color paints nothing beside sides of
/// one color: React Native's reserved border with one colored side
/// (#34 review: a full ring).
#[test]
fn transparent_sides_paint_nothing() {
    for colors in [[0, 0, RED, 0], [RED, 0, RED, 0]] {
        let img = boxed([1.0; 4], colors, 0.0);
        if img.is_empty() {
            eprintln!("no GPU adapter: skipped");
            return;
        }
        let top = if colors[0] == RED { RED_PX } else { WHITE };
        assert_eq!(px(&img, 32, 16), top, "{colors:x?}: top");
        assert_eq!(px(&img, 47, 32), WHITE, "{colors:x?}: right");
        assert_eq!(px(&img, 32, 47), RED_PX, "{colors:x?}: bottom");
        assert_eq!(px(&img, 16, 32), WHITE, "{colors:x?}: left");
    }
}

/// A 1 pt side is one device row wherever the box lands, as the uniform
/// border is (#34 review: none at 28.5, two at 29.5), in one color and
/// in mixed colors.
#[test]
fn hairlines_hold_at_half_pixel_edges() {
    let green = 0x00ff_00ff;
    for h in [28.0, 28.5, 29.5, 30.25] {
        for colors in [[RED; 4], [green, 0, RED, 0]] {
            let widths = if colors[0] == RED {
                [0.0, 0.0, 1.0, 0.0]
            } else {
                [1.0, 0.0, 1.0, 0.0]
            };
            let sides = BorderSides {
                widths,
                colors,
                fallback: 0,
            };
            let img = scene(32.0, h, Some(sides), None, 0.0, |_| {});
            if img.is_empty() {
                eprintln!("no GPU adapter: skipped");
                return;
            }
            let red_rows = (0..W).filter(|&y| px(&img, 32, y) == RED_PX).count();
            assert_eq!(red_rows, 1, "height {h}, {colors:x?}: red rows");
            // The line is the box's last row: the fill's edge.
            let last = (0..W)
                .rfind(|&y| px(&img, 32, y) != [0, 0, 0, 255])
                .unwrap();
            assert_eq!(px(&img, 32, last), RED_PX, "height {h}: at the fill's edge");
        }
    }
}

/// One color, unequal widths, rounded: every corner's arc keeps a
/// border (#34 review: the top-left arc vanished).
#[test]
fn unequal_widths_keep_their_rounded_corners() {
    let img = boxed([1.0, 1.0, 4.0, 1.0], [RED; 4], 12.0);
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    // Along each corner's diagonal, from outside the box inward: some
    // red before the fill (an arc 1 px wide blends with the fill on the
    // diagonal; a missing one blends the fill with black, gray).
    for (cx, cy, dx, dy) in [
        (16, 16, 1, 1),
        (47, 16, -1, 1),
        (16, 47, 1, -1),
        (47, 47, -1, -1),
    ] {
        let diag: Vec<[u8; 4]> = (0..12)
            .map(|k| px(&img, (cx + dx * k) as u32, (cy + dy * k) as u32))
            .collect();
        let reddish = diag.iter().any(|p| p[0] as i32 - p[1] as i32 > 60);
        assert!(reddish, "corner ({cx}, {cy}): {diag:?}");
    }
}

/// Sides replace the uniform border, even sides that paint nothing: all
/// four explicitly 0 wide draw no ring (#34 review: the uniform border
/// came back).
#[test]
fn explicit_zero_sides_paint_nothing() {
    let none = BorderSides {
        widths: [0.0; 4],
        colors: [RED; 4],
        fallback: 0,
    };
    let img = scene(32.0, 32.0, Some(none), Some((RED, 2.0)), 0.0, |_| {});
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert!(!img.contains(&RED_PX), "no red anywhere");
    // Without sides, the uniform border paints.
    let img = scene(32.0, 32.0, None, Some((RED, 2.0)), 0.0, |_| {});
    assert_eq!(px(&img, 32, 16), RED_PX);
}

/// A side whose color falls back draws in the uniform border's color,
/// and follows a later change of it, as `_focus: { borderColor }` on an
/// underlined input does (#34 review).
#[test]
fn fallen_back_colors_follow_the_uniform_border() {
    let blue = 0x0000_ffff;
    let green = 0x00ff_00ff;
    // `borderBottomWidth: 1, borderColor: blue`.
    let underline = BorderSides {
        widths: [0.0, 0.0, 1.0, 0.0],
        colors: [0; 4],
        fallback: 0b1111_1011,
    };
    let img = scene(32.0, 32.0, Some(underline), Some((blue, 0.0)), 0.0, |_| {});
    if img.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    assert_eq!(px(&img, 32, 47), [0, 0, 255, 255], "the uniform color");
    let img = scene(32.0, 32.0, Some(underline), Some((blue, 0.0)), 0.0, |ui| {
        let mut t = Transaction::new(2);
        t.paint(1, None, None, Some((green, 0.0)));
        ui.apply_txn(&t).unwrap();
    });
    assert_eq!(px(&img, 32, 47), [0, 255, 0, 255], "follows the change");
    assert_eq!(px(&img, 32, 16), WHITE, "the other sides stay 0 wide");
}

/// Mixed colors on rounded corners: every corner's arc is painted, the
/// top's color above and the bottom's below (#34 re-check: the arcs
/// between pieces were bare). An input with a highlighted underline,
/// and four colors.
#[test]
fn mixed_colors_keep_their_rounded_corners() {
    let (blue, green) = (0x0000_ffff, 0x00ff_00ff);
    // `borderWidth: 1, borderColor: red, borderBottomColor: green`, r 8.
    let input = BorderSides {
        widths: [0.0; 4],
        colors: [0, 0, green, 0],
        fallback: 0b1011_1111,
    };
    let underline = scene(32.0, 32.0, Some(input), Some((RED, 1.0)), 8.0, |_| {});
    if underline.is_empty() {
        eprintln!("no GPU adapter: skipped");
        return;
    }
    let four = boxed([2.0; 4], [RED, blue, green, blue], 8.0);
    // Along each corner's diagonal, from outside the box inward: the
    // expected hue before the fill.
    let hue = |p: [u8; 4], c: usize| {
        let (v, others) = (
            p[c] as i32,
            (0..3)
                .filter(|&k| k != c)
                .map(|k| p[k] as i32)
                .max()
                .unwrap(),
        );
        v - others > 60
    };
    for (img, name) in [(&underline, "underline"), (&four, "four colors")] {
        for (cx, cy, dx, dy, c) in [
            (16, 16, 1, 1, 0),
            (47, 16, -1, 1, 0),
            (16, 47, 1, -1, 1),
            (47, 47, -1, -1, 1),
        ] {
            let diag: Vec<[u8; 4]> = (0..10)
                .map(|k| px(img, (cx + dx * k) as u32, (cy + dy * k) as u32))
                .collect();
            assert!(
                diag.iter().any(|&p| hue(p, c)),
                "{name}, corner ({cx}, {cy}): {diag:?}"
            );
        }
    }
    // The straight sides: the input's in its uniform red, the four
    // colors' in blue.
    assert_eq!(px(&underline, 16, 32), RED_PX);
    assert_eq!(px(&four, 16, 32), [0, 0, 255, 255]);
    assert_eq!(px(&four, 47, 32), [0, 0, 255, 255]);
    assert_eq!(px(&four, 32, 47), [0, 255, 0, 255]);
}

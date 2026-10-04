//! Box shadows through `Ui`, on the GPU (headless adapter; skipped
//! without one), as Chrome draws them (#26 review): an inset shadow
//! leaves the painted border alone, a spread keeps a circle round, an
//! offscreen box whose shadow shows is built, and shadows follow
//! siblings, clips and scrolling.

use craie_core::geom::Size;
use craie_render::{Gpu, Renderer};
use craie_scene::{Color, Scene};
use craie_ui::{
    mutation::{NodeKind, Transaction},
    shadow::Shadow,
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
fn ui_box(radius: f32, border: f32, sh: Shadow) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut root = sized(64.0, 64.0);
    root.padding = taffy::Rect::length(16.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &root)
        .append(u32::MAX, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(32.0, 32.0))
        .paint(
            1,
            Some(0xffffffff),
            Some(radius),
            Some((0x0000ffff, border)),
        )
        .shadows(1, &[sh])
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    ui
}

/// An inset shadow is cut inside the painted border: a 4-point blue
/// border stays blue under a black inset spread of 4.
#[test]
fn an_inset_shadow_leaves_the_border() {
    let (gpu, mut r) = match gpu() {
        Some(g) => g,
        None => {
            eprintln!("no GPU adapter: skipped");
            return;
        }
    };
    let mut ui = ui_box(
        0.0,
        4.0,
        Shadow {
            spread: 4.0,
            color: 0x000000ff,
            inset: true,
            ..Default::default()
        },
    );
    let img = render(gpu, &mut r, ui.scene_mut());
    assert_eq!(
        px(&img, 17, 32),
        [0, 0, 255, 255],
        "inset shadow should leave the 4px blue border untouched"
    );
}
/// A radius past half the box casts the shadow its used radius does.
#[test]
fn a_radius_past_half_casts_the_used_radius() {
    let sh = Shadow {
        spread: 40.0,
        color: 0x000000ff,
        ..Default::default()
    };
    let a = ui_box(16.0, 0.0, sh);
    let b = ui_box(100.0, 0.0, sh);
    let rects = |ui: &Ui| {
        let c = ui.scene().chunk(1).unwrap();
        ui.scene().rects.get(c.rects).to_vec()
    };
    let a = rects(&a);
    let b = rects(&b);
    let effective = |r: &craie_scene::RectInstance| r.radius.min(r.rect[2].min(r.rect[3]) / 2.0);
    assert_eq!(
        effective(&a[0]),
        effective(&b[0]),
        "32px boxes with radius16 and100 have identical used radii and should cast identical shadows"
    );
}
/// A box below the viewport whose shadow reaches into it is built.
#[test]
fn an_offscreen_box_casts_into_view() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let mut style = sized(32.0, 32.0);
    style.position = taffy::Position::Absolute;
    style.inset.top = taffy::LengthPercentageAuto::length(120.0);
    style.inset.left = taffy::LengthPercentageAuto::length(16.0);
    t.create(0, NodeKind::View)
        .layout(0, &sized(64.0, 64.0))
        .append(u32::MAX, 0);
    t.create(1, NodeKind::View)
        .layout(1, &style)
        .shadows(
            1,
            &[Shadow {
                y: -100.0,
                color: 0xff0000ff,
                ..Default::default()
            }],
        )
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    assert!(
        ui.scene().chunk(1).is_some(),
        "box at y120 is deferred although its shadow is visible at y20"
    );
}
/// A circle with a spread of 40 casts a circle of radius 56.
#[test]
fn a_spread_circle_stays_round() {
    let (gpu, mut r) = match gpu() {
        Some(g) => g,
        None => {
            eprintln!("no GPU adapter: skipped");
            return;
        }
    };
    let mut ui = ui_box(
        16.0,
        0.0,
        Shadow {
            spread: 40.0,
            color: 0x000000ff,
            ..Default::default()
        },
    );
    let s = ui.scene_mut();
    s.clear = Color(0);
    let mut p = s.placement(1);
    p.offset = [40.0, 40.0];
    s.set_placement(1, p);
    let img = render(gpu, &mut r, s);
    assert_eq!(
        px(&img, 14, 14)[3],
        0,
        "circle radius16 plus40 spread should be circle radius56: this pixel is outside it, as in Chrome"
    );
}
/// A later sibling paints over a shadow, and a parent's overflow clips
/// it.
#[test]
fn shadows_follow_siblings_and_clips() {
    let (gpu, mut r) = match gpu() {
        Some(g) => g,
        None => {
            eprintln!("no GPU adapter: skipped");
            return;
        }
    };
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &sized(64.0, 64.0))
        .fill(0, 0x00ff00ff)
        .append(u32::MAX, 0);
    let abs = |w, h, x, y| {
        let mut s = sized(w, h);
        s.position = taffy::Position::Absolute;
        s.inset.left = taffy::LengthPercentageAuto::length(x);
        s.inset.top = taffy::LengthPercentageAuto::length(y);
        s
    };
    t.create(1, NodeKind::View)
        .layout(1, &abs(32.0, 32.0, 16.0, 16.0))
        .fill(1, 0xffffffff)
        .shadows(
            1,
            &[Shadow {
                spread: 4.0,
                color: 0xff0000ff,
                ..Default::default()
            }],
        )
        .append(0, 1);
    t.create(2, NodeKind::View)
        .layout(2, &abs(8.0, 8.0, 12.0, 12.0))
        .fill(2, 0x0000ffff)
        .append(0, 2);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    let img = render(gpu, &mut r, ui.scene_mut());
    assert_eq!(px(&img, 14, 32), [255, 0, 0, 255]);
    assert_eq!(px(&img, 14, 14), [0, 0, 255, 255]);
    let mut parent = abs(32.0, 32.0, 16.0, 16.0);
    parent.overflow = taffy::Point {
        x: taffy::Overflow::Hidden,
        y: taffy::Overflow::Hidden,
    };
    let mut t = Transaction::new(2);
    t.detach(2).remove(2).layout(1, &parent).shadows(1, &[]);
    t.create(3, NodeKind::View)
        .layout(3, &sized(16.0, 16.0))
        .fill(3, 0xffffffff)
        .shadows(
            3,
            &[Shadow {
                spread: 8.0,
                color: 0xff0000ff,
                ..Default::default()
            }],
        )
        .append(1, 3);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    let img = render(gpu, &mut r, ui.scene_mut());
    assert_eq!(px(&img, 12, 24), [0, 255, 0, 255]);
    assert_eq!(px(&img, 33, 24), [255, 0, 0, 255]);
}
/// Scrolling moves a shadow with its box, under the scroller's clip.
#[test]
fn shadows_scroll_with_their_box() {
    use craie_ui::mutation::Command;
    let (gpu, mut r) = match gpu() {
        Some(g) => g,
        None => {
            eprintln!("no GPU adapter: skipped");
            return;
        }
    };
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let mut root = sized(64.0, 64.0);
    root.overflow = taffy::Point {
        x: taffy::Overflow::Scroll,
        y: taffy::Overflow::Scroll,
    };
    t.create(0, NodeKind::View)
        .layout(0, &root)
        .fill(0, 0x00ff00ff)
        .append(u32::MAX, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(32.0, 120.0))
        .append(0, 1);
    let mut child = sized(16.0, 16.0);
    child.position = taffy::Position::Absolute;
    child.inset.left = taffy::LengthPercentageAuto::length(16.0);
    child.inset.top = taffy::LengthPercentageAuto::length(70.0);
    t.create(2, NodeKind::View)
        .layout(2, &child)
        .fill(2, 0xffffffff)
        .shadows(
            2,
            &[Shadow {
                spread: 4.0,
                color: 0xff0000ff,
                ..Default::default()
            }],
        )
        .append(1, 2);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    let before = render(gpu, &mut r, ui.scene_mut());
    assert_eq!(px(&before, 14, 52), [0, 255, 0, 255]);
    let mut t = Transaction::new(2);
    t.command(0, Command::ScrollTo(0.0, 20.0));
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(64.0, 64.0));
    let after = render(gpu, &mut r, ui.scene_mut());
    assert_eq!(px(&after, 14, 52), [255, 0, 0, 255]);
    assert_eq!(px(&after, 18, 52), [255, 255, 255, 255]);
}

/// A 100 × 40 box, radius 20, spread 40: CSS's coverage is 2 × 20 / 100,
/// and the shadow's radius 55.32, as Chrome draws it (#26 re-check).
#[test]
fn a_long_box_takes_the_css_outset_radius() {
    let (gpu, mut r) = match gpu() {
        Some(g) => g,
        None => {
            eprintln!("no GPU adapter: skipped");
            return;
        }
    };
    let mut ui = Ui::new(1.0);
    let mut root = sized(256.0, 256.0);
    root.padding = taffy::Rect::length(80.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &root)
        .append(u32::MAX, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 40.0))
        .paint(1, Some(0), Some(20.0), None)
        .shadows(
            1,
            &[Shadow {
                spread: 40.0,
                color: 0xffffffff,
                ..Default::default()
            }],
        )
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(256.0, 256.0));
    let radius = ui.scene().rects.get(ui.scene().chunk(1).unwrap().rects)[0].radius;
    ui.scene_mut().clear = Color(0);
    let img = render(gpu, &mut r, ui.scene_mut());
    println!(
        "rectangular pill: radius={radius}, pixel(56,56)={:?}; CSS radius=55.32",
        px(&img, 56, 56)
    );
    assert!(
        (radius - 55.32).abs() < 0.001,
        "CSS coverage = 2 * min(20/100,20/40) = 0.4; radius must be 55.32, got {radius}"
    );
}

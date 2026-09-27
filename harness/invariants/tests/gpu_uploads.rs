//! On a real device: an unchanged frame uploads zero bytes, a color
//! change uploads only paint records, and a scroll uploads only the one
//! transform record. Skips when the machine has no GPU adapter.

use craie_core::geom::Size;
use craie_render::{Gpu, Renderer};
use craie_ui::host::NodeId;
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

const VIEW: Size = Size {
    width: 320.0,
    height: 240.0,
};

#[test]
fn gpu_uploads_follow_changes() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(&gpu, format);
    let (w, h) = (640, 480);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());

    let mut ui = Ui::new(2.0);
    let mut s = taffy::Style::default();
    s.flex_direction = taffy::FlexDirection::Column;
    s.size.height = taffy::Dimension::length(120.0);
    s.overflow = taffy::Point {
        x: taffy::Overflow::Scroll,
        y: taffy::Overflow::Scroll,
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &s)
        .fill(0, 0x1415_18FF)
        .append(NIL, 0);
    let mut row = taffy::Style::default();
    row.size.height = taffy::Dimension::length(40.0);
    row.flex_shrink = 0.0;
    for i in 1..=8u32 {
        t.create(i, NodeKind::View)
            .layout(i, &row)
            .fill(i, 0x2A2D_38FF)
            .append(0, i);
    }
    t.create(20, NodeKind::Text)
        .text(20, "uploads", 16.0, 0xFFFF_FFFF)
        .append(1, 20);
    ui.apply_txn(&t).unwrap();

    let mut frame = |ui: &mut Ui| {
        ui.render(VIEW);
        renderer.prepare(&gpu, ui.scene_mut());
        renderer.draw(&gpu, &view, w, h, ui.scene_mut());
        renderer.stats.upload_bytes
    };
    assert!(frame(&mut ui) > 0);
    assert_eq!(frame(&mut ui), 0, "an unchanged frame uploads nothing");

    let mut t = Transaction::new(2);
    t.fill(3, 0xFF00_00FF);
    ui.apply_txn(&t).unwrap();
    assert_eq!(frame(&mut ui), 4, "a fill change uploads one paint record");

    ui.scroll_to(NodeId(0), 0.0, 30.0);
    assert_eq!(
        frame(&mut ui),
        size_of::<craie_scene::WorldGpu>() as u64,
        "a scroll uploads one world matrix"
    );
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
}

/// Renders `ui` at `scale` into a w×h target and reads back one pixel
/// (sRGB bytes, RGBA).
fn pixel(gpu: &Gpu, ui: &mut Ui, w: u32, h: u32, at: (u32, u32)) -> [u8; 4] {
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(gpu, format);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
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
    ui.render(Size::new(w as f32 / ui.scale, h as f32 / ui.scale));
    renderer.prepare(gpu, ui.scene_mut());
    renderer.draw(gpu, &view, w, h, ui.scene_mut());
    let row = (w * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
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
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
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
    let i = (at.1 * row + at.0 * 4) as usize;
    [data[i], data[i + 1], data[i + 2], data[i + 3]]
}

/// At 1x the window root record is the identity: its world row must
/// still reach the GPU (a zero row collapses every chunk).
#[test]
fn identity_world_renders_at_1x() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    for scale in [1.0f32, 2.0] {
        let mut ui = Ui::new(scale);
        ui.clear = 0x0000_00FF;
        let mut s = taffy::Style::default();
        s.size = taffy::Size {
            width: taffy::Dimension::length(40.0),
            height: taffy::Dimension::length(40.0),
        };
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View)
            .layout(0, &s)
            .fill(0, 0xFF00_00FF)
            .append(NIL, 0);
        ui.apply_txn(&t).unwrap();
        let px = pixel(
            &gpu,
            &mut ui,
            100,
            100,
            ((20.0 * scale) as u32, (20.0 * scale) as u32),
        );
        assert_eq!(px, [255, 0, 0, 255], "scale {scale}");
    }
}

/// At 2x, half-pixel origins and edges land where the CPU resolver says
/// (both round half to even).
#[test]
fn half_pixel_edges_match_resolver() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    for x in [10.25f32, 10.75] {
        let mut ui = Ui::new(2.0);
        ui.clear = 0x0000_00FF;
        let mut s = taffy::Style::default();
        s.position = taffy::Position::Absolute;
        s.inset.left = taffy::LengthPercentageAuto::length(x);
        s.size = taffy::Size {
            width: taffy::Dimension::length(20.25),
            height: taffy::Dimension::length(20.0),
        };
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View)
            .layout(0, &{
                let mut r = taffy::Style::default();
                r.size = taffy::Size {
                    width: taffy::Dimension::length(100.0),
                    height: taffy::Dimension::length(100.0),
                };
                r
            })
            .append(NIL, 0);
        t.create(1, NodeKind::View)
            .layout(1, &s)
            .fill(1, 0xFF00_00FF)
            .append(0, 1);
        ui.apply_txn(&t).unwrap();
        ui.render(Size::new(100.0, 100.0));
        let b = ui
            .scene()
            .resolve(&|r| r.0 as u64)
            .into_iter()
            .find(|p| p.color == 0xFF00_00FF)
            .unwrap()
            .bounds;
        let (x0, x1) = (b.origin.x as u32, b.max_x() as u32);
        let red = [255, 0, 0, 255];
        let y = 10;
        assert_eq!(
            pixel(&gpu, &mut ui, 200, 200, (x0, y)),
            red,
            "x {x}: first column {x0}"
        );
        assert_ne!(
            pixel(&gpu, &mut ui, 200, 200, (x0 - 1, y)),
            red,
            "x {x}: before {x0}"
        );
        assert_eq!(
            pixel(&gpu, &mut ui, 200, 200, (x1 - 1, y)),
            red,
            "x {x}: last column"
        );
        assert_ne!(
            pixel(&gpu, &mut ui, 200, 200, (x1, y)),
            red,
            "x {x}: after {x1}"
        );
    }
}

/// A glyph larger than an atlas page draws at its full size on the GPU
/// (a smaller bitmap scaled up), in the place the resolver says.
#[test]
fn oversized_glyph_draws_full_size() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut ui = Ui::new(1.0);
    ui.clear = 0x0000_00FF;
    ui.scene_mut().atlas = craie_scene::RasterAtlas::with_budget(256, 2, 1);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).append(NIL, 0);
    t.create(1, NodeKind::Text)
        .text(1, "I", 500.0, 0xFFFF_FFFF)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 700.0));
    let b = ui
        .scene()
        .resolve(&|r| r.0 as u64)
        .into_iter()
        .find(|p| p.kind == 1)
        .unwrap()
        .bounds;
    assert!(b.size.height > 256.0, "taller than a page: {b:?}");
    assert_eq!(ui.scene().atlas.stats.downscaled, 1);
    let c = (
        b.origin.x + b.size.width * 0.5,
        b.origin.y + b.size.height * 0.5,
    );
    let at = |x: f32, y: f32| (x as u32, y as u32);
    let white = [255, 255, 255, 255];
    let black = [0, 0, 0, 255];
    // The stem's center and its top and bottom thirds are covered.
    for y in [
        c.1,
        b.origin.y + b.size.height / 6.0,
        b.max_y() - b.size.height / 6.0,
    ] {
        assert_eq!(
            pixel(&gpu, &mut ui, 400, 700, at(c.0, y)),
            white,
            "stem at y {y}"
        );
    }
    // Outside the quad stays clear.
    assert_eq!(
        pixel(&gpu, &mut ui, 400, 700, at(c.0, b.max_y() + 8.0)),
        black
    );
    assert_eq!(
        pixel(&gpu, &mut ui, 400, 700, at(b.max_x() + 8.0, c.1)),
        black
    );
}

/// Scroll content half a device pixel off the grid: while moving, its
/// edge pixel is half covered; once settled, the snap reaches the GPU
/// and the edge is crisp. At 1x and 2x.
#[test]
fn settled_scroll_content_is_crisp() {
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    for scale in [1.0f32, 2.0] {
        let mut ui = Ui::new(scale);
        ui.clear = 0x0000_00FF;
        let sized = |w: f32, h: f32| {
            let mut s = taffy::Style::default();
            s.size = taffy::Size {
                width: taffy::Dimension::length(w),
                height: taffy::Dimension::length(h),
            };
            s.flex_shrink = 0.0;
            s
        };
        let mut scroller = sized(100.0, 60.0);
        scroller.flex_direction = taffy::FlexDirection::Column;
        scroller.overflow = taffy::Point {
            x: taffy::Overflow::Scroll,
            y: taffy::Overflow::Scroll,
        };
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View)
            .layout(0, &scroller)
            .append(NIL, 0);
        // A 20-px gap, then a white block: its top edge is what moves.
        t.create(1, NodeKind::View)
            .layout(1, &sized(100.0, 20.0))
            .append(0, 1);
        t.create(2, NodeKind::View)
            .layout(2, &sized(100.0, 200.0))
            .fill(2, 0xFFFF_FFFF)
            .append(0, 2);
        ui.apply_txn(&t).unwrap();
        let (w, h) = ((100.0 * scale) as u32, (60.0 * scale) as u32);
        ui.render(Size::new(100.0, 60.0));
        ui.scroll_to(NodeId(0), 0.0, 0.5 / scale);
        ui.set_time(1.0);
        // The block's top edge sits at 20*scale - 0.5 device px: the
        // pixel row above 20*scale is half covered.
        let row = (20.0 * scale) as u32 - 1;
        let moving = pixel(&gpu, &mut ui, w, h, (10, row));
        assert!(
            moving[0] > 40 && moving[0] < 230,
            "scale {scale}: moving edge half covered, got {moving:?}"
        );
        ui.set_time(1.0 + craie_ui::ui::SETTLE_SECS * 1.5);
        assert!(ui.settle());
        // Ties to even: -0.5 snaps to 0, so the row above is clear and
        // the edge row is fully white.
        assert_eq!(
            pixel(&gpu, &mut ui, w, h, (10, row)),
            [0, 0, 0, 255],
            "scale {scale}"
        );
        assert_eq!(
            pixel(&gpu, &mut ui, w, h, (10, row + 1)),
            [255, 255, 255, 255],
            "scale {scale}"
        );
    }
}

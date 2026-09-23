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
        renderer.draw(&gpu, &view, w, h, ui.scene());
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

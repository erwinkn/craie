//! Vector nodes, headless: imports a few SVG icons (the build-time
//! importer, used here directly), lays them out in a row at several
//! sizes, renders at 2x, and writes a PNG.
//!
//!   cargo run --release -p craie-platform-winit --example vector -- /tmp/craie-vector.png

use craie_core::Size;
use craie_render::{Gpu, Renderer};
use craie_ui::mutation::{NodeKind, Transaction};
use craie_ui::ui::Ui;

const NIL: u32 = u32::MAX;

const ICONS: &[&str] = &[
    // A heart: cubic curves, one solid fill.
    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
      <path d="M12 21s-7-4.4-9.5-8.6C.8 9.4 2.3 5.5 6 5.1c2.2-.2 3.6 1 4.4 2.3.3.5 1 .5 1.3 0 .8-1.3 2.2-2.5 4.4-2.3 3.7.4 5.2 4.3 3.5 7.3C19 16.6 12 21 12 21z" fill="#e5484d"/>
    </svg>"##,
    // A gradient disc with a stroked ring.
    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
      <defs><radialGradient id="g" cx="0.35" cy="0.35" r="0.7">
        <stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#3e63dd"/>
      </radialGradient></defs>
      <circle cx="12" cy="12" r="9" fill="url(#g)"/>
      <circle cx="12" cy="12" r="10" fill="none" stroke="#1d2b5e" stroke-width="1.5"/>
    </svg>"##,
    // A star: straight edges, round joins, a linear gradient.
    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
      <defs><linearGradient id="l" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#ffd23f"/><stop offset="1" stop-color="#f76b15"/>
      </linearGradient></defs>
      <path d="M12 2l2.9 6.3 6.9.7-5.2 4.6 1.5 6.8L12 17l-6.1 3.4 1.5-6.8L2.2 9l6.9-.7z"
            fill="url(#l)" stroke="#8a3d00" stroke-width="1" stroke-linejoin="round"/>
    </svg>"##,
    // A ring by even-odd, and a stroked check mark with round caps.
    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
      <path d="M12 2a10 10 0 1 0 0 20a10 10 0 1 0 0-20zm0 3a7 7 0 1 1 0 14a7 7 0 1 1 0-14z"
            fill="#30a46c" fill-rule="evenodd"/>
      <path d="M8 12.5l2.7 2.7L16.5 9" fill="none" stroke="#30a46c" stroke-width="2"
            stroke-linecap="round" stroke-linejoin="round"/>
    </svg>"##,
];

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/craie-vector.png".into());
    let Some(gpu) = Gpu::try_headless() else {
        eprintln!("no GPU adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(&gpu, format);
    let mut ui = Ui::new(2.0);
    ui.clear = 0xF4F4_F5FF;
    let mut t = Transaction::new(1);
    let mut column = taffy::Style::default();
    column.flex_direction = taffy::FlexDirection::Column;
    column.gap = taffy::Size {
        width: taffy::LengthPercentage::length(12.0),
        height: taffy::LengthPercentage::length(12.0),
    };
    column.padding = taffy::Rect {
        left: taffy::LengthPercentage::length(16.0),
        right: taffy::LengthPercentage::length(16.0),
        top: taffy::LengthPercentage::length(16.0),
        bottom: taffy::LengthPercentage::length(16.0),
    };
    t.create(0, NodeKind::View)
        .layout(0, &column)
        .place(NIL, 0, NIL);
    let mut id = 1;
    for (row, size) in [16.0f32, 32.0, 64.0].into_iter().enumerate() {
        let mut r = taffy::Style::default();
        r.flex_direction = taffy::FlexDirection::Row;
        r.gap = column.gap;
        let row_id = 100 + row as u32;
        t.create(row_id, NodeKind::View)
            .layout(row_id, &r)
            .place(0, row_id, NIL);
        for svg in ICONS {
            let (asset, report) = craie_svg_import::import(svg.as_bytes()).expect("svg");
            assert!(report.unsupported.is_empty(), "{report:?}");
            let mut s = taffy::Style::default();
            s.size.width = taffy::Dimension::length(size);
            t.create(id, NodeKind::Vector)
                .layout(id, &s)
                .payload(id, craie_vector::asset::encode(&asset))
                .place(row_id, id, NIL);
            id += 1;
        }
    }
    ui.apply_txn(&t).expect("valid");
    let (w, h) = (2 * 360, 2 * 190);
    ui.render(Size::new(w as f32 / 2.0, h as f32 / 2.0));
    let mut missing = Vec::new();
    ui.scene_mut()
        .prepare(Size::new(w as f32, h as f32), &mut missing);
    renderer.prepare(&gpu, ui.scene_mut());
    craie_platform_winit::capture::capture_png(
        &gpu,
        &mut renderer,
        format,
        &mut ui,
        (w, h),
        std::path::Path::new(&out),
    )
    .expect("png");
    eprintln!(
        "wrote {out} (layers {}, msaa {})",
        renderer.stats.layers, renderer.stats.msaa
    );
}

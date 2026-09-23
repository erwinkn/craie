//! Path meshes on the GPU (headless adapter; skipped without one): edges
//! anti-aliased by multisampling, frames without paths single-sampled,
//! gradients, and paths inside opacity layers.

use craie_core::geom::{Affine, Rect, Size};
use craie_render::{Gpu, Renderer};
use craie_scene::{ChunkWriter, GradientPaint, NONE, OrderItem, Placement, Scene, gradient};
use craie_vector::{FillRule, Mesh, Path, fill};

const W: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn scene() -> Scene {
    let mut s = Scene::new();
    let root = s.transforms.alloc(Affine::IDENTITY, NONE);
    s.transforms.set_order(vec![root]);
    s
}

fn place(s: &mut Scene, id: u32) {
    s.set_placement(
        id,
        Placement {
            offset: [0.0, 0.0],
            transform: 0,
            clip: NONE,
        },
    );
}

fn mesh(w: &mut ChunkWriter, m: &Mesh, color: u32) {
    let paint = w.paint(color);
    w.mesh(&m.vertices, &m.indices, paint, false);
}

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

fn gpu() -> Option<(Gpu, Renderer)> {
    let gpu = Gpu::try_headless()?;
    let r = Renderer::new(&gpu, FORMAT);
    Some((gpu, r))
}

/// A filled circle: solid inside, clear outside, and partial coverage
/// along its edge (multisampling); the frame is multisampled because it
/// draws a path.
#[test]
fn paths_draw_with_antialiased_edges() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut s = scene();
    let mut w = ChunkWriter::new();
    let circle = fill(&Path::circle(32.0, 32.0, 20.0), FillRule::NonZero, 0.1).unwrap();
    mesh(&mut w, &circle, 0xFF00_00FF);
    s.commit_chunk(0, &mut w);
    place(&mut s, 0);
    s.set_order(vec![OrderItem::Chunk(0)], vec![]);
    let img = render(&gpu, &mut r, &mut s);
    assert!(r.stats.msaa);
    assert_eq!(px(&img, 32, 32), [255, 0, 0, 255]);
    assert_eq!(px(&img, 2, 2), [0, 0, 0, 255]);
    // Along a row where the edge crosses pixels (y = 18.5: x = 32 +-
    // 14.76), the edge pixels blend.
    let edge: Vec<u8> = (0..W).map(|x| px(&img, x, 18)[0]).collect();
    assert!(edge.iter().any(|&v| v > 20 && v < 235), "{edge:?}");
    // Symmetric: the left and right edges match.
    let first = edge.iter().position(|&v| v > 0).unwrap();
    let last = edge.iter().rposition(|&v| v > 0).unwrap();
    assert_eq!(first + last, 63, "{edge:?}");
}

/// A frame that draws no path renders single-sampled, as before.
#[test]
fn frames_without_paths_stay_single_sampled() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut s = scene();
    let mut w = ChunkWriter::new();
    let fill_ = w.paint(0x00FF_00FF);
    w.rect(Rect::new(8.0, 8.0, 16.0, 16.0), 0.0, fill_);
    s.commit_chunk(0, &mut w);
    place(&mut s, 0);
    s.set_order(vec![OrderItem::Chunk(0)], vec![]);
    let img = render(&gpu, &mut r, &mut s);
    assert!(!r.stats.msaa);
    assert_eq!(px(&img, 16, 16), [0, 255, 0, 255]);
}

fn gradient_scene(g: GradientPaint) -> Scene {
    let mut s = scene();
    let mut w = ChunkWriter::new();
    let square = fill(&Path::rect(0.0, 0.0, 64.0, 64.0), FillRule::NonZero, 0.1).unwrap();
    let slot = w.gradient(&g);
    w.mesh(&square.vertices, &square.indices, slot, true);
    s.commit_chunk(0, &mut w);
    place(&mut s, 0);
    s.set_order(vec![OrderItem::Chunk(0)], vec![]);
    s
}

/// Gradients pad past their ends and interpolate premultiplied in sRGB
/// between stops; the gradient's own transform maps chunk-local space.
#[test]
fn gradients_interpolate_their_stops() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    // Red to blue from x = 8 to x = 56 (pixel centers at +0.5).
    let mut s = gradient_scene(GradientPaint {
        kind: gradient::LINEAR,
        geometry: [8.0, 0.0, 56.0, 0.0],
        to_gradient: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        stops: vec![(0.0, 0xFF00_00FF), (1.0, 0x0000_FFFF)],
    });
    let img = render(&gpu, &mut r, &mut s);
    assert_eq!(px(&img, 2, 10), [255, 0, 0, 255], "padded start");
    assert_eq!(px(&img, 60, 10), [0, 0, 255, 255], "padded end");
    let mid = px(&img, 31, 10);
    // t = (31.5 - 8) / 48 = 0.4896: channels near 125 and 130.
    assert!(
        (mid[0] as i32 - 130).abs() <= 2 && (mid[2] as i32 - 125).abs() <= 2,
        "{mid:?}"
    );
    // Three stops, radial, through a scale: the middle stop at the
    // radius' midpoint in gradient space.
    let mut s = gradient_scene(GradientPaint {
        kind: gradient::RADIAL,
        geometry: [0.0, 0.0, 10.0, 0.0],
        // Chunk-local (32, 32) is the gradient center; 1 unit = 2 px.
        to_gradient: [0.5, 0.0, 0.0, 0.5, -16.0, -16.0],
        stops: vec![(0.0, 0xFFFF_FFFF), (0.5, 0x00FF_00FF), (1.0, 0x0000_00FF)],
    });
    let img = render(&gpu, &mut r, &mut s);
    let center = px(&img, 32, 32);
    assert!(center[0] > 230 && center[1] > 240, "{center:?}");
    assert_eq!(px(&img, 1, 1), [0, 0, 0, 255], "outside the radius");
    // Pixel 41's center is 9.5 px out: t = 0.475, just short of the
    // pure green stop (red 5% of white left); pixel 42's is 10.5 px out,
    // just past it (green falling toward black).
    let g = px(&img, 41, 32);
    assert!(g[1] == 255 && g[0] > 5 && g[0] < 25, "{g:?}");
    let g = px(&img, 42, 32);
    assert!(g[0] == 0 && g[1] > 235 && g[1] < 250, "{g:?}");
}

/// A path in an opacity layer composites at the layer's opacity (the
/// layer target is multisampled and resolved like the window).
#[test]
fn paths_in_layers_composite() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut s = scene();
    s.clear = craie_scene::Color(0x0000_00FF);
    let mut w = ChunkWriter::new();
    let square = fill(&Path::rect(8.0, 8.0, 48.0, 48.0), FillRule::NonZero, 0.1).unwrap();
    mesh(&mut w, &square, 0xFFFF_FFFF);
    s.commit_chunk(0, &mut w);
    place(&mut s, 0);
    s.set_order(
        vec![
            OrderItem::BeginLayer(0),
            OrderItem::Chunk(0),
            OrderItem::EndLayer,
        ],
        vec![0.5],
    );
    let img = render(&gpu, &mut r, &mut s);
    assert!(r.stats.msaa && r.stats.layers == 1);
    // White at half coverage over black, in sRGB: 188.
    let c = px(&img, 32, 32);
    assert!((c[0] as i32 - 188).abs() <= 2, "{c:?}");
    assert_eq!(px(&img, 2, 2), [0, 0, 0, 255]);
}

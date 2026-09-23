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

/// A path in an opacity layer composites at the layer's opacity (its
/// run renders in a multisampled layer nested in the opacity layer).
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
    // The opacity layer, and the path run's multisampled layer in it.
    assert!(r.stats.msaa && r.stats.layers == 2);
    // White at half coverage over black, in sRGB: 188.
    let c = px(&img, 32, 32);
    assert!((c[0] as i32 - 188).abs() <= 2, "{c:?}");
    assert_eq!(px(&img, 2, 2), [0, 0, 0, 255]);
}

/// S5A-01: a rect's anti-aliased edge is the same in a multisampled
/// frame (a path elsewhere) as in a single-sampled one: its quad covers
/// every pixel its edge touches, so multisampling adds no coverage of
/// its own.
#[test]
fn rect_edges_match_across_sample_counts() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let frame = |r: &mut Renderer, with_path: bool| {
        let mut s = scene();
        // Unsnapped: the rect's edges sit at quarter pixels.
        s.transforms.set_snap(0, false);
        let mut w = ChunkWriter::new();
        let white = w.paint(0xFFFF_FFFF);
        w.rect(Rect::new(8.25, 8.25, 16.0, 16.0), 0.0, white);
        s.commit_chunk(0, &mut w);
        place(&mut s, 0);
        let mut order = vec![OrderItem::Chunk(0)];
        if with_path {
            let dot = fill(&Path::circle(56.0, 56.0, 4.0), FillRule::NonZero, 0.1).unwrap();
            mesh(&mut w, &dot, 0xFF00_00FF);
            s.commit_chunk(1, &mut w);
            place(&mut s, 1);
            order.push(OrderItem::Chunk(1));
        }
        s.set_order(order, vec![]);
        let img = render(&gpu, r, &mut s);
        assert_eq!(r.stats.msaa, with_path);
        img
    };
    let (plain, ms) = (frame(&mut r, false), frame(&mut r, true));
    for (x, y) in [
        (8, 16),
        (16, 8),
        (24, 16),
        (16, 24),
        (8, 8),
        (24, 24),
        (16, 16),
    ] {
        let (a, b) = (px(&plain, x, y), px(&ms, x, y));
        assert!(
            (a[0] as i32 - b[0] as i32).abs() <= 1,
            "({x}, {y}): {a:?} vs {b:?}"
        );
    }
    // The edge pixel is partly covered (0.75 of it), not cut.
    let edge = px(&plain, 8, 16)[0];
    assert!(edge > 200 && edge < 250, "{edge}");
}

/// S5A-03: a gradient with no stops draws nothing (the words after it
/// are not stops).
#[test]
fn gradients_without_stops_draw_nothing() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut s = scene();
    let mut w = ChunkWriter::new();
    let square = fill(&Path::rect(0.0, 0.0, 64.0, 64.0), FillRule::NonZero, 0.1).unwrap();
    let slot = w.gradient(&GradientPaint {
        kind: gradient::LINEAR,
        geometry: [0.0, 0.0, 64.0, 0.0],
        to_gradient: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        stops: vec![],
    });
    // Words that would read as a red stop.
    w.paint(0);
    w.paint(0xFF00_00FF);
    w.mesh(&square.vertices, &square.indices, slot, true);
    s.commit_chunk(0, &mut w);
    place(&mut s, 0);
    s.set_order(vec![OrderItem::Chunk(0)], vec![]);
    let img = render(&gpu, &mut r, &mut s);
    assert_eq!(px(&img, 32, 32), [0, 0, 0, 255]);
}

/// Renders `build` twice: alone, and with a distant path that makes the
/// frame multisampled. Returns (single-sampled, multisampled) images.
fn with_and_without_path(
    gpu: &Gpu,
    r: &mut Renderer,
    build: &dyn Fn(&mut Scene, &mut ChunkWriter) -> Vec<OrderItem>,
) -> (Vec<[u8; 4]>, Vec<[u8; 4]>) {
    let mut frame = |with_path: bool| {
        let mut s = scene();
        let mut w = ChunkWriter::new();
        let mut order = build(&mut s, &mut w);
        if with_path {
            // In its own untransformed space, whatever `build` did to 0.
            let own = s.transforms.alloc(Affine::IDENTITY, NONE);
            s.transforms.set_order(vec![0, own]);
            let dot = fill(&Path::circle(58.0, 58.0, 3.0), FillRule::NonZero, 0.1).unwrap();
            mesh(&mut w, &dot, 0xFF00_00FF);
            s.commit_chunk(9, &mut w);
            s.set_placement(
                9,
                Placement {
                    offset: [0.0, 0.0],
                    transform: own,
                    clip: NONE,
                },
            );
            order.push(OrderItem::Chunk(9));
        }
        s.set_order(order, vec![1.0]);
        let img = render(gpu, r, &mut s);
        assert_eq!(r.stats.msaa, with_path);
        img
    };
    (frame(false), frame(true))
}

fn assert_same(a: &[[u8; 4]], b: &[[u8; 4]], region: (u32, u32, u32, u32)) {
    for y in region.1..region.3 {
        for x in region.0..region.2 {
            let (p, q) = (px(a, x, y), px(b, x, y));
            assert!(
                (0..3).all(|k| (p[k] as i32 - q[k] as i32).abs() <= 1),
                "({x}, {y}): {p:?} vs {q:?}"
            );
        }
    }
}

/// S5A-07: a glyph's edges are the same at any sample count: its quad
/// reaches one texel into the cleared atlas gutter.
#[test]
fn glyph_edges_match_across_sample_counts() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let (a, b) = with_and_without_path(&gpu, &mut r, &|s, w| {
        s.transforms.set_snap(0, false);
        let id = s.atlas.new_id(8, 8, false);
        s.atlas.insert(id, &[255u8; 64]);
        let white = w.paint(0xFFFF_FFFF);
        w.glyph(8.25, 8.25, 8.0, 8.0, id, white);
        s.commit_chunk(0, w);
        place(s, 0);
        vec![OrderItem::Chunk(0)]
    });
    assert_same(&a, &b, (4, 4, 24, 24));
    let edge = px(&a, 8, 12)[0];
    assert!(edge > 200 && edge < 250, "{edge}");
}

/// S5A-08: a zero-area rect draws nothing, at any sample count.
#[test]
fn zero_area_rects_draw_nothing() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let (a, b) = with_and_without_path(&gpu, &mut r, &|s, w| {
        s.transforms.set_snap(0, false);
        let white = w.paint(0xFFFF_FFFF);
        w.rect(Rect::new(8.5, 8.0, 0.0, 16.0), 0.0, white);
        w.rect(Rect::new(20.0, 8.5, 16.0, 0.0), 0.0, white);
        s.commit_chunk(0, w);
        place(s, 0);
        vec![OrderItem::Chunk(0)]
    });
    // Snapped: a 0.3-wide rect whose edges round to one column has no
    // area either.
    let (c, d) = with_and_without_path(&gpu, &mut r, &|s, w| {
        let white = w.paint(0xFFFF_FFFF);
        w.rect(Rect::new(8.1, 8.0, 0.3, 16.0), 0.0, white);
        s.commit_chunk(0, w);
        place(s, 0);
        vec![OrderItem::Chunk(0)]
    });
    // Rotated (not axis-aligned): zero width still draws nothing.
    let (e, f) = with_and_without_path(&gpu, &mut r, &|s, w| {
        s.transforms
            .set_local(0, Affine::translate(20.0, 20.0).mul(&Affine::rotate(0.5)));
        let white = w.paint(0xFFFF_FFFF);
        w.rect(Rect::new(0.0, 0.0, 0.0, 16.0), 0.0, white);
        s.commit_chunk(0, w);
        place(s, 0);
        vec![OrderItem::Chunk(0)]
    });
    for img in [&a, &b, &c, &d, &e, &f] {
        for y in 0..40 {
            for x in 0..40 {
                assert_eq!(px(img, x, y), [0, 0, 0, 255], "({x}, {y})");
            }
        }
    }
}

/// S5A-09: a rect under an anisotropic transform draws the same inside
/// an opacity-1 layer as outside it: the layer's bounds hold its padded
/// quad.
#[test]
fn stretched_rects_fit_their_layer() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let draw = |r: &mut Renderer, layered: bool| {
        let mut s = scene();
        s.transforms
            .set_local(0, Affine([8.0, 0.01, 0.0, 0.125, 8.0, 20.0]));
        s.transforms.set_snap(0, false);
        let mut w = ChunkWriter::new();
        let white = w.paint(0xFFFF_FFFF);
        w.rect(Rect::new(0.0, 0.0, 6.0, 16.0), 0.0, white);
        s.commit_chunk(0, &mut w);
        place(&mut s, 0);
        let order = if layered {
            vec![
                OrderItem::BeginLayer(0),
                OrderItem::Chunk(0),
                OrderItem::EndLayer,
            ]
        } else {
            vec![OrderItem::Chunk(0)]
        };
        s.set_order(order, vec![1.0]);
        render(&gpu, r, &mut s)
    };
    let (direct, layered) = (draw(&mut r, false), draw(&mut r, true));
    assert_same(&direct, &layered, (0, 0, W, W));
    // The rect spans x 8..56 and y 20..22: drawn.
    assert!(px(&direct, 30, 21)[0] > 100);
}

/// S5A-10: a clip that matches a path's own edges changes nothing: the
/// clip is tested per sample, not multiplied into the path's coverage.
#[test]
fn clips_matching_a_path_change_nothing() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let draw = |r: &mut Renderer, clipped: bool| {
        let mut s = scene();
        s.transforms.set_snap(0, false);
        let mut w = ChunkWriter::new();
        let square = fill(&Path::rect(8.25, 8.25, 16.0, 16.0), FillRule::NonZero, 0.1).unwrap();
        mesh(&mut w, &square, 0xFFFF_FFFF);
        s.commit_chunk(0, &mut w);
        let mut p = Placement {
            offset: [0.0, 0.0],
            transform: 0,
            clip: NONE,
        };
        if clipped {
            s.clips.set_all(vec![craie_scene::ClipRecord {
                rect: Rect::new(8.25, 8.25, 16.0, 16.0),
                radius: 0.0,
                transform: 0,
                parent: NONE,
                open: [false, false],
            }]);
            p.clip = 0;
        }
        s.set_placement(0, p);
        s.set_order(vec![OrderItem::Chunk(0)], vec![]);
        render(&gpu, r, &mut s)
    };
    let (plain, clipped) = (draw(&mut r, false), draw(&mut r, true));
    assert_same(&plain, &clipped, (4, 4, 30, 30));
    let edge = px(&plain, 8, 16)[0];
    assert!(edge > 200 && edge < 250, "{edge}");
}

/// S5A-15, S5A-17: rects and glyphs never render multisampled (paths
/// draw in layers of their own), so a path elsewhere changes none of
/// their pixels, under any transform: anisotropic rects, reduced and
/// enlarged glyphs.
#[test]
fn transformed_rects_and_glyphs_ignore_paths_elsewhere() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let (a, b) = with_and_without_path(&gpu, &mut r, &|s, w| {
        s.transforms
            .set_local(0, Affine([8.0, 0.01, 0.0, 0.125, 8.25, 20.25]));
        s.transforms.set_snap(0, false);
        let white = w.paint(0xFFFF_FFFF);
        w.rect(Rect::new(0.0, 0.0, 6.0, 16.0), 0.0, white);
        s.commit_chunk(0, w);
        place(s, 0);
        vec![OrderItem::Chunk(0)]
    });
    // Everything above the added dot (rows 55..61).
    assert_same(&a, &b, (0, 0, W, 52));
    for scale in [0.25, 6.0] {
        let (a, b) = with_and_without_path(&gpu, &mut r, &|s, w| {
            s.transforms
                .set_local(0, Affine([scale, 0.0, 0.0, scale, 8.49, 8.49]));
            s.transforms.set_snap(0, false);
            let id = s.atlas.new_id(8, 8, false);
            s.atlas.insert(id, &[255u8; 64]);
            let white = w.paint(0xFFFF_FFFF);
            w.glyph(0.0, 0.0, 8.0, 8.0, id, white);
            s.commit_chunk(0, w);
            place(s, 0);
            vec![OrderItem::Chunk(0)]
        });
        // Everything above the added dot (rows 55..61).
        assert_same(&a, &b, (0, 0, W, 52));
    }
}

/// S5A-16: an enlarged glyph draws the same inside an opacity-1 layer
/// as outside it (its layer holds its whole footprint).
#[test]
fn enlarged_glyphs_fit_their_layer() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let draw = |r: &mut Renderer, layered: bool| {
        let mut s = scene();
        s.transforms
            .set_local(0, Affine([6.0, 0.0, 0.0, 6.0, 8.0, 8.0]));
        s.transforms.set_snap(0, false);
        let id = s.atlas.new_id(8, 8, false);
        s.atlas.insert(id, &[255u8; 64]);
        let mut w = ChunkWriter::new();
        let white = w.paint(0xFFFF_FFFF);
        w.glyph(0.0, 0.0, 8.0, 8.0, id, white);
        s.commit_chunk(0, &mut w);
        place(&mut s, 0);
        let order = if layered {
            vec![
                OrderItem::BeginLayer(0),
                OrderItem::Chunk(0),
                OrderItem::EndLayer,
            ]
        } else {
            vec![OrderItem::Chunk(0)]
        };
        s.set_order(order, vec![1.0]);
        render(&gpu, r, &mut s)
    };
    let (direct, layered) = (draw(&mut r, false), draw(&mut r, true));
    assert_same(&direct, &layered, (0, 0, W, W));
    assert!(px(&direct, 30, 30)[0] > 200);
}

/// S5A-13: a glyph in a slot an evicted bitmap used draws nothing of
/// that bitmap beside itself.
#[test]
fn glyphs_in_reused_slots_stay_clean() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut s = scene();
    s.atlas = craie_scene::RasterAtlas::with_budget(64, 1, 0);
    s.transforms.set_snap(0, false);
    let big = s.atlas.new_id(62, 62, false);
    s.atlas.insert(big, &[255u8; 62 * 62]);
    s.atlas.begin_epoch();
    let small = s.atlas.new_id(8, 8, false);
    s.atlas.insert(small, &[255u8; 64]);
    assert_eq!(s.atlas.stats.evictions, 1);
    let mut w = ChunkWriter::new();
    let white = w.paint(0xFFFF_FFFF);
    w.glyph(8.75, 8.75, 8.0, 8.0, small, white);
    s.commit_chunk(0, &mut w);
    place(&mut s, 0);
    s.set_order(vec![OrderItem::Chunk(0)], vec![]);
    let img = render(&gpu, &mut r, &mut s);
    for (x, y) in [(17, 12), (12, 17), (18, 12), (7, 12)] {
        assert_eq!(px(&img, x, y), [0, 0, 0, 255], "({x}, {y})");
    }
}

/// S5A-14: nested clips that match a path's edges change nothing: each
/// clip is its own inside test per sample.
#[test]
fn nested_clips_matching_a_path_change_nothing() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let draw = |r: &mut Renderer, clips: usize| {
        let mut s = scene();
        s.transforms.set_snap(0, false);
        let mut w = ChunkWriter::new();
        let square = fill(&Path::rect(8.25, 8.25, 16.0, 16.0), FillRule::NonZero, 0.1).unwrap();
        mesh(&mut w, &square, 0xFFFF_FFFF);
        s.commit_chunk(0, &mut w);
        let records = (0..clips)
            .map(|k| craie_scene::ClipRecord {
                rect: Rect::new(8.25, 8.25, 16.0, 16.0),
                radius: 0.0,
                transform: 0,
                parent: if k == 0 { NONE } else { k as u32 - 1 },
                open: [false, false],
            })
            .collect();
        s.clips.set_all(records);
        s.set_placement(
            0,
            Placement {
                offset: [0.0, 0.0],
                transform: 0,
                clip: if clips == 0 { NONE } else { clips as u32 - 1 },
            },
        );
        s.set_order(vec![OrderItem::Chunk(0)], vec![]);
        render(&gpu, r, &mut s)
    };
    let plain = draw(&mut r, 0);
    assert_same(&plain, &draw(&mut r, 1), (4, 4, 30, 30));
    assert_same(&plain, &draw(&mut r, 2), (4, 4, 30, 30));
}

/// A run of meshes composites in painter order between the content
/// before and after it: red rect, blue mesh, green rect, overlapping.
#[test]
fn path_layers_keep_painter_order() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut s = scene();
    let mut w = ChunkWriter::new();
    let red = w.paint(0xFF00_00FF);
    w.rect(Rect::new(8.0, 8.0, 24.0, 24.0), 0.0, red);
    s.commit_chunk(0, &mut w);
    let blue = fill(&Path::rect(16.0, 16.0, 24.0, 24.0), FillRule::NonZero, 0.1).unwrap();
    mesh(&mut w, &blue, 0x0000_FFFF);
    s.commit_chunk(1, &mut w);
    let green = w.paint(0x00FF_00FF);
    w.rect(Rect::new(24.0, 24.0, 24.0, 24.0), 0.0, green);
    s.commit_chunk(2, &mut w);
    for id in 0..3 {
        place(&mut s, id);
    }
    s.set_order((0..3).map(OrderItem::Chunk).collect(), vec![]);
    let img = render(&gpu, &mut r, &mut s);
    assert_eq!(px(&img, 10, 10), [255, 0, 0, 255]);
    assert_eq!(px(&img, 20, 20), [0, 0, 255, 255]);
    assert_eq!(px(&img, 30, 30), [0, 255, 0, 255]);
    assert_eq!(px(&img, 38, 18), [0, 0, 255, 255]);
    assert_eq!(r.stats.layers, 1);
}

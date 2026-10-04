//! Blending on the GPU (headless adapter; skipped without one): on a
//! plain target translucent colors and antialiased edges blend
//! sRGB-encoded values, as Chromium draws them; on an *-srgb target they
//! blend in linear light. The Chromium values come from headless Chrome
//! (chrome-headless-shell 1208) on the same colors.

use craie_core::geom::{Affine, Rect, Size};
use craie_render::{Blending, Gpu, Renderer};
use craie_scene::{ChunkWriter, Color, NONE, OrderItem, Placement, Scene};

const W: u32 = 64;

/// Renders `s` into a W x W sRGB target and reads it back (RGBA rows).
fn render(gpu: &Gpu, r: &mut Renderer, s: &mut Scene, format: wgpu::TextureFormat) -> Vec<[u8; 4]> {
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
        format,
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

fn gpu() -> Option<&'static Gpu> {
    static GPU: std::sync::OnceLock<Option<Gpu>> = std::sync::OnceLock::new();
    GPU.get_or_init(Gpu::try_headless).as_ref()
}

/// On a canvas of #18181b: a 16 % blue tint, a half-black square over
/// #fafafa, and a white square whose left edge falls mid-pixel (x 10.5,
/// in a space that does not snap).
fn draw(gpu: &Gpu, blending: Blending) -> Vec<[u8; 4]> {
    let format = blending.offscreen();
    let mut r = Renderer::new(gpu, format);
    let mut s = Scene::new();
    let root = s.transforms.alloc(Affine::IDENTITY, NONE);
    s.transforms.set_order(vec![root]);
    s.transforms.set_snap(root, false);
    s.clear = Color(0x1818_1bff);
    let mut w = ChunkWriter::new();
    let rect = |w: &mut ChunkWriter, r: Rect, c: u32| {
        let p = w.paint(c);
        w.rect(r, 0.0, p);
    };
    rect(&mut w, Rect::new(0.0, 0.0, 32.0, 32.0), 0x3b82_f629);
    rect(&mut w, Rect::new(32.0, 0.0, 32.0, 32.0), 0xfafa_faff);
    rect(&mut w, Rect::new(36.0, 4.0, 24.0, 24.0), 0x0000_0080);
    rect(&mut w, Rect::new(10.5, 40.0, 10.0, 10.0), 0xffff_ffff);
    s.commit_chunk(0, &mut w);
    s.set_placement(
        0,
        Placement {
            offset: [0.0, 0.0],
            transform: 0,
            clip: NONE,
        },
    );
    s.set_order(vec![OrderItem::Chunk(0)], vec![]);
    let img = render(gpu, &mut r, &mut s, format);
    // Bgra targets read back in that order.
    img.into_iter().map(|[b, g, r, a]| [r, g, b, a]).collect()
}

fn near(got: [u8; 4], want: [u8; 3]) -> bool {
    (0..3).all(|i| got[i].abs_diff(want[i]) <= 1)
}

/// The default: as Chromium. A 16 % tint (0x29 of alpha) of #3b82f6 over
/// #18181b reads (29, 41, 62); half black over #fafafa reads 125; a
/// half-covered white edge on #18181b reads (140, 140, 141).
#[test]
fn srgb_blending_matches_chromium() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let img = draw(gpu, Blending::Srgb);
    assert!(
        near(px(&img, 16, 16), [29, 41, 62]),
        "tint {:?}",
        px(&img, 16, 16)
    );
    assert!(
        near(px(&img, 48, 16), [125, 125, 125]),
        "half black {:?}",
        px(&img, 48, 16)
    );
    let edge = px(&img, 10, 45);
    assert!(near(edge, [140, 140, 141]), "edge {edge:?}");
    assert!(near(px(&img, 11, 45), [255, 255, 255]));
}

/// Linear light (`Blending::Linear`): the same tint reads stronger and a
/// half-covered edge brighter, as physical light adds.
#[test]
fn linear_blending_reads_stronger() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let img = draw(gpu, Blending::Linear);
    let tint = px(&img, 16, 16);
    assert!(tint[2] > 62 + 10, "tint {tint:?}");
    assert!(
        near(px(&img, 48, 16), [182, 182, 182]),
        "half black {:?}",
        px(&img, 48, 16)
    );
    assert!(px(&img, 10, 45)[0] > 180, "edge {:?}", px(&img, 10, 45));
}

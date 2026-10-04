//! Box shadows on the GPU (headless adapter; skipped without one): a
//! blurred shape against the analytic Gaussian, a crisp ring on the
//! box's edge, an inset shadow inside the box only, and offset and
//! spread moving and growing the shape.

use craie_core::geom::{Affine, Rect, Size};
use craie_render::{Gpu, Renderer};
use craie_scene::{BoxShadow, ChunkWriter, Color, NONE, OrderItem, Placement, Scene};

const W: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The box every case cuts against: 32 x 32 at (16, 16).
const BOX: Rect = Rect {
    origin: craie_core::geom::Point { x: 16.0, y: 16.0 },
    size: Size {
        width: 32.0,
        height: 32.0,
    },
};

fn scene() -> Scene {
    let mut s = Scene::new();
    let root = s.transforms.alloc(Affine::IDENTITY, NONE);
    s.transforms.set_order(vec![root]);
    // Clear to transparent: a white shadow's alpha is its coverage.
    s.clear = Color(0);
    s
}

/// One chunk holding `shadow` alone (no fill: what shows is the shadow).
fn draw(gpu: &Gpu, r: &mut Renderer, shadow: BoxShadow) -> Vec<[u8; 4]> {
    let mut s = scene();
    let mut w = ChunkWriter::new();
    let at = w.reserve_rect();
    w.set_shadow(at, &shadow);
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
    render(gpu, r, &mut s)
}

fn shadow(shape: Rect, radius: f32, sigma: f32, inset: bool) -> BoxShadow {
    BoxShadow {
        shape,
        radius,
        sigma,
        box_rect: BOX,
        box_radius: 0.0,
        color: 0xFFFF_FFFF,
        inset,
    }
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

/// One device for the whole binary: the Vulkan validation layer crashes
/// when test threads create and destroy devices concurrently.
fn gpu() -> Option<(&'static Gpu, Renderer)> {
    static GPU: std::sync::OnceLock<Option<Gpu>> = std::sync::OnceLock::new();
    let gpu = GPU.get_or_init(Gpu::try_headless).as_ref()?;
    let r = Renderer::new(gpu, FORMAT);
    Some((gpu, r))
}

/// erf (Abramowitz and Stegun 7.1.26, error under 1.5e-7).
fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-x * x).exp();
    y.copysign(x)
}

/// A square of half size `h` centered at 0, blurred by σ, at (x, y):
/// the product of two one-dimensional box blurs.
fn blurred(x: f64, y: f64, h: f64, sigma: f64) -> f64 {
    let f = |t: f64| {
        0.5 * (erf((t + h) / (sigma * 2f64.sqrt())) - erf((t - h) / (sigma * 2f64.sqrt())))
    };
    f(x) * f(y)
}

/// Alpha at pixel (x, y), as coverage in [0, 1].
fn alpha(img: &[[u8; 4]], x: u32, y: u32) -> f64 {
    px(img, x, y)[3] as f64 / 255.0
}

/// An outer shadow σ = 4 around the box: outside it, the analytic
/// Gaussian blur of the shape within 0.02; inside, nothing (CSS cuts an
/// outer shadow out under its box).
#[test]
fn an_outer_shadow_is_the_blurred_shape_outside_the_box() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let img = draw(gpu, &mut r, shadow(BOX, 0.0, 4.0, false));
    for (x, y) in [
        (15, 32),
        (13, 32),
        (10, 32),
        (6, 32),
        (2, 32),
        (12, 12),
        (8, 40),
        (50, 50),
    ] {
        // Pixel centers, from the box's center (32, 32).
        let want = blurred(x as f64 + 0.5 - 32.0, y as f64 + 0.5 - 32.0, 16.0, 4.0);
        let got = alpha(&img, x, y);
        assert!(
            (got - want).abs() < 0.02,
            "({x}, {y}): {got} against {want}"
        );
    }
    assert!(alpha(&img, 13, 32) > 0.2, "the blur reaches past the box");
    for (x, y) in [(16, 32), (32, 32), (47, 47)] {
        assert_eq!(alpha(&img, x, y), 0.0, "({x}, {y}) is under the box");
    }
}

/// A ring (spread 1, no blur) is one crisp pixel on the box's edge.
#[test]
fn a_ring_is_one_crisp_pixel_around_the_box() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let ring = Rect::new(15.0, 15.0, 34.0, 34.0);
    let img = draw(gpu, &mut r, shadow(ring, 0.0, 0.0, false));
    let row: Vec<u8> = (12..20).map(|x| px(&img, x, 32)[3]).collect();
    assert_eq!(row, [0, 0, 0, 255, 0, 0, 0, 0]);
    assert_eq!(px(&img, 48, 32)[3], 255);
    assert_eq!(px(&img, 32, 15)[3], 255);
    assert_eq!(px(&img, 32, 48)[3], 255);
    assert_eq!(px(&img, 15, 15)[3], 255, "a square ring's corner");
}

/// An inset shadow shows inside the box only: 1 less the blurred shape.
#[test]
fn an_inset_shadow_shows_inside_the_box() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let img = draw(gpu, &mut r, shadow(BOX, 0.0, 4.0, true));
    for (x, y) in [(16, 32), (18, 32), (22, 32), (32, 32), (17, 17)] {
        let want = 1.0 - blurred(x as f64 + 0.5 - 32.0, y as f64 + 0.5 - 32.0, 16.0, 4.0);
        let got = alpha(&img, x, y);
        assert!(
            (got - want).abs() < 0.02,
            "({x}, {y}): {got} against {want}"
        );
    }
    assert!(alpha(&img, 16, 32) > 0.4, "dark at the edge");
    assert!(alpha(&img, 32, 32) < 0.01, "clear in the middle");
    for (x, y) in [(15, 32), (8, 8), (50, 32)] {
        assert_eq!(alpha(&img, x, y), 0.0, "({x}, {y}) is outside the box");
    }
}

/// A hard shadow offset (4, 4) with spread 2 covers its moved, grown
/// shape where the box is not.
#[test]
fn offset_and_spread_move_and_grow_the_shape() {
    let Some((gpu, mut r)) = gpu() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    // The box moved by (4, 4), grown by 2: (18, 18) to (54, 54).
    let shape = Rect::new(18.0, 18.0, 36.0, 36.0);
    let img = draw(gpu, &mut r, shadow(shape, 0.0, 0.0, false));
    assert_eq!(px(&img, 50, 50)[3], 255, "below and right of the box");
    assert_eq!(px(&img, 53, 30)[3], 255);
    assert_eq!(px(&img, 54, 30)[3], 0, "past the grown edge");
    assert_eq!(px(&img, 17, 30)[3], 0, "left of the moved shape");
    assert_eq!(px(&img, 30, 30)[3], 0, "under the box");
}

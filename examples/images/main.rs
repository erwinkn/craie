//! Image decoding cost (work item 8): a 12-megapixel photo shown small.
//! A synthetic 4,000 x 3,000 photo (smooth gradients plus grain, so it
//! compresses like one), as JPEG (quality 85) and PNG, through the
//! platform decoder (`images::run`, the worker's body):
//!
//! - probe: the header, for the natural size;
//! - decode only: the full image to pixels (`image::load_from_memory`);
//! - decode at 80 x 80, cover (a 40 pt avatar at 2x: a 3,000 x 3,000
//!   crop, averaged down), and at 400 x 300, contain (200 pt at 2x);
//!
//! with the texture bytes each result holds against the full image's.
//! Times are medians of `RUNS`, one thread.
//!
//!   cargo run --release -p craie-platform-winit --example images

use std::io::Cursor;
use std::time::Instant;

use craie_platform_winit::images;
use craie_ui::image::{ImageId, ImageRequest, ImageResult};
use image::codecs::jpeg::JpegEncoder;
use image::{ImageFormat, RgbImage};

const RUNS: usize = 7;
const W: u32 = 4000;
const H: u32 = 3000;

fn photo() -> RgbImage {
    let mut seed = 0x2545_F491u32;
    RgbImage::from_fn(W, H, |x, y| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let grain = (seed % 17) as f32 - 8.0;
        let (u, v) = (x as f32 / W as f32, y as f32 / H as f32);
        let r = 40.0 + 180.0 * u + 20.0 * (v * 9.0).sin() + grain;
        let g = 60.0 + 120.0 * v + 30.0 * (u * 7.0).cos() + grain;
        let b = 90.0 + 100.0 * (1.0 - u) * v + grain;
        image::Rgb([r, g, b].map(|c| c.clamp(0.0, 255.0) as u8))
    })
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn median(f: impl FnMut() -> f64) -> f64 {
    let mut v: Vec<f64> = std::iter::repeat_with(f).take(RUNS).collect();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn decode(bytes: &[u8], crop: [u32; 4], width: u32, height: u32) -> (f64, usize) {
    let req = ImageRequest::Decode {
        id: ImageId(1),
        bytes: bytes.into(),
        crop,
        width,
        height,
    };
    let mut held = 0;
    let t = median(|| {
        let t = Instant::now();
        let r = images::run(&req);
        let e = ms(t);
        let ImageResult::Pixels { rgba, .. } = r else {
            panic!("{r:?}")
        };
        held = rgba.len();
        e
    });
    (t, held)
}

fn main() {
    let img = photo();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&img)
        .unwrap();
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    let full = (W * H * 4) as usize;
    println!("{W} x {H} photo; a full RGBA texture would hold {full} bytes");
    for (name, bytes) in [("JPEG q85", &jpeg), ("PNG", &png)] {
        let probe = median(|| {
            let t = Instant::now();
            let r = images::run(&ImageRequest::Probe {
                id: ImageId(1),
                bytes: bytes.as_slice().into(),
            });
            assert!(matches!(r, ImageResult::Size { .. }));
            ms(t)
        });
        let only = median(|| {
            let t = Instant::now();
            std::hint::black_box(image::load_from_memory(bytes).unwrap());
            ms(t)
        });
        let (avatar, avatar_bytes) = decode(bytes, [500, 0, 3000, 3000], 80, 80);
        let (card, card_bytes) = decode(bytes, [0, 0, W, H], 400, 300);
        println!(
            "{name} ({} KB): probe {probe:.3} ms, decode only {only:.1} ms",
            bytes.len() / 1024
        );
        println!(
            "  80 x 80 cover: {avatar:.1} ms, {avatar_bytes} texture bytes ({:.0}x less)",
            full as f64 / avatar_bytes as f64
        );
        println!(
            "  400 x 300 contain: {card:.1} ms, {card_bytes} texture bytes ({:.0}x less)",
            full as f64 / card_bytes as f64
        );
    }
}

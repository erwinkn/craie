//! Image decoding for `Ui` image nodes, on one worker thread.
//!
//! The core queues requests (`Ui::take_image_requests`): a probe reads
//! a header for the natural size, a decode reads the pixels of a source
//! rect scaled down to the size the node shows them at. The worker
//! answers each with an `ImageResult` and wakes the event loop, which
//! feeds it back (`Decoder::pump`). PNG, JPEG, WebP, and GIF (its first
//! frame), with the EXIF orientation applied.
//!
//! Decoding is bounded: an image over `MAX_PIXELS`, or whose buffers
//! would pass `MAX_ALLOC`, fails before anything is allocated (a 250 KB
//! PNG can claim 16,000 x 16,000 pixels).

use std::collections::VecDeque;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use craie_ui::image::{ImageId, ImageRequest, ImageResult};
use craie_ui::ui::Ui;
use image::imageops::{self, FilterType};
use image::metadata::Orientation;
use image::{
    ColorType, DynamicImage, ImageBuffer, ImageDecoder, ImageReader, Limits, Pixel, RgbaImage,
};

/// Pixels an image may have: 8,000 x 8,000.
pub const MAX_PIXELS: u64 = 64_000_000;
/// Bytes the image crate's buffers may take at once: the decoded image
/// and the RGBA copy of a format that needs one (a 64 MP RGB8 photo
/// takes 192 MB; a 64 MP RGBA16 PNG, 512 MB plus a 256 MB copy, fails).
/// Codecs may add up to about one more image of their own (lossless
/// WebP decodes through a `w*h*4` buffer; progressive JPEG keeps its
/// coefficients).
pub const MAX_ALLOC: u64 = 512 << 20;
/// Either side, as the codecs check it.
const MAX_SIDE: u32 = 32_768;

/// Requests waiting, and results for the UI thread.
#[derive(Default)]
struct Queue {
    /// Probes size layout: they go first.
    probes: VecDeque<ImageRequest>,
    /// At most one per image (a newer one replaces it).
    decodes: VecDeque<ImageRequest>,
    results: Vec<ImageResult>,
    /// The request the worker is running.
    running: Option<ImageId>,
    /// The worker died (a panic `work` could not catch).
    dead: bool,
    closed: bool,
}

impl Queue {
    /// Drops the queued work of `dropped` images, then queues `requests`.
    fn add(&mut self, dropped: &[ImageId], requests: Vec<ImageRequest>) {
        self.probes.retain(|r| !dropped.contains(&r.id()));
        self.decodes.retain(|r| !dropped.contains(&r.id()));
        for req in requests {
            match req {
                ImageRequest::Probe { .. } => self.probes.push_back(req),
                ImageRequest::Decode { id, .. } => {
                    match self.decodes.iter_mut().find(|r| r.id() == id) {
                        Some(queued) => *queued = req,
                        None => self.decodes.push_back(req),
                    }
                }
            }
        }
    }

    fn next(&mut self) -> Option<ImageRequest> {
        self.probes.pop_front().or_else(|| self.decodes.pop_front())
    }
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

type Run = fn(&ImageRequest) -> ImageResult;
type Wake = Arc<dyn Fn() + Send + Sync>;

/// The worker and its queue.
pub struct Decoder {
    shared: Arc<Shared>,
    wake: Wake,
    run: Run,
}

impl Decoder {
    /// Starts the worker. `wake` runs after each result, off the UI
    /// thread. The worker ends with the decoder.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Decoder {
        Decoder::with(run, Arc::new(wake))
    }

    fn with(run: Run, wake: Wake) -> Decoder {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            ready: Condvar::new(),
        });
        spawn(shared.clone(), wake.clone(), run);
        Decoder { shared, wake, run }
    }

    /// Feeds finished results to `ui` and queues its new requests,
    /// after dropping queued work for images it no longer shows.
    /// Returns whether a result arrived (the UI may owe a paint).
    pub fn pump(&mut self, ui: &mut Ui) -> bool {
        let results = {
            let mut q = self.shared.lock();
            // `work` catches a codec's panic, so this is for a panic it
            // could not: its request fails and a new worker takes over.
            if q.dead {
                q.dead = false;
                if let Some(id) = q.running.take() {
                    let error = "the image decoder stopped".into();
                    q.results.push(ImageResult::Failed { id, error });
                }
                spawn(self.shared.clone(), self.wake.clone(), self.run);
            }
            std::mem::take(&mut q.results)
        };
        let any = !results.is_empty();
        for r in results {
            ui.image_result(r);
        }
        let dropped = ui.take_dropped_images();
        let requests = ui.take_image_requests();
        if !dropped.is_empty() || !requests.is_empty() {
            self.shared.lock().add(&dropped, requests);
            self.shared.ready.notify_one();
        }
        any
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.ready.notify_one();
    }
}

fn spawn(shared: Arc<Shared>, wake: Wake, run: Run) {
    std::thread::Builder::new()
        .name("craie-images".into())
        .spawn(move || work(&shared, &*wake, run))
        .expect("spawn the image decoder");
}

/// Marks the worker dead if it unwinds, and wakes the UI to replace it.
struct Watch<'a>(&'a Shared, &'a (dyn Fn() + Send + Sync));

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.lock().dead = true;
            (self.1)();
        }
    }
}

/// The worker: probes first, then decodes, oldest first. A panic in a
/// codec fails its image, not the worker.
fn work(shared: &Shared, wake: &(dyn Fn() + Send + Sync), run: Run) {
    let _watch = Watch(shared, wake);
    loop {
        let req = {
            let mut q = shared.lock();
            loop {
                if q.closed {
                    return;
                }
                if let Some(req) = q.next() {
                    q.running = Some(req.id());
                    break req;
                }
                q = shared.ready.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let result =
            catch_unwind(AssertUnwindSafe(|| run(&req))).unwrap_or_else(|_| ImageResult::Failed {
                id: req.id(),
                error: "the image decoder panicked".into(),
            });
        {
            let mut q = shared.lock();
            q.running = None;
            q.results.push(result);
        }
        let _ = catch_unwind(AssertUnwindSafe(wake));
    }
}

/// Answers one request (blocking: the worker's body; tests and
/// benchmarks call it directly).
pub fn run(req: &ImageRequest) -> ImageResult {
    let id = req.id();
    match req {
        ImageRequest::Probe { bytes, .. } => match probe(bytes) {
            Ok([width, height]) => ImageResult::Size { id, width, height },
            Err(error) => ImageResult::Failed { id, error },
        },
        ImageRequest::Decode {
            bytes,
            crop,
            width,
            height,
            ..
        } => match decode(bytes, *crop, [*width, *height], MAX_ALLOC) {
            Ok(rgba) => ImageResult::Pixels {
                id,
                crop: *crop,
                width: *width,
                height: *height,
                rgba,
            },
            Err(error) => ImageResult::Failed { id, error },
        },
    }
}

fn limits(alloc: u64) -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(alloc);
    limits
}

/// A decoder for `bytes` that has read the header, and the size it
/// claims, within `MAX_PIXELS`.
fn reader(bytes: &[u8], limits: Limits) -> Result<(impl ImageDecoder + '_, u32, u32), String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.limits(limits);
    let d = reader.into_decoder().map_err(|e| e.to_string())?;
    let (w, h) = d.dimensions();
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!(
            "{w} x {h} is over the {} MP limit",
            MAX_PIXELS / 1_000_000
        ));
    }
    Ok((d, w, h))
}

/// Whether `o` swaps width and height.
fn turns(o: Orientation) -> bool {
    use Orientation::*;
    matches!(o, Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH)
}

/// The EXIF orientation. An unreadable EXIF chunk shows the image
/// unturned, as browsers do.
fn orientation(d: &mut impl ImageDecoder, bytes: &[u8]) -> Orientation {
    let webp = bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP");
    if webp && !webp_chunks_fit(bytes) {
        return Orientation::NoTransforms;
    }
    d.orientation().unwrap_or(Orientation::NoTransforms)
}

/// Whether each chunk of a RIFF (WebP) file fits in what is left of
/// it. image-webp allocates an EXIF chunk's declared size before
/// reading it, past `Limits`: 78 bytes can ask for 4 GiB.
fn webp_chunks_fit(bytes: &[u8]) -> bool {
    let mut rest = bytes.get(12..).unwrap_or_default();
    while let Some(head) = rest.get(..8) {
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
        let left = rest.len() as u64 - 8;
        if size > left {
            return false;
        }
        // Chunks are padded to an even size (the last one may not be).
        rest = &rest[8 + (size + size % 2).min(left) as usize..];
    }
    true
}

/// The natural size: the header's, turned by the EXIF orientation.
fn probe(bytes: &[u8]) -> Result<[u32; 2], String> {
    let (mut d, w, h) = reader(bytes, limits(MAX_ALLOC))?;
    let o = orientation(&mut d, bytes);
    Ok(if turns(o) { [h, w] } else { [w, h] })
}

/// `crop` of the oriented image, scaled to `size`: RGBA, straight
/// alpha, sRGB. The decoder's buffers are reserved against `alloc`
/// first (`from_decoder` checks no limit itself).
fn decode(bytes: &[u8], crop: [u32; 4], size: [u32; 2], alloc: u64) -> Result<Vec<u8>, String> {
    let mut limits = limits(alloc);
    let (mut d, w, h) = reader(bytes, limits.clone())?;
    let o = orientation(&mut d, bytes);
    // RGB, gray, and RGBA are shrunk as decoded; the rest are converted
    // to RGBA first, a second full-size buffer.
    let native = matches!(
        d.color_type(),
        ColorType::Rgb8 | ColorType::L8 | ColorType::Rgba8
    );
    let opaque = !d.color_type().has_alpha();
    limits.reserve(d.total_bytes()).map_err(|e| e.to_string())?;
    if !native {
        limits
            .reserve(w as u64 * h as u64 * 4)
            .map_err(|e| e.to_string())?;
    }
    let (ow, oh) = if turns(o) { (h, w) } else { (w, h) };
    let [x, y, cw, ch] = crop.map(u64::from);
    if cw == 0 || ch == 0 || x + cw > ow as u64 || y + ch > oh as u64 {
        return Err("crop outside the image".into());
    }
    let img = DynamicImage::from_decoder(d).map_err(|e| e.to_string())?;
    // Cropped and scaled in the decoder's orientation, then turned: a
    // rotation moves the small result, not the full image.
    let raw = unorient(crop, o, w, h);
    let [tw, th] = if turns(o) { [size[1], size[0]] } else { size };
    // Scaled in the decoded format (a photo is RGB: no alpha channel to
    // add to 12 million pixels first), converted after. With alpha, the
    // average is premultiplied: a transparent pixel adds no color.
    let mut out = match img {
        DynamicImage::ImageRgb8(buf) => DynamicImage::ImageRgb8(shrink(&buf, raw, tw, th)),
        DynamicImage::ImageLuma8(buf) => DynamicImage::ImageLuma8(shrink(&buf, raw, tw, th)),
        other => {
            let mut buf = other.into_rgba8();
            premultiply(&mut buf, raw);
            DynamicImage::ImageRgba8(shrink(&buf, raw, tw, th))
        }
    };
    out.apply_orientation(o);
    let mut rgba = out.into_rgba8().into_raw();
    if !opaque {
        unpremultiply(&mut rgba);
        bleed(&mut rgba, size[0] as usize, size[1] as usize);
    }
    Ok(rgba)
}

/// `crop` of the oriented image as a rect of the decoded one (`w` x
/// `h`): the inverse of `DynamicImage::apply_orientation`.
fn unorient([x, y, cw, ch]: [u32; 4], o: Orientation, w: u32, h: u32) -> [u32; 4] {
    use Orientation::*;
    match o {
        NoTransforms => [x, y, cw, ch],
        Rotate90 => [y, h - x - cw, ch, cw],
        Rotate180 => [w - x - cw, h - y - ch, cw, ch],
        Rotate270 => [w - y - ch, x, ch, cw],
        FlipHorizontal => [w - x - cw, y, cw, ch],
        FlipVertical => [x, h - y - ch, cw, ch],
        Rotate90FlipH => [y, x, ch, cw],
        Rotate270FlipH => [w - y - ch, h - x - cw, ch, cw],
    }
}

/// `crop` of `buf` scaled to `width` x `height`: an area average when
/// shrinking (every source pixel counts: no aliasing), Catmull-Rom in
/// the rare case the crop is smaller than the target on a side.
fn shrink<P: Pixel<Subpixel = u8> + 'static>(
    buf: &ImageBuffer<P, Vec<u8>>,
    [x, y, w, h]: [u32; 4],
    width: u32,
    height: u32,
) -> ImageBuffer<P, Vec<u8>> {
    let view = imageops::crop_imm(buf, x, y, w, h);
    if (w, h) == (width, height) {
        view.to_image()
    } else if width <= w && height <= h {
        imageops::thumbnail(&*view, width, height)
    } else {
        imageops::resize(&*view, width, height, FilterType::CatmullRom)
    }
}

/// Premultiplies `crop` of `buf` in place.
fn premultiply(buf: &mut RgbaImage, [x, y, w, h]: [u32; 4]) {
    let stride = buf.width() as usize * 4;
    let (x, w) = (x as usize * 4, w as usize * 4);
    for row in buf
        .chunks_exact_mut(stride)
        .skip(y as usize)
        .take(h as usize)
    {
        for p in row[x..x + w].chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a < 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
            }
        }
    }
}

/// Back to straight alpha.
fn unpremultiply(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

/// Gives each fully transparent pixel the average color of its visible
/// neighbours. The GPU filters straight alpha, so a transparent pixel's
/// color blends into the edge beside it: black would draw a dark rim.
fn bleed(rgba: &mut [u8], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if rgba[i + 3] != 0 {
                continue;
            }
            let (mut sum, mut n) = ([0u32; 3], 0);
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let j = (ny * w + nx) * 4;
                    if rgba[j + 3] != 0 {
                        for c in 0..3 {
                            sum[c] += rgba[j + c] as u32;
                        }
                        n += 1;
                    }
                }
            }
            if n > 0 {
                for c in 0..3 {
                    rgba[i + c] = (sum[c] / n) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craie_ui::mutation::{NIL, NodeKind, Transaction};
    use image::codecs::webp::WebPEncoder;
    use image::{ExtendedColorType, GrayAlphaImage, GrayImage, ImageFormat, Luma, LumaA, Rgba};
    use std::sync::mpsc::{Receiver, channel};

    fn test_png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> Rgba<u8>) -> Vec<u8> {
        let buf = ImageBuffer::from_fn(width, height, pixel);
        crate::capture::encode_png(width, height, buf.as_raw())
    }

    fn decode_req(bytes: &[u8], crop: [u32; 4], width: u32, height: u32) -> ImageResult {
        run(&ImageRequest::Decode {
            id: ImageId(1),
            bytes: bytes.into(),
            crop,
            width,
            height,
        })
    }

    /// Left half red, right half blue: a crop picks a side, a shrink
    /// averages within it.
    #[test]
    fn probes_crops_and_shrinks() {
        let png = test_png(40, 20, |x, _| {
            if x < 20 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 128])
            }
        });
        let r = run(&ImageRequest::Probe {
            id: ImageId(1),
            bytes: png.as_slice().into(),
        });
        assert!(matches!(
            r,
            ImageResult::Size {
                width: 40,
                height: 20,
                ..
            }
        ));
        let ImageResult::Pixels { rgba, .. } = decode_req(&png, [20, 0, 20, 20], 5, 5) else {
            panic!()
        };
        assert_eq!(rgba.len(), 5 * 5 * 4);
        assert!(rgba.chunks(4).all(|p| p == [0, 0, 255, 128]));
        let ImageResult::Pixels { rgba, .. } = decode_req(&png, [0, 0, 40, 20], 2, 1) else {
            panic!()
        };
        assert_eq!(rgba, [255, 0, 0, 255, 0, 0, 255, 128]);
    }

    #[test]
    fn bad_bytes_and_crops_fail() {
        let r = run(&ImageRequest::Probe {
            id: ImageId(1),
            bytes: (&b"not an image"[..]).into(),
        });
        assert!(matches!(r, ImageResult::Failed { .. }), "{r:?}");
        let png = test_png(4, 4, |_, _| Rgba([0; 4]));
        let r = decode_req(&png, [2, 2, 4, 4], 2, 2);
        assert!(matches!(r, ImageResult::Failed { .. }), "{r:?}");
    }

    /// Requests go out through `pump`; results come back through it.
    #[test]
    fn the_worker_answers_through_pump() {
        let (mut decoder, rx) = decoder(run);
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        let png = test_png(3, 2, |_, _| Rgba([9, 9, 9, 255]));
        t.create(1, NodeKind::Image)
            .payload(1, png.as_slice())
            .place(NIL, 1, NIL);
        ui.apply_txn(&t).unwrap();
        assert!(!decoder.pump(&mut ui));
        rx.recv().unwrap();
        assert!(decoder.pump(&mut ui));
        ui.render(craie_core::Size::new(10.0, 10.0));
        decoder.pump(&mut ui);
        rx.recv().unwrap();
        assert!(decoder.pump(&mut ui));
        let ev = ui.take_events();
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].kind, ev[0].key, ev[0].x, ev[0].y), (18, 0, 3.0, 2.0));
        assert_eq!(ui.image_bytes(), 3 * 2 * 4);
    }

    fn decoder(run: Run) -> (Decoder, Receiver<()>) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let wake = move || {
            let _ = tx.lock().unwrap().send(());
        };
        (Decoder::with(run, Arc::new(wake)), rx)
    }

    fn probe_req(bytes: &[u8]) -> ImageResult {
        run(&ImageRequest::Probe {
            id: ImageId(1),
            bytes: bytes.into(),
        })
    }

    fn error(r: ImageResult) -> String {
        match r {
            ImageResult::Failed { error, .. } => error,
            r => panic!("not a failure: {r:?}"),
        }
    }

    fn pixels(r: ImageResult) -> Vec<u8> {
        match r {
            ImageResult::Pixels { rgba, .. } => rgba,
            r => panic!("no pixels: {r:?}"),
        }
    }

    fn crc(bytes: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in bytes {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    /// A PNG chunk: length, kind, data, CRC.
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        c.extend(kind);
        c.extend(data);
        c.extend(crc(&c[4..]).to_be_bytes());
        c
    }

    /// `png` with an `eXIf` chunk before its pixels: a big-endian TIFF
    /// with one tag, Orientation (0x0112) = `o`.
    fn with_exif(png: &[u8], o: u16) -> Vec<u8> {
        let mut tiff = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        tiff.extend(o.to_be_bytes());
        tiff.extend([0; 6]);
        let at = 8 + 25; // the signature, then IHDR
        [&png[..at], &chunk(b"eXIf", &tiff), &png[at..]].concat()
    }

    /// A 4 x 4 PNG whose header claims 20,000 x 20,000 (400 MP; 1.6 GB
    /// as RGBA).
    #[test]
    fn a_decompression_bomb_fails_before_decoding() {
        let mut png = test_png(4, 4, |_, _| Rgba([1, 2, 3, 255]));
        png[16..24].copy_from_slice(&[0, 0, 0x4E, 0x20, 0, 0, 0x4E, 0x20]);
        let sum = crc(&png[12..29]);
        png[29..33].copy_from_slice(&sum.to_be_bytes());
        let over = "20000 x 20000 is over the 64 MP limit";
        assert_eq!(error(probe_req(&png)), over);
        assert_eq!(error(decode_req(&png, [0, 0, 100, 100], 10, 10)), over);
        // Within the pixel budget, the buffers must fit the allocation
        // one: the decoded RGBA, 40 KB ...
        let png = test_png(100, 100, |_, _| Rgba([1, 2, 3, 255]));
        let full = [0, 0, 100, 100];
        assert!(decode(&png, full, [10, 10], 39_999).is_err());
        assert!(decode(&png, full, [10, 10], 40_000).is_ok());
        // ... and for a format the decoder converts, the RGBA copy too:
        // gray with alpha, 20 KB, then 40 KB.
        let mut gray = Cursor::new(Vec::new());
        GrayAlphaImage::from_pixel(100, 100, LumaA([7, 255]))
            .write_to(&mut gray, ImageFormat::Png)
            .unwrap();
        let gray = gray.into_inner();
        assert!(decode(&gray, full, [10, 10], 59_999).is_err());
        let rgba = decode(&gray, full, [10, 10], 60_000).unwrap();
        assert_eq!(&rgba[..4], [7, 7, 7, 255]);
        // Plain gray is shrunk as decoded: 10 KB is enough.
        let mut gray = Cursor::new(Vec::new());
        GrayImage::from_pixel(100, 100, Luma([7]))
            .write_to(&mut gray, ImageFormat::Png)
            .unwrap();
        let rgba = decode(gray.get_ref(), full, [10, 10], 10_000).unwrap();
        assert_eq!(&rgba[..4], [7, 7, 7, 255]);
        // A 1 x 1 WebP whose EXIF chunk claims 4 GiB: its orientation is
        // not read (image-webp would allocate the claim).
        let webp = webp_bomb();
        assert!(!webp_chunks_fit(&webp));
        assert!(webp_chunks_fit(&webp[..webp.len() - 8]));
        assert!(matches!(
            probe_req(&webp),
            ImageResult::Size {
                width: 1,
                height: 1,
                ..
            }
        ));
        let full = [0, 0, 1, 1];
        assert_eq!(
            decode(&webp, full, [1, 1], MAX_ALLOC).unwrap(),
            [9, 8, 7, 255]
        );
    }

    /// RIFF, VP8X with the EXIF flag, a 1 x 1 VP8L, and an EXIF chunk
    /// header claiming 0xFFFF_FFF0 bytes, none of them there.
    fn webp_bomb() -> Vec<u8> {
        let mut lossless = Vec::new();
        WebPEncoder::new_lossless(&mut lossless)
            .encode(&[9, 8, 7, 255], 1, 1, ExtendedColorType::Rgba8)
            .unwrap();
        let vp8x = [
            b"VP8X".as_slice(),
            &10u32.to_le_bytes(),
            &[0x08, 0, 0, 0],
            &[0; 6],
        ];
        let exif = [b"EXIF".as_slice(), &0xFFFF_FFF0u32.to_le_bytes()];
        let body = [&vp8x[..], &[&lossless[12..]], &exif[..]].concat().concat();
        let size = (body.len() as u32 + 4).to_le_bytes();
        [b"RIFF".as_slice(), &size, b"WEBP", &body].concat()
    }

    /// The probe reads the header only: a file cut short fails at the
    /// decode, as an image event rather than a crash.
    #[test]
    fn a_truncated_file_fails_its_decode() {
        let png = test_png(64, 64, |x, y| {
            Rgba([(x * 37 ^ y * 91) as u8, x as u8, y as u8, 255])
        });
        let cut = &png[..png.len() / 2];
        assert!(matches!(
            probe_req(cut),
            ImageResult::Size { width: 64, .. }
        ));
        error(decode_req(cut, [0, 0, 64, 64], 64, 64));
    }

    /// Each EXIF orientation, against the image crate's own: the natural
    /// size is turned, and a crop of the turned image (at 1:1 and
    /// shrunk) matches the turned image's crop.
    #[test]
    fn exif_orientations_turn_the_size_and_the_pixels() {
        let raw = ImageBuffer::from_fn(6, 4, |x, y| Rgba([x as u8 * 40, y as u8 * 60, 9, 255]));
        let png = crate::capture::encode_png(6, 4, raw.as_raw());
        for o in 1..=8u16 {
            let bytes = with_exif(&png, o);
            let mut want = DynamicImage::ImageRgba8(raw.clone());
            want.apply_orientation(Orientation::from_exif(o as u8).unwrap());
            let (w, h) = (want.width(), want.height());
            let ImageResult::Size { width, height, .. } = probe_req(&bytes) else {
                panic!("orientation {o}")
            };
            assert_eq!([width, height], [w, h], "orientation {o}");
            let crop = [1, 1, w - 2, h - 1];
            let got = pixels(decode_req(&bytes, crop, w - 2, h - 1));
            let cut = imageops::crop_imm(want.as_rgba8().unwrap(), 1, 1, w - 2, h - 1);
            assert_eq!(got, cut.to_image().into_raw(), "orientation {o}");
            let small = pixels(decode_req(&bytes, [0, 0, w, h], w / 2, h / 2));
            let half = imageops::thumbnail(want.as_rgba8().unwrap(), w / 2, h / 2);
            assert_eq!(small, half.into_raw(), "orientation {o} shrunk");
        }
        // An orientation out of range draws the image as stored.
        let bytes = with_exif(&png, 9);
        let got = pixels(decode_req(&bytes, [0, 0, 6, 4], 6, 4));
        assert_eq!(got, raw.into_raw());
    }

    /// Red beside transparent black. A straight average makes the edge
    /// dark red; a premultiplied one keeps it red, half transparent.
    /// The transparent pixel beyond takes the edge's color, so the GPU's
    /// filtering between them stays red too.
    #[test]
    fn transparency_leaves_no_dark_rim() {
        let png = test_png(4, 1, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0; 4])
            }
        });
        let rgba = pixels(decode_req(&png, [0, 0, 4, 1], 2, 1));
        assert_eq!(rgba[..3], [255, 0, 0]);
        assert!((127..=128).contains(&rgba[3]), "{rgba:?}");
        assert_eq!(rgba[4..], [255, 0, 0, 0]);
    }

    /// Probes go first; a newer decode of an image replaces its queued
    /// one; a dropped image's work goes.
    #[test]
    fn the_queue_orders_and_prunes() {
        let bytes: Arc<[u8]> = Arc::from(&b"x"[..]);
        let decode = |id, width| ImageRequest::Decode {
            id: ImageId(id),
            bytes: bytes.clone(),
            crop: [0; 4],
            width,
            height: 1,
        };
        let probe = |id| ImageRequest::Probe {
            id: ImageId(id),
            bytes: bytes.clone(),
        };
        let mut q = Queue::default();
        q.add(&[], vec![decode(1, 10), decode(2, 10), probe(3)]);
        q.add(&[ImageId(2)], vec![decode(1, 20), probe(4)]);
        let order: Vec<_> = std::iter::from_fn(|| q.next())
            .map(|r| match r {
                ImageRequest::Probe { id, .. } => (id.0, 0),
                ImageRequest::Decode { id, width, .. } => (id.0, width),
            })
            .collect();
        assert_eq!(order, [(3, 0), (4, 0), (1, 20)]);
    }

    fn image(ui: &mut Ui, node: u32, bytes: &[u8]) {
        let mut t = Transaction::new(1);
        t.create(node, NodeKind::Image)
            .payload(node, bytes)
            .place(NIL, node, NIL);
        ui.apply_txn(&t).unwrap();
    }

    /// Renders and pumps until `ui` has `n` image events.
    fn events(decoder: &mut Decoder, rx: &Receiver<()>, ui: &mut Ui, n: usize) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        decoder.pump(ui);
        while out.len() < n {
            rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            decoder.pump(ui);
            ui.render(craie_core::Size::new(10.0, 10.0));
            decoder.pump(ui);
            out.extend(ui.take_events().into_iter().map(|e| (e.node, e.key)));
        }
        out.sort();
        out
    }

    fn boom(req: &ImageRequest) -> ImageResult {
        match req {
            ImageRequest::Probe { bytes, .. } if &**bytes == b"boom" => panic!("a codec bug"),
            req => run(req),
        }
    }

    #[test]
    fn a_panicking_codec_fails_its_image_only() {
        let (mut decoder, rx) = decoder(boom);
        let mut ui = Ui::new(1.0);
        image(&mut ui, 1, b"boom");
        image(&mut ui, 2, &test_png(2, 2, |_, _| Rgba([1; 4])));
        assert_eq!(events(&mut decoder, &rx, &mut ui, 2), [(1, 1), (2, 0)]);
    }

    /// A panic whose payload panics as it drops escapes `catch_unwind`:
    /// the worker dies, its image fails, and a new worker takes the rest.
    struct Nested;

    impl Drop for Nested {
        fn drop(&mut self) {
            panic!("and again");
        }
    }

    fn kill(req: &ImageRequest) -> ImageResult {
        match req {
            ImageRequest::Probe { bytes, .. } if &**bytes == b"kill" => {
                std::panic::panic_any(Nested)
            }
            req => run(req),
        }
    }

    #[test]
    fn a_dead_worker_is_replaced() {
        let (mut decoder, rx) = decoder(kill);
        let mut ui = Ui::new(1.0);
        image(&mut ui, 1, b"kill");
        assert_eq!(events(&mut decoder, &rx, &mut ui, 1), [(1, 1)]);
        image(&mut ui, 2, &test_png(2, 2, |_, _| Rgba([1; 4])));
        assert_eq!(events(&mut decoder, &rx, &mut ui, 1), [(2, 0)]);
    }
}

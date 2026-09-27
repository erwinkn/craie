//! Image decoding for `Ui` image nodes, on one worker thread.
//!
//! The core queues requests (`Ui::take_image_requests`): a probe reads
//! a header for the natural size, a decode reads the pixels of a source
//! rect scaled down to the size the node shows them at. The worker
//! answers each with an `ImageResult` and wakes the event loop, which
//! feeds it back (`Decoder::pump`). PNG, JPEG, WebP, and GIF (its first
//! frame), with the EXIF orientation applied.

use std::io::Cursor;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use craie_ui::image::{ImageRequest, ImageResult};
use craie_ui::ui::Ui;
use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageBuffer, ImageDecoder, ImageReader, Pixel};

/// The worker and its answers.
pub struct Decoder {
    tx: Sender<ImageRequest>,
    results: Arc<Mutex<Vec<ImageResult>>>,
}

impl Decoder {
    /// Starts the worker. `wake` runs after each result, off the UI
    /// thread. The worker ends with the decoder.
    pub fn new(wake: impl Fn() + Send + 'static) -> Decoder {
        let (tx, rx) = channel::<ImageRequest>();
        let results = Arc::new(Mutex::new(Vec::new()));
        let out = results.clone();
        std::thread::Builder::new()
            .name("craie-images".into())
            .spawn(move || {
                for req in rx {
                    let r = run(&req);
                    out.lock().unwrap().push(r);
                    wake();
                }
            })
            .expect("spawn the image decoder");
        Decoder { tx, results }
    }

    /// Feeds finished results to `ui` and sends it new requests.
    /// Returns whether a result arrived (the UI may owe a paint).
    pub fn pump(&self, ui: &mut Ui) -> bool {
        let results = std::mem::take(&mut *self.results.lock().unwrap());
        let any = !results.is_empty();
        for r in results {
            ui.image_result(r);
        }
        for req in ui.take_image_requests() {
            let _ = self.tx.send(req);
        }
        any
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
        } => match decode(bytes, *crop, *width, *height) {
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

fn reader(bytes: &[u8]) -> Result<impl ImageDecoder + '_, String> {
    // The default limits (512 MiB of allocation) bound a hostile header.
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())
}

/// The natural size: the header's, turned by the EXIF orientation.
fn probe(bytes: &[u8]) -> Result<[u32; 2], String> {
    let mut d = reader(bytes)?;
    let (w, h) = d.dimensions();
    let turned = d.orientation().is_ok_and(|o| {
        use image::metadata::Orientation::*;
        matches!(o, Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH)
    });
    Ok(if turned { [h, w] } else { [w, h] })
}

/// `crop` of the oriented image, scaled to `width` x `height`: RGBA,
/// straight alpha, sRGB.
fn decode(bytes: &[u8], crop: [u32; 4], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut d = reader(bytes)?;
    let orientation = d.orientation().map_err(|e| e.to_string())?;
    let mut img = DynamicImage::from_decoder(d).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let [x, y, w, h] = crop;
    if w == 0 || h == 0 || x + w > img.width() || y + h > img.height() {
        return Err("crop outside the image".into());
    }
    // Scaled in the decoded format (a photo is RGB: no alpha channel to
    // add to 12 million pixels first), converted after.
    Ok(match img {
        DynamicImage::ImageRgb8(buf) => {
            DynamicImage::ImageRgb8(shrink(&buf, crop, width, height)).into_rgba8()
        }
        DynamicImage::ImageRgba8(buf) => shrink(&buf, crop, width, height),
        other => shrink(&other.into_rgba8(), crop, width, height),
    }
    .into_raw())
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

#[cfg(test)]
mod tests {
    use super::*;
    use craie_ui::image::ImageId;
    use image::Rgba;

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
        use craie_ui::mutation::{NIL, NodeKind, Transaction};
        let (tx, rx) = channel();
        let decoder = Decoder::new(move || {
            let _ = tx.send(());
        });
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
}

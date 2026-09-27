//! Image nodes: the core side of decoding and residency.
//!
//! JS fetches an image and sends its encoded bytes once, as the payload
//! of an Image node. The platform decodes them off the UI thread; the
//! core owns the rest: each payload's `ImageId`, what to decode (the
//! source rect and the pixel size the node shows it at), and the pixels'
//! residency in the scene's color atlas.
//!
//! ```text
//! payload --> Probe (header: natural size) --> layout (intrinsic size)
//! frame   --> Decode (crop, size in device px) --> pixels --> atlas
//! ```
//!
//! A 4,000 px photo in a 40 pt box at 2x decodes to 80 px, so its
//! texture is 25 KB, not 64 MB. The pixels are drawn as a color raster
//! (the glyph path): one textured quad per node.

use std::collections::HashMap;
use std::sync::Arc;

use craie_core::geom::Rect;
use craie_scene::{ChunkWriter, RasterAtlas, RasterId};

use crate::events::out_kind;
use crate::host::NodeId;
use crate::layout::LayoutData;
use crate::ui::Ui;

/// One payload of one image node. A new payload gets a new id, so a
/// result for replaced bytes is recognized and dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ImageId(pub u64);

/// How an image fills its node's content box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Fit {
    /// Fills the box, aspect kept, cropped to it (centered).
    #[default]
    Cover = 0,
    /// Fits inside the box, aspect kept, centered.
    Contain = 1,
    /// Stretched to the box.
    Fill = 2,
}

impl Fit {
    pub fn from_u8(v: u8) -> Option<Fit> {
        Some(match v {
            0 => Fit::Cover,
            1 => Fit::Contain,
            2 => Fit::Fill,
            _ => return None,
        })
    }
}

/// Work for the platform's decoder.
#[derive(Clone, Debug)]
pub enum ImageRequest {
    /// The natural size in pixels, from the header, after the EXIF
    /// orientation.
    Probe { id: ImageId, bytes: Arc<[u8]> },
    /// The pixels of `crop` (x, y, width, height in natural pixels),
    /// scaled down to `width` x `height`.
    Decode {
        id: ImageId,
        bytes: Arc<[u8]>,
        crop: [u32; 4],
        width: u32,
        height: u32,
    },
}

impl ImageRequest {
    pub fn id(&self) -> ImageId {
        match self {
            ImageRequest::Probe { id, .. } | ImageRequest::Decode { id, .. } => *id,
        }
    }
}

/// The decoder's answer to a request.
#[derive(Clone, Debug)]
pub enum ImageResult {
    Size {
        id: ImageId,
        width: u32,
        height: u32,
    },
    /// `width` x `height` RGBA, sRGB, straight alpha, of `crop`.
    Pixels {
        id: ImageId,
        crop: [u32; 4],
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// The bytes are not an image the decoder reads.
    Failed { id: ImageId, error: String },
}

impl ImageResult {
    pub fn id(&self) -> ImageId {
        match self {
            ImageResult::Size { id, .. }
            | ImageResult::Pixels { id, .. }
            | ImageResult::Failed { id, .. } => *id,
        }
    }
}

/// The `key` of an `out_kind::IMAGE` event.
pub mod image_event {
    /// Decoded and drawn: x/y = the natural size.
    pub const LOADED: u32 = 0;
    /// Not decodable: `text` = why.
    pub const FAILED: u32 = 1;
}

/// What a node shows of its image: the source rect, the drawn rect
/// (content-box coordinates, logical) and its size in device pixels,
/// and the pixel size to decode to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub crop: [u32; 4],
    pub dest: Rect,
    pub quad: [u16; 2],
    pub size: [u32; 2],
}

/// Plans `natural` (pixels) fitted into `content` at display `scale`.
/// The decode size is the drawn size in device pixels, never above the
/// source (no upscaling: the quad scales up instead) and at most `max`
/// on either side (a page of the atlas). `None`: nothing to draw.
pub fn plan(fit: Fit, natural: [u32; 2], content: Rect, scale: f32, max: u32) -> Option<Plan> {
    let [nw, nh] = natural.map(|v| v as f32);
    let (bw, bh) = (content.size.width, content.size.height);
    if !(bw > 0.0 && bh > 0.0 && nw > 0.0 && nh > 0.0) {
        return None;
    }
    let full = [0, 0, natural[0], natural[1]];
    let (crop, dest) = match fit {
        Fit::Fill => (full, content),
        Fit::Contain => {
            let k = (bw / nw).min(bh / nh);
            let (w, h) = (nw * k, nh * k);
            let x = content.origin.x + (bw - w) / 2.0;
            let y = content.origin.y + (bh - h) / 2.0;
            (full, Rect::new(x, y, w, h))
        }
        Fit::Cover => {
            // The largest centered source rect with the box's aspect.
            let k = (nw / bw).min(nh / bh);
            let cw = ((bw * k).round() as u32).clamp(1, natural[0]);
            let ch = ((bh * k).round() as u32).clamp(1, natural[1]);
            (
                [(natural[0] - cw) / 2, (natural[1] - ch) / 2, cw, ch],
                content,
            )
        }
    };
    let px = |v: f32| (v * scale).round().max(1.0);
    let (qw, qh) = (px(dest.size.width), px(dest.size.height));
    let mut size = [(qw as u32).min(crop[2]), (qh as u32).min(crop[3])];
    let over = size[0].max(size[1]);
    if over > max {
        size = size.map(|v| ((v as u64 * max as u64 / over as u64) as u32).max(1));
    }
    Some(Plan {
        crop,
        dest,
        quad: [
            qw.min(u16::MAX as f32) as u16,
            qh.min(u16::MAX as f32) as u16,
        ],
        size,
    })
}

/// Decoded pixels of one node and their raster.
struct Bitmap {
    crop: [u32; 4],
    size: [u32; 2],
    rgba: Vec<u8>,
    raster: RasterId,
}

impl Bitmap {
    /// Whether `p` wants other pixels: another source rect, a larger
    /// size, or less than half the size on a side (hysteresis: a box
    /// that shrinks a little keeps its pixels, drawn scaled down).
    fn stale(&self, p: &Plan) -> bool {
        let [w, h] = self.size;
        self.crop != p.crop
            || p.size[0] > w
            || p.size[1] > h
            || p.size[0] * 2 < w
            || p.size[1] * 2 < h
    }
}

/// Decode state of one image node.
struct State {
    id: ImageId,
    /// A request for this image is with the platform: at most one at a
    /// time, so a resizing box does not queue a decode per frame.
    pending: bool,
    failed: bool,
    /// The load event went out.
    loaded: bool,
    bitmap: Option<Bitmap>,
}

/// Image nodes' decode state and the platform's work queue.
#[derive(Default)]
pub(crate) struct Images {
    next: u64,
    /// Each live image's node.
    nodes: HashMap<ImageId, u32>,
    /// By node id.
    states: HashMap<u32, State>,
    requests: Vec<ImageRequest>,
}

impl Images {
    /// Drops a node's image: its raster, queued requests, and the
    /// mapping that routes results to it (late results are dropped).
    pub(crate) fn forget(&mut self, node: u32, atlas: &mut RasterAtlas) {
        let Some(st) = self.states.remove(&node) else {
            return;
        };
        self.nodes.remove(&st.id);
        self.requests.retain(|r| r.id() != st.id);
        if let Some(b) = st.bitmap {
            atlas.release(b.raster);
        }
    }

    /// Re-inserts the pixels of evicted image rasters that a visible
    /// chunk draws (`missing` sorted by id).
    pub(crate) fn ensure_resident(&self, missing: &[RasterId], atlas: &mut RasterAtlas) {
        for b in self.states.values().filter_map(|s| s.bitmap.as_ref()) {
            if missing.binary_search_by_key(&b.raster.0, |r| r.0).is_ok() {
                atlas.insert(b.raster, &b.rgba);
            }
        }
    }

    /// Texture bytes held by image rasters (CPU copies; the atlas holds
    /// as many while resident).
    pub(crate) fn bytes(&self) -> usize {
        self.states
            .values()
            .filter_map(|s| s.bitmap.as_ref())
            .map(|b| b.rgba.len())
            .sum()
    }
}

impl Ui {
    /// Decode work queued for the platform since the last call.
    pub fn take_image_requests(&mut self) -> Vec<ImageRequest> {
        std::mem::take(&mut self.images.requests)
    }

    /// Bytes of decoded image pixels held (one copy; the atlas holds
    /// another while they are resident).
    pub fn image_bytes(&self) -> usize {
        self.images.bytes()
    }

    /// A decoder result. Results for an image replaced or removed since
    /// the request are dropped.
    pub fn image_result(&mut self, result: ImageResult) {
        let Some(&node) = self.images.nodes.get(&result.id()) else {
            return;
        };
        let Some(st) = self.images.states.get_mut(&node) else {
            return;
        };
        let id = NodeId(node);
        match result {
            ImageResult::Size { width, height, .. } => {
                st.pending = false;
                if width == 0 || height == 0 {
                    st.failed = true;
                    self.image_event(id, image_event::FAILED, "empty image");
                    return;
                }
                if let Some(d) = self.host.images.get_mut(&node) {
                    d.natural = Some([width, height]);
                }
                // The natural size is the intrinsic size.
                self.host.revs.layout_input.bump();
                self.host.mark_layout(id);
                self.host.dirty.content.push(node);
            }
            ImageResult::Pixels {
                crop,
                width,
                height,
                rgba,
                ..
            } => {
                st.pending = false;
                let max = self.scene.atlas.page_size() - 2;
                if width == 0 || height == 0 || width > max || height > max {
                    return;
                }
                if rgba.len() != width as usize * height as usize * 4 {
                    return;
                }
                if let Some(old) = st.bitmap.take() {
                    self.scene.atlas.release(old.raster);
                }
                let (w, h) = (width as u16, height as u16);
                let raster = self.scene.atlas.new_scaled_id(w, h, w, h, true);
                self.scene.atlas.insert(raster, &rgba);
                st.bitmap = Some(Bitmap {
                    crop,
                    size: [width, height],
                    rgba,
                    raster,
                });
                // The chunk names the old raster: it rebuilds before the
                // next draw.
                self.host.dirty.content.push(node);
                if !std::mem::replace(&mut st.loaded, true) {
                    let natural = self.host.images.get(&node).and_then(|d| d.natural);
                    let [nw, nh] = natural.unwrap_or([width, height]);
                    let mut e = self.event(out_kind::IMAGE, id);
                    e.key = image_event::LOADED;
                    e.x = nw as f32;
                    e.y = nh as f32;
                    self.pending_events.push(e);
                }
            }
            ImageResult::Failed { error, .. } => {
                st.pending = false;
                st.failed = true;
                self.image_event(id, image_event::FAILED, &error);
            }
        }
    }

    fn image_event(&mut self, id: NodeId, key: u32, text: &str) {
        let mut e = self.event(out_kind::IMAGE, id);
        e.key = key;
        e.text = text.to_string();
        self.pending_events.push(e);
    }

    /// A PAYLOAD on an Image node: new bytes, a new `ImageId`, and a
    /// probe for the natural size. Empty bytes clear the image.
    pub(crate) fn set_image(&mut self, node: u32, bytes: &[u8]) {
        let Some(d) = self.host.images.get(&node) else {
            return;
        };
        if d.bytes[..] == *bytes {
            return;
        }
        self.images.forget(node, &mut self.scene.atlas);
        self.images.next += 1;
        let id = ImageId(self.images.next);
        let bytes: Arc<[u8]> = bytes.into();
        self.host.copied_bytes += bytes.len() as u64;
        if let Some(d) = self.host.images.get_mut(&node) {
            d.bytes = bytes.clone();
            d.natural = None;
        }
        if !bytes.is_empty() {
            self.images.nodes.insert(id, node);
            self.images.states.insert(
                node,
                State {
                    id,
                    pending: true,
                    failed: false,
                    loaded: false,
                    bitmap: None,
                },
            );
            self.images.requests.push(ImageRequest::Probe { id, bytes });
        }
        self.host.revs.resource.bump();
        self.host.revs.layout_input.bump();
        self.host.mark_layout(NodeId(node));
        self.host.dirty.content.push(node);
    }

    pub(crate) fn set_image_fit(&mut self, node: u32, fit: Fit) {
        if let Some(d) = self.host.images.get_mut(&node)
            && d.fit != fit
        {
            d.fit = fit;
            self.host.revs.resource.bump();
            self.host.dirty.content.push(node);
        }
    }

    /// Image chunk: the pixels as one quad, fitted to the content box.
    /// Asks for a decode when the drawn size or source rect outgrew the
    /// pixels; until they arrive, the old pixels draw scaled.
    pub(crate) fn build_image(&mut self, id: NodeId, data: &LayoutData, w: &mut ChunkWriter) {
        let Some(d) = self.host.images.get(&id.0) else {
            return;
        };
        let (Some(natural), Some(st)) = (d.natural, self.images.states.get_mut(&id.0)) else {
            return;
        };
        if st.failed {
            return;
        }
        let content = Rect::new(
            data.content[0],
            data.content[1],
            (data.rect.size.width - data.insets[0]).max(0.0),
            (data.rect.size.height - data.insets[1]).max(0.0),
        );
        let max = self.scene.atlas.page_size() - 2;
        let Some(p) = plan(d.fit, natural, content, self.scale, max) else {
            return;
        };
        if !st.pending && st.bitmap.as_ref().is_none_or(|b| b.stale(&p)) {
            st.pending = true;
            self.images.requests.push(ImageRequest::Decode {
                id: st.id,
                bytes: d.bytes.clone(),
                crop: p.crop,
                width: p.size[0],
                height: p.size[1],
            });
        }
        if let Some(b) = &st.bitmap {
            self.scene.atlas.set_quad(b.raster, p.quad[0], p.quad[1]);
            let paint = w.paint(0xFFFF_FFFF);
            let r = p.dest;
            w.glyph(
                r.origin.x,
                r.origin.y,
                r.size.width,
                r.size.height,
                b.raster,
                paint,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::UiEvent;
    use crate::mutation::{NIL, NodeKind, Transaction};
    use crate::ui::Ui;
    use craie_core::geom::Size;

    const MAX: u32 = 2046;

    fn plan_of(fit: Fit, natural: [u32; 2], w: f32, h: f32, scale: f32) -> Plan {
        plan(fit, natural, Rect::new(0.0, 0.0, w, h), scale, MAX).unwrap()
    }

    /// Cover crops the centered source rect with the box's aspect;
    /// contain letterboxes; fill stretches. Each decodes at the drawn
    /// device size.
    #[test]
    fn fits_plan_crop_and_size() {
        // A 4,000 x 3,000 photo in a 40 pt square at 2x.
        let p = plan_of(Fit::Cover, [4000, 3000], 40.0, 40.0, 2.0);
        assert_eq!(p.crop, [500, 0, 3000, 3000]);
        assert_eq!(p.size, [80, 80]);
        assert_eq!(p.quad, [80, 80]);
        assert_eq!(p.dest, Rect::new(0.0, 0.0, 40.0, 40.0));

        let p = plan_of(Fit::Contain, [4000, 3000], 40.0, 40.0, 2.0);
        assert_eq!(p.crop, [0, 0, 4000, 3000]);
        assert_eq!(p.dest, Rect::new(0.0, 5.0, 40.0, 30.0));
        assert_eq!(p.size, [80, 60]);

        let p = plan_of(Fit::Fill, [4000, 3000], 40.0, 20.0, 2.0);
        assert_eq!(p.crop, [0, 0, 4000, 3000]);
        assert_eq!(p.size, [80, 40]);
    }

    /// Never upscaled: a small image in a large box decodes at its own
    /// size and its quad scales up. A huge box is capped at a page.
    #[test]
    fn plans_never_upscale_and_fit_a_page() {
        let p = plan_of(Fit::Fill, [16, 8], 100.0, 100.0, 2.0);
        assert_eq!(p.size, [16, 8]);
        assert_eq!(p.quad, [200, 200]);
        let p = plan_of(Fit::Contain, [8000, 4000], 3000.0, 1500.0, 2.0);
        assert_eq!(p.size, [MAX, MAX / 2]);
        assert!(
            plan(
                Fit::Cover,
                [10, 10],
                Rect::new(0.0, 0.0, 0.0, 5.0),
                1.0,
                MAX
            )
            .is_none()
        );
    }

    fn image_ui(fit: Fit) -> Ui {
        let mut ui = Ui::new(2.0);
        let mut t = Transaction::new(1);
        t.create(1, NodeKind::Image)
            .layout(
                1,
                &taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(40.0),
                        height: taffy::Dimension::length(40.0),
                    },
                    ..Default::default()
                },
            )
            .payload(1, &b"encoded"[..])
            .image_config(1, fit)
            .place(NIL, 1, NIL);
        ui.apply_txn(&t).unwrap();
        ui
    }

    fn events(ui: &mut Ui) -> Vec<UiEvent> {
        ui.take_events()
            .into_iter()
            .filter(|e| e.kind == out_kind::IMAGE)
            .collect()
    }

    /// Payload -> probe -> layout -> decode at the drawn size -> a
    /// resident raster and one load event.
    #[test]
    fn a_payload_probes_then_decodes_at_the_drawn_size() {
        let mut ui = image_ui(Fit::Cover);
        let reqs = ui.take_image_requests();
        let [ImageRequest::Probe { id, bytes }] = &reqs[..] else {
            panic!("{reqs:?}")
        };
        assert_eq!(&bytes[..], b"encoded");
        let id = *id;
        ui.render(Size::new(100.0, 100.0));
        assert!(ui.take_image_requests().is_empty(), "no size yet");
        ui.image_result(ImageResult::Size {
            id,
            width: 4000,
            height: 3000,
        });
        assert!(ui.needs_paint());
        ui.render(Size::new(100.0, 100.0));
        let reqs = ui.take_image_requests();
        let [
            ImageRequest::Decode {
                crop,
                width,
                height,
                ..
            },
        ] = &reqs[..]
        else {
            panic!("{reqs:?}")
        };
        assert_eq!((*crop, *width, *height), ([500, 0, 3000, 3000], 80, 80));
        // Nothing asked twice while the decode is out.
        ui.render(Size::new(100.0, 100.0));
        assert!(ui.take_image_requests().is_empty());
        ui.image_result(ImageResult::Pixels {
            id,
            crop: *crop,
            width: 80,
            height: 80,
            rgba: vec![200; 80 * 80 * 4],
        });
        ui.render(Size::new(100.0, 100.0));
        assert!(ui.take_image_requests().is_empty(), "the pixels fit");
        assert_eq!(ui.image_bytes(), 80 * 80 * 4);
        let ev = events(&mut ui);
        assert_eq!(ev.len(), 1);
        assert_eq!(
            (ev[0].key, ev[0].x, ev[0].y),
            (image_event::LOADED, 4000.0, 3000.0)
        );
        let raster = ui.images.states[&1].bitmap.as_ref().unwrap().raster;
        let e = ui.scene.atlas.entry(raster);
        assert!(e.resident && e.color);
        assert_eq!((e.quad_w, e.quad_h), (80, 80));
    }

    /// The intrinsic size is the natural size, a pixel per point.
    #[test]
    fn an_unsized_image_takes_its_natural_size() {
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        t.create(1, NodeKind::Image)
            .payload(1, &b"x"[..])
            .place(NIL, 1, NIL);
        ui.apply_txn(&t).unwrap();
        let id = ui.take_image_requests()[0].id();
        ui.image_result(ImageResult::Size {
            id,
            width: 30,
            height: 20,
        });
        ui.render(Size::new(100.0, 100.0));
        assert_eq!(ui.layouts.data(NodeId(1)).rect.size, Size::new(30.0, 20.0));
    }

    /// A failure reports once; results for replaced bytes are dropped.
    #[test]
    fn failures_report_and_stale_results_drop() {
        let mut ui = image_ui(Fit::Contain);
        let old = ui.take_image_requests()[0].id();
        let mut t = Transaction::new(2);
        t.payload(1, &b"other"[..]);
        ui.apply_txn(&t).unwrap();
        let new = ui.take_image_requests()[0].id();
        assert_ne!(old, new);
        ui.image_result(ImageResult::Size {
            id: old,
            width: 10,
            height: 10,
        });
        assert!(ui.host.images[&1].natural.is_none(), "stale result applied");
        ui.image_result(ImageResult::Failed {
            id: new,
            error: "unsupported format".into(),
        });
        let ev = events(&mut ui);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].key, image_event::FAILED);
        assert_eq!(ev[0].text, "unsupported format");
        ui.render(Size::new(100.0, 100.0));
        assert!(ui.take_image_requests().is_empty());
    }

    /// A box that grows asks for more pixels; one that shrinks a little
    /// keeps them. Removing the node releases its raster.
    #[test]
    fn resizes_redecode_past_the_hysteresis_and_removal_releases() {
        let mut ui = image_ui(Fit::Fill);
        let id = ui.take_image_requests()[0].id();
        ui.image_result(ImageResult::Size {
            id,
            width: 1000,
            height: 1000,
        });
        ui.render(Size::new(100.0, 100.0));
        ui.take_image_requests();
        ui.image_result(ImageResult::Pixels {
            id,
            crop: [0, 0, 1000, 1000],
            width: 80,
            height: 80,
            rgba: vec![1; 80 * 80 * 4],
        });
        let resize = |ui: &mut Ui, side: f32| {
            let mut t = Transaction::new(3);
            t.layout(
                1,
                &taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(side),
                        height: taffy::Dimension::length(side),
                    },
                    ..Default::default()
                },
            );
            ui.apply_txn(&t).unwrap();
            ui.render(Size::new(200.0, 200.0));
            ui.take_image_requests()
        };
        assert!(resize(&mut ui, 30.0).is_empty(), "a small shrink redecodes");
        let raster = ui.images.states[&1].bitmap.as_ref().unwrap().raster;
        assert_eq!(ui.scene.atlas.entry(raster).quad_w, 60);
        let reqs = resize(&mut ui, 50.0);
        assert!(matches!(
            reqs[..],
            [ImageRequest::Decode {
                width: 100,
                height: 100,
                ..
            }]
        ));
        let mut t = Transaction::new(4);
        t.remove(1);
        ui.apply_txn(&t).unwrap();
        assert!(!ui.scene.atlas.entry(raster).resident);
        assert_eq!(ui.image_bytes(), 0);
    }

    /// Bad input rejects the transaction; undecodable bytes do not (they
    /// fail later, as an event).
    #[test]
    fn image_ops_validate() {
        let mut ui = image_ui(Fit::Cover);
        let mut t = Transaction::new(2);
        t.create(2, NodeKind::View).image_config(2, Fit::Fill);
        assert!(ui.apply_txn(&t).is_err());
        let mut t = Transaction::new(2);
        t.payload(1, &[0xFFu8; 3][..]);
        assert!(ui.apply_txn(&t).is_ok());
        let mut t = Transaction::new(3);
        t.image_config(1, Fit::Contain);
        let mut buf = crate::wire::encode(&t);
        *buf.last_mut().unwrap() = 3;
        assert!(crate::wire::decode(&buf).is_err());
        buf.pop();
        assert!(crate::wire::decode(&buf).is_err());
    }
}

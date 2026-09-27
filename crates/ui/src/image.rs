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
/// (content-box coordinates, logical, its origin on a device pixel) and
/// its size in device pixels, and the pixel size to decode to.
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
    // The drawn rect starts on a device pixel, so a bitmap decoded at
    // its drawn size maps texel to pixel (no blur from a half-pixel
    // letterbox offset). The snap is relative to the node, whose chunk
    // origin snaps in world space (at rest, as text does).
    let snap = |v: f32| (v * scale).round() / scale;
    let mut dest = Rect::new(
        snap(dest.origin.x),
        snap(dest.origin.y),
        dest.size.width,
        dest.size.height,
    );
    let px = |v: f32| (v * scale).round().max(1.0);
    let (mut qw, mut qh) = (px(dest.size.width), px(dest.size.height));
    // A quad side is a u16 of device pixels: a larger box draws its
    // image at the largest size that fits, aspect kept.
    let over = qw.max(qh) / u16::MAX as f32;
    if over > 1.0 {
        (qw, qh) = ((qw / over).floor().max(1.0), (qh / over).floor().max(1.0));
        dest.size = craie_core::geom::Size::new(qw / scale, qh / scale);
    }
    let mut size = [(qw as u32).min(crop[2]), (qh as u32).min(crop[3])];
    let over = size[0].max(size[1]);
    if over > max {
        size = size.map(|v| ((v as u64 * max as u64 / over as u64) as u32).max(1));
    }
    Some(Plan {
        crop,
        dest,
        quad: [qw as u16, qh as u16],
        size,
    })
}

/// A request that grows a bitmap, rounded up a step (x1.25) so a box
/// that keeps growing (a window drag) decodes every few frames, not
/// every frame; capped by the source rect and a page.
fn grown(size: [u32; 2], crop: [u32; 4], max: u32) -> [u32; 2] {
    let [w, h] = size.map(|v| v as f32);
    let k = 1.25f32
        .min(crop[2] as f32 / w)
        .min(crop[3] as f32 / h)
        .min(max as f32 / w.max(h));
    if k <= 1.0 {
        return size;
    }
    [
        ((w * k) as u32).max(size[0]).min(crop[2]),
        ((h * k) as u32).max(size[1]).min(crop[3]),
    ]
}

/// Decoded pixels on screen and their raster.
struct Bitmap {
    /// The payload they came from: the node's current one, or, until
    /// that one loads or fails, the one before (a new `src` keeps the
    /// old image up).
    id: ImageId,
    natural: [u32; 2],
    crop: [u32; 4],
    size: [u32; 2],
    /// The CPU copy, to re-insert after an atlas eviction. Dropped past
    /// the memory budget, least recently drawn first; an eviction then
    /// decodes again.
    rgba: Option<Vec<u8>>,
    raster: RasterId,
}

impl Bitmap {
    /// Whether `p` wants other pixels: a larger size, less than half
    /// the size on a side (hysteresis: a box that shrinks a little keeps
    /// its pixels, drawn scaled down), or a source rect whose edges moved
    /// more than 2 drawn pixels (a cover box that changes aspect).
    fn stale(&self, p: &Plan) -> bool {
        let [w, h] = self.size;
        let edges = |c: [u32; 4]| [c[0], c[0] + c[2], c[1], c[1] + c[3]].map(|v| v as f32);
        let (a, b) = (edges(self.crop), edges(p.crop));
        let per = [
            p.quad[0] as f32 / p.crop[2] as f32,
            p.quad[1] as f32 / p.crop[3] as f32,
        ];
        (0..4).any(|i| (a[i] - b[i]).abs() * per[i / 2] > 2.0)
            || p.size[0] > w
            || p.size[1] > h
            || p.size[0] * 2 < w
            || p.size[1] * 2 < h
    }

    /// Whether `p` is drawn at another size, past a pixel of slack.
    fn off(&self, p: &Plan) -> bool {
        (0..2).any(|i| self.size[i].abs_diff(p.size[i]) > 1)
    }
}

/// Decode state of one image node.
struct State {
    /// The current payload.
    id: ImageId,
    bytes: Arc<[u8]>,
    /// The current payload's natural size, once probed.
    natural: Option<[u32; 2]>,
    /// A request for this payload is with the platform: at most one at
    /// a time, so a resizing box does not queue a decode per frame.
    pending: bool,
    /// Why the current payload failed. Nothing more is asked for it.
    error: Option<String>,
    /// The load event went out.
    loaded: bool,
    /// The plan the node's last build asked for: one unchanged since
    /// then has settled.
    last: Option<Plan>,
    bitmap: Option<Bitmap>,
}

impl State {
    /// Whether the bitmap can be decoded again (it is of this payload,
    /// which has not failed): only then may its CPU copy go.
    fn restorable(&self) -> bool {
        self.bitmap
            .as_ref()
            .is_some_and(|b| b.id == self.id && self.error.is_none())
    }
}

/// CPU copies of decoded pixels kept for re-insertion (`Bitmap::rgba`).
const BUDGET: usize = 64 << 20;

/// Image nodes' decode state and the platform's work queue.
pub(crate) struct Images {
    next: u64,
    /// Each live image's node.
    nodes: HashMap<ImageId, u32>,
    /// By node id.
    states: HashMap<u32, State>,
    requests: Vec<ImageRequest>,
    /// Payloads replaced or removed since the platform last asked: work
    /// it queued for them is moot.
    dropped: Vec<ImageId>,
    /// Bytes of CPU copies to keep (`BUDGET`; tests set less).
    pub(crate) budget: usize,
}

impl Default for Images {
    fn default() -> Images {
        Images {
            next: 0,
            nodes: HashMap::new(),
            states: HashMap::new(),
            requests: Vec::new(),
            dropped: Vec::new(),
            budget: BUDGET,
        }
    }
}

impl Images {
    /// Retires a payload: its queued requests go, and results for it
    /// are dropped from now on.
    fn retire(&mut self, id: ImageId) {
        self.nodes.remove(&id);
        self.requests.retain(|r| r.id() != id);
        self.dropped.push(id);
    }

    /// Drops a node's image: its raster, queued requests, and the
    /// mapping that routes results to it (late results are dropped).
    pub(crate) fn forget(&mut self, node: u32, atlas: &mut RasterAtlas) {
        let Some(st) = self.states.remove(&node) else {
            return;
        };
        self.retire(st.id);
        if let Some(b) = st.bitmap {
            atlas.release(b.raster);
        }
    }

    /// Re-inserts evicted image rasters that a visible chunk draws
    /// (`missing` sorted by id) from their CPU copy, or, when the budget
    /// dropped it, asks for the same pixels again.
    pub(crate) fn ensure_resident(&mut self, missing: &[RasterId], atlas: &mut RasterAtlas) {
        for st in self.states.values_mut() {
            let Some(b) = &st.bitmap else { continue };
            if missing.binary_search_by_key(&b.raster.0, |r| r.0).is_err() {
                continue;
            }
            match &b.rgba {
                Some(rgba) => atlas.insert(b.raster, rgba),
                None if st.restorable() && !st.pending => {
                    st.pending = true;
                    self.requests.push(ImageRequest::Decode {
                        id: st.id,
                        bytes: st.bytes.clone(),
                        crop: b.crop,
                        width: b.size[0],
                        height: b.size[1],
                    });
                }
                None => {}
            }
        }
    }

    /// Drops CPU copies past the budget, least recently drawn first
    /// (the atlas's use stamps). `keep` stays: it was just decoded. So
    /// does a copy that could not be decoded again (`restorable`): an
    /// old `src` up while the new one loads, or pixels of a payload that
    /// later failed.
    fn trim(&mut self, keep: u32, atlas: &RasterAtlas) {
        let mut held = self.bytes();
        if held <= self.budget {
            return;
        }
        let epoch = atlas.epoch();
        let mut old: Vec<(u32, u32)> = self
            .states
            .iter()
            .filter(|&(&node, st)| node != keep && st.restorable())
            .filter_map(|(&node, st)| {
                let b = st.bitmap.as_ref().filter(|b| b.rgba.is_some())?;
                Some((epoch.wrapping_sub(atlas.entry(b.raster).last_used), node))
            })
            .collect();
        old.sort_unstable_by(|a, b| b.cmp(a));
        for (_, node) in old {
            if held <= self.budget {
                break;
            }
            let b = self.states.get_mut(&node).and_then(|s| s.bitmap.as_mut());
            if let Some(rgba) = b.and_then(|b| b.rgba.take()) {
                held -= rgba.len();
            }
        }
    }

    /// Bytes of CPU copies held (the atlas holds its own while
    /// resident).
    pub(crate) fn bytes(&self) -> usize {
        self.states
            .values()
            .filter_map(|s| s.bitmap.as_ref()?.rgba.as_ref())
            .map(Vec::len)
            .sum()
    }
}

impl Ui {
    /// Decode work queued for the platform since the last call.
    pub fn take_image_requests(&mut self) -> Vec<ImageRequest> {
        std::mem::take(&mut self.images.requests)
    }

    /// Payloads replaced or removed since the last call: the platform
    /// drops work it still has queued for them. The embedder must call
    /// it (as it calls `take_image_requests`), or the list grows.
    pub fn take_dropped_images(&mut self) -> Vec<ImageId> {
        std::mem::take(&mut self.images.dropped)
    }

    /// Bytes of decoded image pixels held on the CPU (the atlas holds
    /// its own copy while they are resident).
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
        st.pending = false;
        match result {
            ImageResult::Size { width, height, .. } => {
                if width == 0 || height == 0 {
                    self.image_failed(node, "empty image".into());
                    return;
                }
                st.natural = Some([width, height]);
                if st.bitmap.is_none() {
                    // The natural size is the intrinsic size. A previous
                    // image still up keeps its own until this one draws.
                    self.set_natural(node, Some([width, height]));
                }
                self.host.dirty.content.push(node);
            }
            ImageResult::Pixels {
                crop,
                width,
                height,
                rgba,
                ..
            } => {
                let max = self.scene.atlas.page_size() - 2;
                if width == 0 || height == 0 || width > max || height > max {
                    return;
                }
                if rgba.len() != width as usize * height as usize * 4 {
                    return;
                }
                let natural = st.natural.unwrap_or([width, height]);
                if let Some(old) = st.bitmap.take() {
                    self.scene.atlas.release(old.raster);
                }
                let (w, h) = (width as u16, height as u16);
                let raster = self.scene.atlas.new_scaled_id(w, h, w, h, true);
                self.scene.atlas.insert(raster, &rgba);
                st.bitmap = Some(Bitmap {
                    id: st.id,
                    natural,
                    crop,
                    size: [width, height],
                    rgba: Some(rgba),
                    raster,
                });
                let first = !std::mem::replace(&mut st.loaded, true);
                // The chunk names the old raster: it rebuilds before the
                // next draw.
                self.host.dirty.content.push(node);
                self.set_natural(node, Some(natural));
                self.images.trim(node, &self.scene.atlas);
                if first {
                    self.image_loaded(node, natural);
                }
            }
            ImageResult::Failed { error, .. } => self.image_failed(node, error),
        }
    }

    /// The layout's natural size (the intrinsic size of an unsized
    /// image).
    fn set_natural(&mut self, node: u32, natural: Option<[u32; 2]>) {
        if let Some(d) = self.host.images.get_mut(&node)
            && d.natural != natural
        {
            d.natural = natural;
            self.host.revs.layout_input.bump();
            self.host.mark_layout(NodeId(node));
        }
    }

    /// The current payload failed. A previous image still up goes (it
    /// is not this `src`); pixels of this payload stay (a later decode
    /// at another size failed) and it is not asked again.
    fn image_failed(&mut self, node: u32, error: String) {
        let Some(st) = self.images.states.get_mut(&node) else {
            return;
        };
        st.error = Some(error.clone());
        if let Some(b) = st.bitmap.take_if(|b| b.id != st.id) {
            self.scene.atlas.release(b.raster);
        }
        if st.bitmap.is_none() {
            self.set_natural(node, None);
        }
        self.host.dirty.content.push(node);
        self.image_event(NodeId(node), image_event::FAILED, &error);
    }

    fn image_loaded(&mut self, node: u32, [w, h]: [u32; 2]) {
        let mut e = self.event(out_kind::IMAGE, NodeId(node));
        e.key = image_event::LOADED;
        e.x = w as f32;
        e.y = h as f32;
        self.pending_events.push(e);
    }

    fn image_event(&mut self, id: NodeId, key: u32, text: &str) {
        let mut e = self.event(out_kind::IMAGE, id);
        e.key = key;
        e.text = text.to_string();
        self.pending_events.push(e);
    }

    /// A PAYLOAD on an Image node: new bytes, a new `ImageId`, and a
    /// probe for the natural size. The image on screen stays until the
    /// new one draws or fails. Empty bytes clear the image. Equal bytes
    /// report again (a new `src` with the same content still gets its
    /// load or error event).
    pub(crate) fn set_image(&mut self, node: u32, bytes: &[u8]) {
        let Some(d) = self.host.images.get(&node) else {
            return;
        };
        if d.bytes[..] == *bytes {
            let st = self.images.states.get(&node);
            if let Some(error) = st.and_then(|s| s.error.clone()) {
                self.image_event(NodeId(node), image_event::FAILED, &error);
            } else if let Some(st) = st.filter(|s| s.loaded) {
                let natural = st.natural.unwrap_or_default();
                self.image_loaded(node, natural);
            }
            return;
        }
        let bytes: Arc<[u8]> = bytes.into();
        self.host.copied_bytes += bytes.len() as u64;
        if let Some(d) = self.host.images.get_mut(&node) {
            d.bytes = bytes.clone();
        }
        self.host.revs.resource.bump();
        self.host.dirty.content.push(node);
        if bytes.is_empty() {
            self.images.forget(node, &mut self.scene.atlas);
            self.set_natural(node, None);
            return;
        }
        self.images.next += 1;
        let id = ImageId(self.images.next);
        let bitmap = match self.images.states.remove(&node) {
            Some(old) => {
                self.images.retire(old.id);
                old.bitmap
            }
            None => None,
        };
        self.images.nodes.insert(id, node);
        self.images.states.insert(
            node,
            State {
                id,
                bytes: bytes.clone(),
                natural: None,
                pending: true,
                error: None,
                loaded: false,
                last: None,
                bitmap,
            },
        );
        self.images.requests.push(ImageRequest::Probe { id, bytes });
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
    /// pixels (a step ahead, while the box grows); until they arrive,
    /// the old pixels draw scaled. Once the plan holds for a frame,
    /// pixels at another size are decoded again at the drawn size.
    pub(crate) fn build_image(&mut self, id: NodeId, data: &LayoutData, w: &mut ChunkWriter) {
        let Some(d) = self.host.images.get(&id.0) else {
            return;
        };
        let Some(st) = self.images.states.get_mut(&id.0) else {
            return;
        };
        let content = Rect::new(
            data.content[0],
            data.content[1],
            (data.rect.size.width - data.insets[0]).max(0.0),
            (data.rect.size.height - data.insets[1]).max(0.0),
        );
        let max = self.scene.atlas.page_size() - 2;
        let planned = st
            .natural
            .filter(|_| st.error.is_none())
            .and_then(|n| plan(d.fit, n, content, self.scale, max));
        let settled = planned.is_some() && planned == st.last;
        st.last = planned;
        if let Some(p) = planned.filter(|_| !st.pending) {
            let size = match st.bitmap.as_ref().filter(|b| b.id == st.id) {
                None => Some(p.size),
                Some(b) if b.stale(&p) => Some(if p.size[0] > b.size[0] || p.size[1] > b.size[1] {
                    grown(p.size, p.crop, max)
                } else {
                    p.size
                }),
                Some(b) if b.off(&p) && settled => Some(p.size),
                Some(b) if b.off(&p) => {
                    // Look again next frame: if the box holds, it settles.
                    self.host.dirty.content.push(id.0);
                    None
                }
                Some(_) => None,
            };
            if let Some(size) = size {
                st.pending = true;
                self.images.requests.push(ImageRequest::Decode {
                    id: st.id,
                    bytes: st.bytes.clone(),
                    crop: p.crop,
                    width: size[0],
                    height: size[1],
                });
            }
        }
        // The pixels on screen, planned against their own image (a
        // previous `src` keeps its own aspect until the new one draws).
        let Some(b) = &st.bitmap else { return };
        let Some(p) = plan(d.fit, b.natural, content, self.scale, max) else {
            return;
        };
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
        // A grow asks a step ahead: 100 px drawn, 125 decoded, so the
        // next few grows need nothing.
        let reqs = resize(&mut ui, 50.0);
        let [
            ImageRequest::Decode {
                width: 125,
                height: 125,
                crop,
                ..
            },
        ] = reqs[..]
        else {
            panic!("{reqs:?}")
        };
        ui.image_result(ImageResult::Pixels {
            id,
            crop,
            width: 125,
            height: 125,
            rgba: vec![1; 125 * 125 * 4],
        });
        assert!(resize(&mut ui, 55.0).is_empty(), "within the step");
        assert_eq!(resize(&mut ui, 70.0).len(), 1);
        let raster = ui.images.states[&1].bitmap.as_ref().unwrap().raster;
        let mut t = Transaction::new(4);
        t.remove(1);
        ui.apply_txn(&t).unwrap();
        assert!(!ui.scene.atlas.entry(raster).resident);
        assert_eq!(ui.image_bytes(), 0);
        assert!(ui.take_dropped_images().contains(&id));
    }

    /// Answers a request as a decoder would for an image of `natural`
    /// pixels (pixels all 255).
    fn answer(r: &ImageRequest, [w, h]: [u32; 2]) -> ImageResult {
        match *r {
            ImageRequest::Probe { id, .. } => ImageResult::Size {
                id,
                width: w,
                height: h,
            },
            ImageRequest::Decode {
                id,
                crop,
                width,
                height,
                ..
            } => ImageResult::Pixels {
                id,
                crop,
                width,
                height,
                rgba: vec![255; (width * height * 4) as usize],
            },
        }
    }

    /// Renders and answers requests until none are left.
    fn serve(ui: &mut Ui, natural: [u32; 2]) {
        for _ in 0..8 {
            ui.render(Size::new(200.0, 200.0));
            let reqs = ui.take_image_requests();
            if reqs.is_empty() {
                return;
            }
            for r in &reqs {
                ui.image_result(answer(r, natural));
            }
        }
        panic!("image requests never settled");
    }

    fn sized(w: f32, h: f32) -> taffy::Style {
        taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(w),
                height: taffy::Dimension::length(h),
            },
            ..Default::default()
        }
    }

    fn relayout(ui: &mut Ui, node: u32, style: &taffy::Style) {
        let mut t = Transaction::new(9);
        t.layout(node, style);
        ui.apply_txn(&t).unwrap();
    }

    fn payload(ui: &mut Ui, node: u32, bytes: &[u8]) {
        let mut t = Transaction::new(9);
        t.payload(node, bytes);
        ui.apply_txn(&t).unwrap();
    }

    fn unsized_image(bytes: &[u8]) -> Ui {
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        t.create(1, NodeKind::Image)
            .payload(1, bytes)
            .place(NIL, 1, NIL);
        ui.apply_txn(&t).unwrap();
        ui
    }

    fn laid_out(ui: &mut Ui) -> Size {
        ui.render(Size::new(200.0, 200.0));
        ui.layouts.data(NodeId(1)).rect.size
    }

    fn shown(ui: &Ui, node: u32) -> ImageId {
        ui.images.states[&node].bitmap.as_ref().unwrap().id
    }

    /// A new `src` keeps the old image, and its size, on screen until
    /// the new one draws; the old payload's queued work is dropped.
    #[test]
    fn a_new_src_keeps_the_old_image_until_it_draws() {
        let mut ui = unsized_image(b"a");
        serve(&mut ui, [30, 20]);
        let a = ui.images.states[&1].id;
        events(&mut ui);
        payload(&mut ui, 1, b"b");
        assert_eq!(laid_out(&mut ui), Size::new(30.0, 20.0));
        assert_eq!(shown(&ui, 1), a);
        let raster = ui.images.states[&1].bitmap.as_ref().unwrap().raster;
        assert!(ui.scene.atlas.entry(raster).resident);
        assert_eq!(ui.take_dropped_images(), [a]);
        let reqs = ui.take_image_requests();
        let [ImageRequest::Probe { id: b, .. }] = reqs[..] else {
            panic!("{reqs:?}")
        };
        ui.image_result(answer(&reqs[0], [50, 10]));
        assert_eq!(laid_out(&mut ui), Size::new(30.0, 20.0), "not collapsed");
        assert_eq!(shown(&ui, 1), a);
        serve(&mut ui, [50, 10]);
        assert_eq!(shown(&ui, 1), b);
        assert_eq!(laid_out(&mut ui), Size::new(50.0, 10.0));
        let ev = events(&mut ui);
        assert_eq!(
            ev.iter().map(|e| (e.key, e.x, e.y)).collect::<Vec<_>>(),
            [(image_event::LOADED, 50.0, 10.0)]
        );
    }

    /// A new `src` that fails takes the old image down: it is not that
    /// `src`'s picture.
    #[test]
    fn a_failed_src_clears_the_old_image() {
        let mut ui = unsized_image(b"a");
        serve(&mut ui, [30, 20]);
        events(&mut ui);
        payload(&mut ui, 1, b"b");
        let id = ui.take_image_requests()[0].id();
        ui.image_result(ImageResult::Failed {
            id,
            error: "unsupported format".into(),
        });
        assert!(ui.images.states[&1].bitmap.is_none());
        assert_eq!(laid_out(&mut ui), Size::ZERO);
        assert_eq!(events(&mut ui)[0].key, image_event::FAILED);
    }

    /// A header that probes but does not decode (a truncated file)
    /// leaves no intrinsic size behind.
    #[test]
    fn a_decode_failure_after_a_probe_clears_the_natural_size() {
        let mut ui = unsized_image(b"a");
        let probe = ui.take_image_requests();
        ui.image_result(answer(&probe[0], [30, 20]));
        assert_eq!(laid_out(&mut ui), Size::new(30.0, 20.0));
        let id = ui.take_image_requests()[0].id();
        ui.image_result(ImageResult::Failed {
            id,
            error: "unexpected end of file".into(),
        });
        assert_eq!(laid_out(&mut ui), Size::ZERO);
    }

    /// A decode at a new size that fails keeps the pixels already up,
    /// and is not tried again.
    #[test]
    fn a_failed_redecode_keeps_the_pixels() {
        let mut ui = image_ui(Fit::Fill);
        serve(&mut ui, [1000, 1000]);
        events(&mut ui);
        relayout(&mut ui, 1, &sized(60.0, 60.0));
        ui.render(Size::new(200.0, 200.0));
        let id = ui.take_image_requests()[0].id();
        ui.image_result(ImageResult::Failed {
            id,
            error: "out of memory".into(),
        });
        assert_eq!(shown(&ui, 1), id);
        assert_eq!(events(&mut ui)[0].key, image_event::FAILED);
        relayout(&mut ui, 1, &sized(90.0, 90.0));
        ui.render(Size::new(200.0, 200.0));
        assert!(ui.take_image_requests().is_empty());
        assert!(ui.image_bytes() > 0);
    }

    /// Equal bytes (a new `src` with the same content) report again.
    #[test]
    fn equal_bytes_report_again() {
        let mut ui = image_ui(Fit::Cover);
        serve(&mut ui, [30, 20]);
        events(&mut ui);
        payload(&mut ui, 1, b"encoded");
        let ev = events(&mut ui);
        assert_eq!(
            ev.iter().map(|e| (e.key, e.x, e.y)).collect::<Vec<_>>(),
            [(image_event::LOADED, 30.0, 20.0)]
        );
        assert!(ui.take_image_requests().is_empty());

        let mut ui = image_ui(Fit::Cover);
        let id = ui.take_image_requests()[0].id();
        ui.image_result(ImageResult::Failed {
            id,
            error: "bad".into(),
        });
        events(&mut ui);
        payload(&mut ui, 1, b"encoded");
        let ev = events(&mut ui);
        assert_eq!((ev.len(), ev[0].key, &ev[0].text[..]), (1, 1, "bad"));
    }

    /// Two 40 x 40 images, one shown at a time, in an atlas with room
    /// for one: showing one evicts the other's raster.
    fn two_images(budget: usize) -> Ui {
        let mut ui = Ui::new(1.0);
        ui.scene.atlas = RasterAtlas::with_budget(64, 1, 1);
        ui.images.budget = budget;
        let mut t = Transaction::new(1);
        for node in [1u32, 2] {
            let bytes: &[u8] = if node == 1 { b"1" } else { b"2" };
            t.create(node, NodeKind::Image)
                .payload(node, bytes)
                .image_config(node, Fit::Fill)
                .place(NIL, node, NIL);
        }
        ui.apply_txn(&t).unwrap();
        ui
    }

    fn show(ui: &mut Ui, node: u32) {
        let hidden = taffy::Style {
            display: taffy::Display::None,
            ..sized(40.0, 40.0)
        };
        let mut t = Transaction::new(9);
        t.layout(node, &sized(40.0, 40.0)).layout(3 - node, &hidden);
        ui.apply_txn(&t).unwrap();
    }

    fn raster(ui: &Ui, node: u32) -> RasterId {
        ui.images.states[&node].bitmap.as_ref().unwrap().raster
    }

    /// An evicted image comes back from its CPU copy, without a decode.
    #[test]
    fn an_evicted_image_reinserts_from_its_copy() {
        let mut ui = two_images(BUDGET);
        show(&mut ui, 1);
        serve(&mut ui, [100, 100]);
        let r1 = raster(&ui, 1);
        show(&mut ui, 2);
        serve(&mut ui, [100, 100]);
        assert!(!ui.scene.atlas.entry(r1).resident, "evicted");
        assert!(ui.scene.atlas.entry(raster(&ui, 2)).resident);
        show(&mut ui, 1);
        ui.render(Size::new(200.0, 200.0));
        assert!(ui.take_image_requests().is_empty());
        assert!(ui.scene.atlas.entry(r1).resident, "re-inserted");
        assert_eq!(ui.image_bytes(), 2 * 40 * 40 * 4);
    }

    /// Past the budget, the least recently drawn copy goes; its image
    /// decodes again when it is evicted and drawn.
    #[test]
    fn copies_past_the_budget_decode_again() {
        let mut ui = two_images(40 * 40 * 4);
        show(&mut ui, 1);
        serve(&mut ui, [100, 100]);
        show(&mut ui, 2);
        serve(&mut ui, [100, 100]);
        assert_eq!(ui.image_bytes(), 40 * 40 * 4);
        assert!(ui.images.states[&1].bitmap.as_ref().unwrap().rgba.is_none());
        events(&mut ui);
        show(&mut ui, 1);
        ui.render(Size::new(200.0, 200.0));
        let reqs = ui.take_image_requests();
        let [
            ImageRequest::Decode {
                crop,
                width: 40,
                height: 40,
                ..
            },
        ] = reqs[..]
        else {
            panic!("{reqs:?}")
        };
        assert_eq!(crop, [0, 0, 100, 100]);
        ui.image_result(answer(&reqs[0], [100, 100]));
        assert!(ui.scene.atlas.entry(raster(&ui, 1)).resident);
        assert!(events(&mut ui).is_empty(), "loaded once");
    }

    /// An old `src` still up while the new one loads keeps its CPU copy
    /// past the budget: it cannot be decoded again, so after an
    /// eviction it comes back from the copy instead of going blank.
    #[test]
    fn an_old_src_keeps_its_copy() {
        let mut ui = two_images(40 * 40 * 4);
        show(&mut ui, 1);
        serve(&mut ui, [100, 100]);
        let old = shown(&ui, 1);
        payload(&mut ui, 1, b"1b");
        show(&mut ui, 2);
        serve(&mut ui, [100, 100]);
        assert_eq!(shown(&ui, 1), old);
        assert!(!ui.scene.atlas.entry(raster(&ui, 1)).resident, "evicted");
        assert_eq!(ui.image_bytes(), 2 * 40 * 40 * 4, "over the budget");
        show(&mut ui, 1);
        ui.render(Size::new(200.0, 200.0));
        assert_eq!(shown(&ui, 1), old);
        assert!(ui.scene.atlas.entry(raster(&ui, 1)).resident, "re-inserted");
    }

    /// A box that grows decodes a step ahead while it moves, then once
    /// more at its drawn size when it holds.
    #[test]
    fn a_resize_settles_at_the_drawn_size() {
        let mut ui = image_ui(Fit::Fill);
        serve(&mut ui, [1000, 1000]);
        let frame = |ui: &mut Ui| -> Vec<[u32; 2]> {
            ui.render(Size::new(200.0, 200.0));
            let reqs = ui.take_image_requests();
            for r in &reqs {
                ui.image_result(answer(r, [1000, 1000]));
            }
            reqs.iter()
                .map(|r| match *r {
                    ImageRequest::Decode { width, height, .. } => [width, height],
                    ImageRequest::Probe { .. } => panic!("{r:?}"),
                })
                .collect()
        };
        // 80 px, then 84 and 88 (x1.1 over two frames), then held.
        relayout(&mut ui, 1, &sized(42.0, 42.0));
        assert_eq!(frame(&mut ui), [[105, 105]]);
        relayout(&mut ui, 1, &sized(44.0, 44.0));
        assert!(frame(&mut ui).is_empty());
        assert_eq!(frame(&mut ui), [[88, 88]]);
        for _ in 0..3 {
            assert!(frame(&mut ui).is_empty());
        }
        assert!(!ui.needs_paint());
        assert_eq!(ui.images.states[&1].bitmap.as_ref().unwrap().size, [88, 88]);
    }

    /// The decode size is in device pixels: a scale change asks again.
    #[test]
    fn a_scale_change_redecodes() {
        let mut ui = image_ui(Fit::Fill);
        ui.scale = 1.0;
        serve(&mut ui, [1000, 1000]);
        assert_eq!(ui.images.states[&1].bitmap.as_ref().unwrap().size, [40, 40]);
        ui.scale = 2.0;
        ui.render(Size::new(200.0, 200.0));
        let reqs = ui.take_image_requests();
        // 80 px drawn, a step ahead.
        assert!(
            matches!(
                reqs[..],
                [ImageRequest::Decode {
                    width: 100,
                    height: 100,
                    ..
                }]
            ),
            "{reqs:?}"
        );
    }

    /// A cover box that changes aspect redecodes its source rect; a
    /// pixel of change does not. Pixels for an older rect that land
    /// after a resize draw until the new rect's arrive.
    #[test]
    fn cover_redecodes_a_new_aspect_and_takes_late_pixels() {
        let mut ui = image_ui(Fit::Cover);
        serve(&mut ui, [4000, 3000]);
        let crop_of = |ui: &Ui| ui.images.states[&1].bitmap.as_ref().unwrap().crop;
        assert_eq!(crop_of(&ui), [500, 0, 3000, 3000]);
        relayout(&mut ui, 1, &sized(40.0, 39.0));
        ui.render(Size::new(200.0, 200.0));
        assert!(ui.take_image_requests().is_empty(), "a pixel of aspect");
        relayout(&mut ui, 1, &sized(60.0, 40.0));
        ui.render(Size::new(200.0, 200.0));
        let wide = ui.take_image_requests();
        assert_eq!(wide.len(), 1);
        relayout(&mut ui, 1, &sized(40.0, 60.0));
        ui.render(Size::new(200.0, 200.0));
        assert!(ui.take_image_requests().is_empty(), "one at a time");
        ui.image_result(answer(&wide[0], [4000, 3000]));
        assert_eq!(crop_of(&ui), [0, 166, 4000, 2667]);
        ui.render(Size::new(200.0, 200.0));
        let reqs = ui.take_image_requests();
        let [ImageRequest::Decode { crop, .. }] = reqs[..] else {
            panic!("{reqs:?}")
        };
        assert_eq!(crop, [1000, 0, 2000, 3000]);
    }

    /// The drawn rect starts on a device pixel; a quad past a u16 keeps
    /// its aspect.
    #[test]
    fn plans_snap_to_device_pixels_and_clamp_the_quad() {
        // Contain letterboxes 3 x 2 in 10 x 10: y = 1.67, snapped to 2.
        let p = plan_of(Fit::Contain, [3, 2], 10.0, 10.0, 1.0);
        assert_eq!(p.dest.origin.y, 2.0);
        let p = plan_of(Fit::Contain, [3, 2], 10.0, 10.0, 1.5);
        assert_eq!(p.dest.origin.y * 1.5, 3.0);
        let p = plan_of(Fit::Fill, [100, 100], 100_000.0, 10.0, 1.0);
        assert_eq!(p.quad, [65535, 6]);
        assert_eq!(p.dest.size, Size::new(65535.0, 6.0));
    }

    /// The snap is node-relative, and the node's chunk origin snaps in
    /// world space: at 1.25x, a letterboxed image at x = 11 pt (13.75
    /// device px) still starts on a device pixel.
    #[test]
    fn the_quad_lands_on_device_pixels_at_fractional_scales() {
        for scale in [1.25, 1.5] {
            let mut ui = Ui::new(scale);
            let inset = |v: f32| taffy::LengthPercentage::length(v);
            let parent = taffy::Style {
                padding: taffy::Rect {
                    left: inset(11.0),
                    top: inset(7.3),
                    right: inset(0.0),
                    bottom: inset(0.0),
                },
                ..Default::default()
            };
            let mut t = Transaction::new(1);
            t.create(2, NodeKind::View)
                .layout(2, &parent)
                .create(1, NodeKind::Image)
                .layout(1, &sized(10.0, 10.0))
                .payload(1, &b"x"[..])
                .image_config(1, Fit::Contain)
                .place(NIL, 2, NIL)
                .place(2, 1, NIL);
            ui.apply_txn(&t).unwrap();
            serve(&mut ui, [3, 2]);
            let r = ui.scene.chunk_world_bounds(1);
            assert!(r.origin.x > 11.0 * scale - 1.0, "inset: {r:?}");
            assert!(r.size.height < 10.0 * scale, "letterboxed: {r:?}");
            assert_eq!(r.origin.x, r.origin.x.round(), "{scale}: {r:?}");
            assert_eq!(r.origin.y, r.origin.y.round(), "{scale}: {r:?}");
        }
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

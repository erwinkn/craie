//! Fonts (ARCHITECTURE.md §6): faces and instances owned by craie-text,
//! and the source contract that finds them.
//!
//! A `FontFaceId` names one face of one font file; a `FontInstanceId`
//! names a face at a variation location with synthesis (faux bold and
//! italic). Both are dense u16 indices into the `FontStore`, the one
//! table of faces: shaping, metrics, and rasterization all read it.
//!
//! Discovery and fallback are the platform's: a `FontSource` answers
//! "which face for this family and style" and "which faces may cover this
//! character". Desktop uses fontique (craie-platform-winit); browser
//! profiles and tests pass raw font data (`RawFonts`). Nothing here
//! enumerates system fonts.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::raw::{FileRef, FontRef, TableProvider};
use skrifa::string::StringId;
use skrifa::{MetadataProvider, attribute::Style};

/// A font file's bytes, shared without copying (a platform blob, an
/// `Arc<[u8]>`, a static slice).
pub type FaceBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontFaceId(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontInstanceId(pub u16);

/// Faux styling a face needs to match a request it lacks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Synthesis {
    pub embolden: bool,
    /// Faux italic angle, degrees (0: none).
    pub skew: f32,
}

/// One face a source offers: bytes, face index, a stable identity for
/// the bytes, and the synthesis the match needs.
#[derive(Clone)]
pub struct FontBlob {
    /// Identity of the bytes, unique in the process: two blobs with the
    /// same id and index are the same face. Platform (fontique) blob ids
    /// come from fontique's process-wide counter and stay below
    /// `RAW_ID_BASE`; `RawFonts` ids come from its own process-wide
    /// counter at or above it. A custom `FontSource` must take its ids
    /// from one of these two counters (fontique blobs, or bytes handed to
    /// `RawFonts`); an id it invents can alias another source's face.
    pub id: u64,
    pub index: u32,
    pub bytes: FaceBytes,
    pub synthesis: Synthesis,
    /// Variation settings the match needs (a variable font at the
    /// requested weight, width, or slant): axis tag and user value.
    pub variations: Vec<([u8; 4], f32)>,
}

/// Requested style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontAttrs {
    /// CSS weight, 1..=1000.
    pub weight: u16,
    pub italic: bool,
}

impl Default for FontAttrs {
    fn default() -> FontAttrs {
        FontAttrs {
            weight: 400,
            italic: false,
        }
    }
}

/// An ISO 15924 script tag ("Latn", "Arab", "Zyyy").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScriptTag(pub [u8; 4]);

impl ScriptTag {
    pub const COMMON: ScriptTag = ScriptTag(*b"Zyyy");

    pub fn of(ch: char) -> ScriptTag {
        ScriptTag(
            unicode_script::Script::from(ch)
                .as_iso15924_tag()
                .to_be_bytes(),
        )
    }
}

/// The platform contract for fonts: discovery and fallback. Desktop
/// implements it over fontique; browser profiles and tests over raw
/// font data (`RawFonts`).
pub trait FontSource: Send {
    /// The face that best matches `family` (a family name, or a generic
    /// name: `sans-serif`, `serif`, `monospace`, `system-ui`) in `attrs`,
    /// with the synthesis the match needs. None: no such family.
    fn select(&mut self, family: &str, attrs: FontAttrs) -> Option<FontBlob>;

    /// Faces to try, in priority order, for a grapheme `cluster` of
    /// `script` that the selected face does not cover: every face that
    /// covers its first character needing a glyph (`ignorable`), up to and
    /// including the first that covers the whole cluster (the source may
    /// stop there, never before). `emoji`: the cluster asks for emoji
    /// presentation (`emoji_presentation`), so emoji faces go first. The
    /// order must not depend on enumeration order.
    fn fallback(
        &mut self,
        cluster: &str,
        script: ScriptTag,
        attrs: FontAttrs,
        emoji: bool,
    ) -> Vec<FontBlob>;
}

/// Characters a font need not cover for a cluster to be covered
/// (controls, zero-width and bidi formatting, variation selectors).
pub fn ignorable(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{E0100}'..='\u{E01EF}')
}

/// Whether a cluster asks for emoji presentation (UTS #51): a
/// variation selector 15 asks for text; a variation selector 16 or a
/// keycap asks for emoji; else the first character's Emoji_Presentation
/// property decides (Unicode 17 tables, `unicode-properties`).
pub fn emoji_presentation(cluster: &str) -> bool {
    use unicode_properties::emoji::{EmojiStatus, UnicodeEmoji};
    if cluster.contains('\u{FE0E}') {
        return false;
    }
    if cluster.contains(['\u{FE0F}', '\u{20E3}']) {
        return true;
    }
    cluster.chars().next().is_some_and(|c| {
        matches!(
            c.emoji_status(),
            EmojiStatus::EmojiPresentation
                | EmojiStatus::EmojiPresentationAndModifierBase
                | EmojiStatus::EmojiPresentationAndEmojiComponent
                | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
        )
    })
}

/// One face in the store.
pub struct Face {
    pub id: u64,
    pub index: u32,
    pub bytes: FaceBytes,
    pub units_per_em: u16,
    /// Coverage of U+0000..U+007F, one bit per character: the common
    /// case of `covers` without parsing the font.
    pub ascii: u128,
}

impl Face {
    /// The face's font tables (parsed on demand; cheap: a table
    /// directory read).
    pub fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(self.bytes.as_ref().as_ref(), self.index).ok()
    }
}

/// A face at a variation location with synthesis.
pub struct Instance {
    pub face: FontFaceId,
    /// Normalized variation coordinates (F2Dot14 bits, axis order).
    pub coords: Box<[i16]>,
    pub synthesis: Synthesis,
}

/// An instance's interning key: face, coordinates, embolden, skew bits.
type InstanceKey = (FontFaceId, Box<[i16]>, bool, u32);

/// The one table of faces and instances. Interning keeps ids stable for
/// the engine's life, so glyph cache keys and shaped runs can hold them.
#[derive(Default)]
pub struct FontStore {
    faces: Vec<Face>,
    face_ix: HashMap<(u64, u32), FontFaceId>,
    instances: Vec<Instance>,
    instance_ix: HashMap<InstanceKey, FontInstanceId>,
    /// HarfRust per-face shaping data, built on first use.
    shapers: Vec<Option<harfrust::ShaperData>>,
    /// Coverage of non-ASCII characters per face (the charmap parse is
    /// the cost; ASCII reads the face's mask).
    coverage: HashMap<(FontFaceId, char), bool>,
}

impl FontStore {
    pub fn new() -> FontStore {
        FontStore::default()
    }

    /// Interns a face. None when the bytes are not a font or the face is
    /// missing.
    pub fn face(&mut self, blob: &FontBlob) -> Option<FontFaceId> {
        if let Some(&id) = self.face_ix.get(&(blob.id, blob.index)) {
            return Some(id);
        }
        let font = FontRef::from_index(blob.bytes.as_ref().as_ref(), blob.index).ok()?;
        let units_per_em = font.head().ok()?.units_per_em();
        let charmap = font.charmap();
        let ascii = (0u8..128)
            .filter(|&b| charmap.map(b as char).is_some_and(|g| g.to_u32() != 0))
            .fold(0u128, |m, b| m | 1 << b);
        let id = FontFaceId(u16::try_from(self.faces.len()).ok()?);
        self.faces.push(Face {
            id: blob.id,
            index: blob.index,
            bytes: blob.bytes.clone(),
            units_per_em,
            ascii,
        });
        self.shapers.push(None);
        self.face_ix.insert((blob.id, blob.index), id);
        Some(id)
    }

    /// Interns an instance of `face`.
    pub fn instance(
        &mut self,
        face: FontFaceId,
        coords: &[i16],
        synthesis: Synthesis,
    ) -> Option<FontInstanceId> {
        let key = (
            face,
            Box::<[i16]>::from(coords),
            synthesis.embolden,
            synthesis.skew.to_bits(),
        );
        if let Some(&id) = self.instance_ix.get(&key) {
            return Some(id);
        }
        let id = FontInstanceId(u16::try_from(self.instances.len()).ok()?);
        self.instances.push(Instance {
            face,
            coords: coords.into(),
            synthesis,
        });
        self.instance_ix.insert(key, id);
        Some(id)
    }

    /// Interns the face of `blob` and its instance at the blob's
    /// variation settings, with its synthesis.
    pub fn instance_of(&mut self, blob: &FontBlob) -> Option<FontInstanceId> {
        let face = self.face(blob)?;
        let coords: Vec<i16> = if blob.variations.is_empty() {
            Vec::new()
        } else {
            let font = self.faces[face.0 as usize].font()?;
            let settings = blob
                .variations
                .iter()
                .map(|(tag, value)| (skrifa::Tag::new(tag), *value));
            let location = font.axes().location(settings);
            let coords: Vec<i16> = location.coords().iter().map(|c| c.to_bits()).collect();
            // All-default coordinates are the default instance.
            if coords.iter().all(|&c| c == 0) {
                Vec::new()
            } else {
                coords
            }
        };
        self.instance(face, &coords, blob.synthesis)
    }

    pub fn face_data(&self, id: FontFaceId) -> &Face {
        &self.faces[id.0 as usize]
    }

    pub fn instance_data(&self, id: FontInstanceId) -> &Instance {
        &self.instances[id.0 as usize]
    }

    pub fn face_of(&self, id: FontInstanceId) -> &Face {
        self.face_data(self.instance_data(id).face)
    }

    /// Whether the face of `id` maps `ch` to a glyph.
    pub fn covers(&mut self, id: FontInstanceId, ch: char) -> bool {
        let face_id = self.instances[id.0 as usize].face;
        let face = &self.faces[face_id.0 as usize];
        if ch.is_ascii() {
            return face.ascii & (1 << ch as u32) != 0;
        }
        *self.coverage.entry((face_id, ch)).or_insert_with(|| {
            face.font()
                .is_some_and(|f| f.charmap().map(ch).is_some_and(|g| g.to_u32() != 0))
        })
    }

    /// HarfRust shaping data of `face`, built once.
    pub fn shaper_data(&mut self, face: FontFaceId) -> Option<&harfrust::ShaperData> {
        let i = face.0 as usize;
        if self.shapers[i].is_none() {
            let f = &self.faces[i];
            let font = harfrust::FontRef::from_index(f.bytes.as_ref().as_ref(), f.index).ok()?;
            self.shapers[i] = Some(harfrust::ShaperData::new(&font));
        }
        self.shapers[i].as_ref()
    }

    /// Shaping data built by `shaper_data`.
    pub fn shaper(&self, face: FontFaceId) -> &harfrust::ShaperData {
        self.shapers[face.0 as usize]
            .as_ref()
            .expect("shaper data built first")
    }

    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

/// One registered face of `RawFonts`.
struct RawFace {
    blob: FontBlob,
    /// Lower-case family name.
    family: String,
    weight: u16,
    italic: bool,
    /// A variable face's weight axis (`wght`): its range. It matches any
    /// weight in it, instanced there.
    wght: Option<(f32, f32)>,
}

/// A `FontSource` over font files handed over as bytes: browser profiles
/// and tests. Family matching by name, style by nearest weight and
/// italic (with synthesis for a missing bold or italic), fallback by
/// character coverage in registration order.
#[derive(Default)]
pub struct RawFonts {
    faces: Vec<RawFace>,
    /// The family generic names (`sans-serif`, `system-ui`, ...) resolve
    /// to: the first registered family unless set.
    default_family: Option<String>,
}

/// The first `RawFonts` byte identity. The ids of every `RawFonts` come
/// from one process-wide counter, so two sources never reuse an id for
/// different bytes.
pub const RAW_ID_BASE: u64 = 1 << 63;

static NEXT_RAW_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(RAW_ID_BASE);

impl RawFonts {
    pub fn new() -> RawFonts {
        RawFonts::default()
    }

    /// Registers every face of a font file (a collection has several).
    /// Returns how many faces it added.
    pub fn add(&mut self, bytes: FaceBytes) -> usize {
        self.add_as(bytes, None)
    }

    /// How many faces of `data` would register: 0 when it is not a font
    /// file (or no face names a family).
    pub fn faces_in(data: &[u8]) -> usize {
        let count = match FileRef::new(data) {
            Ok(FileRef::Collection(c)) => c.len(),
            Ok(FileRef::Font(_)) => 1,
            Err(_) => 0,
        };
        (0..count)
            .filter(|&i| {
                FontRef::from_index(data, i).is_ok_and(|f| {
                    f.localized_strings(StringId::FAMILY_NAME)
                        .english_or_first()
                        .is_some()
                })
            })
            .count()
    }

    /// `add`, under `family` instead of the file's own family name.
    pub fn add_as(&mut self, bytes: FaceBytes, family: Option<&str>) -> usize {
        let data = bytes.as_ref().as_ref();
        let count = match FileRef::new(data) {
            Ok(FileRef::Collection(c)) => c.len(),
            Ok(FileRef::Font(_)) => 1,
            Err(_) => 0,
        };
        let id = NEXT_RAW_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut added = 0;
        for index in 0..count {
            let Ok(font) = FontRef::from_index(data, index) else {
                continue;
            };
            let name = |id: StringId| {
                font.localized_strings(id)
                    .english_or_first()
                    .map(|s| s.chars().collect::<String>().to_lowercase())
            };
            let Some(family) = family.map(str::to_lowercase).or_else(|| {
                name(StringId::TYPOGRAPHIC_FAMILY_NAME).or_else(|| name(StringId::FAMILY_NAME))
            }) else {
                continue;
            };
            let a = font.attributes();
            let wght = font
                .axes()
                .iter()
                .find(|x| x.tag() == skrifa::Tag::new(b"wght"))
                .map(|x| (x.min_value(), x.max_value()));
            self.faces.push(RawFace {
                blob: FontBlob {
                    id,
                    index,
                    bytes: bytes.clone(),
                    synthesis: Synthesis::default(),
                    variations: Vec::new(),
                },
                family,
                weight: a.weight.value().round().clamp(1.0, 1000.0) as u16,
                italic: !matches!(a.style, Style::Normal),
                wght,
            });
            added += 1;
        }
        added
    }

    /// Adds a static font file (no copy).
    pub fn add_static(&mut self, bytes: &'static [u8]) -> usize {
        self.add(Arc::new(bytes))
    }

    /// Sets the family generic names resolve to.
    pub fn set_default_family(&mut self, family: &str) {
        self.default_family = Some(family.to_lowercase());
    }

    /// The best face of `family` for `attrs`: italic match first, then
    /// the nearest weight (heavier on a tie for bold requests).
    fn best(&self, family: &str, attrs: FontAttrs) -> Option<&RawFace> {
        self.faces
            .iter()
            .filter(|f| f.family == family)
            .min_by_key(|f| {
                let italic_miss = (f.italic != attrs.italic) as u32;
                // A variable face is at the requested weight anywhere in
                // its axis.
                let w = match f.wght {
                    Some((lo, hi)) => (attrs.weight as f32).clamp(lo, hi).round() as i32,
                    None => f.weight as i32,
                };
                let distance = (w - attrs.weight as i32).unsigned_abs();
                let lighter = (attrs.weight >= 500 && f.weight < attrs.weight) as u32;
                (italic_miss, distance, lighter)
            })
    }

    /// `face` as a match for `attrs`: a variable face instanced at the
    /// weight (clamped to its axis), with synthesis for what it lacks.
    fn matched(face: &RawFace, attrs: FontAttrs) -> FontBlob {
        let mut blob = face.blob.clone();
        let weight = match face.wght {
            Some((lo, hi)) => {
                let w = (attrs.weight as f32).clamp(lo, hi);
                blob.variations = vec![(*b"wght", w)];
                w
            }
            None => face.weight as f32,
        };
        blob.synthesis = Synthesis {
            embolden: attrs.weight >= 600 && weight < 600.0,
            skew: if attrs.italic && !face.italic {
                14.0
            } else {
                0.0
            },
        };
        blob
    }

    /// A generic family name (`sans-serif`, `system-ui`...), which a
    /// source maps to a family of its own.
    pub fn generic(name: &str) -> bool {
        matches!(
            name,
            "" | "sans-serif" | "serif" | "monospace" | "system-ui" | "ui-sans-serif" | "cursive"
        )
    }
}

impl FontSource for RawFonts {
    fn select(&mut self, family: &str, attrs: FontAttrs) -> Option<FontBlob> {
        let family = family.trim().to_lowercase();
        let family = if Self::generic(&family) {
            self.default_family
                .clone()
                .or_else(|| self.faces.first().map(|f| f.family.clone()))?
        } else {
            family
        };
        self.best(&family, attrs).map(|f| Self::matched(f, attrs))
    }

    fn fallback(
        &mut self,
        cluster: &str,
        _script: ScriptTag,
        attrs: FontAttrs,
        _emoji: bool,
    ) -> Vec<FontBlob> {
        let Some(ch) = cluster.chars().find(|&c| !ignorable(c)) else {
            return Vec::new();
        };
        // One face per family that covers `ch`, in registration order: the
        // complete set (the engine picks the first covering the cluster).
        let mut families: Vec<&str> = Vec::new();
        for f in &self.faces {
            if families.contains(&f.family.as_str()) {
                continue;
            }
            let covers = FontRef::from_index(f.blob.bytes.as_ref().as_ref(), f.blob.index)
                .ok()
                .is_some_and(|font| font.charmap().map(ch).is_some_and(|g| g.to_u32() != 0));
            if covers {
                families.push(&f.family);
            }
        }
        families
            .into_iter()
            .filter_map(|fam| self.best(fam, attrs))
            .map(|f| Self::matched(f, attrs))
            .collect()
    }
}

/// The pinned test font files (assets/fonts), Noto Sans first: the
/// fallback order.
#[cfg(feature = "pinned-fonts")]
pub fn pinned_files() -> [&'static [u8]; 9] {
    macro_rules! font {
        ($name:literal) => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/fonts/",
                $name
            ))
        };
    }
    [
        font!("NotoSans-Regular.ttf"),
        font!("NotoSans-Bold.ttf"),
        font!("NotoSans-Italic.ttf"),
        font!("NotoSansArabic-Regular.ttf"),
        font!("NotoSansHebrew-Regular.ttf"),
        font!("NotoSansDevanagari-Regular.ttf"),
        font!("NotoSansJP-Subset-Regular.otf"),
        font!("NotoSansSymbols2-Regular.ttf"),
        font!("NotoEmoji-Subset-Regular.ttf"),
    ]
}

/// The pinned test fonts as a source, Noto Sans the default family.
#[cfg(feature = "pinned-fonts")]
pub fn pinned() -> RawFonts {
    let mut fonts = RawFonts::new();
    for bytes in pinned_files() {
        fonts.add_static(bytes);
    }
    fonts.set_default_family("Noto Sans");
    fonts
}

type SourceFactory = Box<dyn Fn() -> Box<dyn FontSource> + Send + Sync>;

static DEFAULT_SOURCE: std::sync::OnceLock<SourceFactory> = std::sync::OnceLock::new();

/// Sets the font source every new text engine starts with (the platform
/// adapter calls this once at startup). Returns false if one was set.
pub fn set_default_source(
    factory: impl Fn() -> Box<dyn FontSource> + Send + Sync + 'static,
) -> bool {
    DEFAULT_SOURCE.set(Box::new(factory)).is_ok()
}

/// A new engine's font source: the one the platform set; else, with the
/// `pinned-fonts` feature, the pinned test fonts; else no fonts.
pub fn default_source() -> Box<dyn FontSource> {
    if let Some(f) = DEFAULT_SOURCE.get() {
        return f();
    }
    #[cfg(feature = "pinned-fonts")]
    {
        Box::new(pinned())
    }
    #[cfg(not(feature = "pinned-fonts"))]
    {
        Box::new(RawFonts::new())
    }
}

#[cfg(all(test, feature = "pinned-fonts"))]
mod tests {
    use super::*;

    #[test]
    fn raw_fonts_select_and_fall_back() {
        let mut src = pinned();
        let mut store = FontStore::new();
        let regular = src.select("sans-serif", FontAttrs::default()).unwrap();
        let bold = src
            .select(
                "Noto Sans",
                FontAttrs {
                    weight: 700,
                    italic: false,
                },
            )
            .unwrap();
        assert_ne!(
            (regular.id, regular.index),
            (bold.id, bold.index),
            "a real bold face"
        );
        assert!(!bold.synthesis.embolden);
        // Bold italic: the italic face, emboldened.
        let bi = src
            .select(
                "noto sans",
                FontAttrs {
                    weight: 700,
                    italic: true,
                },
            )
            .unwrap();
        assert!(bi.synthesis.embolden && bi.synthesis.skew == 0.0);
        assert!(src.select("No Such Family", FontAttrs::default()).is_none());
        // ✕ is not in Noto Sans: the symbols face covers it.
        let r = store.instance_of(&regular).unwrap();
        assert!(!store.covers(r, '✕'));
        let fb = src.fallback("✕", ScriptTag::of('✕'), FontAttrs::default(), false);
        let f = store.instance_of(&fb[0]).unwrap();
        assert!(store.covers(f, '✕'));
        assert_eq!(ScriptTag::of('ب'), ScriptTag(*b"Arab"));
        // Interning is stable.
        assert_eq!(store.instance_of(&regular), Some(r));
    }

    /// UTS #51 presentation: Emoji_Presentation from the tables, VS15
    /// forces text, VS16 and keycaps force emoji.
    #[test]
    fn emoji_presentation_follows_the_property() {
        for (cluster, emoji) in [
            ("\u{2705}", true),          // ✅ Emoji_Presentation=Yes
            ("\u{2705}\u{FE0E}", false), // ✅ with VS15
            ("\u{2715}", false),         // ✕ not emoji
            ("\u{263A}", false),         // ☺ Emoji=Yes, default text
            ("\u{263A}\u{FE0F}", true),  // ☺ with VS16
            ("\u{2764}", false),         // ❤ default text
            ("1\u{FE0F}\u{20E3}", true), // keycap
            ("1", false),
            ("\u{1F1EF}\u{1F1F5}", true), // flag
            ("\u{1F44D}\u{1F3FD}", true), // skin tone
            ("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", true),
        ] {
            assert_eq!(emoji_presentation(cluster), emoji, "{cluster:?}");
        }
    }
}

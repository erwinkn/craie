//! Desktop font discovery and fallback (ARCHITECTURE.md §6): the
//! platform side of `craie_text::fonts::FontSource`, over fontique.
//! fontique is reachable only through this adapter.

use std::sync::Arc;

use craie_text::fonts::{FontAttrs, FontBlob, FontSource, ScriptTag, Synthesis, ignorable};
use fontique::{
    Attributes, Collection, CollectionOptions, FallbackKey, FontStyle, FontWeight, FontWidth,
    GenericFamily, QueryFamily, QueryFont, QueryStatus, Script, SourceCache,
};

/// System fonts through fontique.
pub struct SystemFonts {
    collection: Collection,
    cache: SourceCache,
}

/// Families tried first in the last-resort scan, per platform, before
/// every other family in name order. Symbol and wide-coverage faces.
const LAST_RESORT: &[&str] = if cfg!(target_os = "macos") {
    &[
        "Apple Symbols",
        "Arial Unicode MS",
        "STIX Two Math",
        "Menlo",
    ]
} else if cfg!(target_os = "windows") {
    &[
        "Segoe UI Symbol",
        "Segoe UI Historic",
        "Arial Unicode MS",
        "Cambria Math",
    ]
} else {
    &[
        "Noto Sans Symbols",
        "Noto Sans Symbols 2",
        "DejaVu Sans",
        "FreeSerif",
    ]
};

impl SystemFonts {
    pub fn new() -> SystemFonts {
        SystemFonts::with_collection(Collection::new(CollectionOptions {
            shared: true,
            system_fonts: true,
        }))
    }

    /// Over a given collection (tests register fonts by hand).
    pub fn with_collection(collection: Collection) -> SystemFonts {
        SystemFonts {
            collection,
            cache: SourceCache::default(),
        }
    }

    /// Every family in the last-resort order: `LAST_RESORT` first, then
    /// the rest by name. Independent of enumeration order.
    fn last_resort_order(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self.collection.family_names().map(str::to_string).collect();
        names.sort_unstable();
        names.dedup();
        let rank = |n: &str| {
            LAST_RESORT
                .iter()
                .position(|p| *p == n)
                .unwrap_or(LAST_RESORT.len())
        };
        names.sort_by_key(|n| rank(n));
        names
    }

    /// Appends the faces of `families` (in order) that cover the
    /// cluster's first character `first`, until one covers the whole
    /// `cluster`. Returns whether one did.
    fn collect<'a>(
        &mut self,
        families: impl IntoIterator<Item = QueryFamily<'a>>,
        fallback: Option<FallbackKey>,
        first: char,
        cluster: &str,
        attrs: FontAttrs,
        out: &mut Vec<FontBlob>,
    ) -> bool {
        let mut complete = false;
        let mut query = self.collection.query(&mut self.cache);
        query.set_families(families);
        query.set_attributes(Self::attributes(attrs));
        if let Some(key) = fallback {
            query.set_fallbacks(key);
        }
        query.matches_with(|font| {
            let seen = out
                .iter()
                .any(|b| b.id == font.blob.id() && b.index == font.index);
            if !seen && Self::covers(font, first) {
                out.push(Self::blob(font));
                if cluster
                    .chars()
                    .filter(|&c| !ignorable(c))
                    .all(|c| Self::covers(font, c))
                {
                    complete = true;
                    return QueryStatus::Stop;
                }
            }
            QueryStatus::Continue
        });
        complete
    }

    fn attributes(attrs: FontAttrs) -> Attributes {
        Attributes::new(
            FontWidth::NORMAL,
            if attrs.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            FontWeight::new(attrs.weight as f32),
        )
    }

    fn blob(font: &QueryFont) -> FontBlob {
        debug_assert!(font.blob.id() < craie_text::fonts::RAW_ID_BASE);
        FontBlob {
            id: font.blob.id(),
            index: font.index,
            bytes: Arc::new(font.blob.clone()),
            synthesis: Synthesis {
                embolden: font.synthesis.embolden(),
                skew: font.synthesis.skew().unwrap_or(0.0),
            },
            variations: font
                .synthesis
                .variation_settings()
                .iter()
                .map(|(tag, value)| (tag.to_be_bytes(), *value))
                .collect(),
        }
    }

    fn generic(name: &str) -> Option<GenericFamily> {
        Some(match name {
            "" | "system-ui" => GenericFamily::SystemUi,
            "sans-serif" => GenericFamily::SansSerif,
            "serif" => GenericFamily::Serif,
            "monospace" => GenericFamily::Monospace,
            "ui-sans-serif" => GenericFamily::UiSansSerif,
            _ => return None,
        })
    }

    fn covers(font: &QueryFont, ch: char) -> bool {
        font.charmap()
            .is_some_and(|m| m.map(ch).is_some_and(|g| g != 0))
    }
}

impl Default for SystemFonts {
    fn default() -> SystemFonts {
        SystemFonts::new()
    }
}

impl FontSource for SystemFonts {
    fn select(&mut self, family: &str, attrs: FontAttrs) -> Option<FontBlob> {
        let lower = family.trim().to_lowercase();
        let fam = match Self::generic(&lower) {
            Some(g) => QueryFamily::Generic(g),
            None => QueryFamily::Named(family.trim()),
        };
        let mut query = self.collection.query(&mut self.cache);
        query.set_families([fam]);
        query.set_attributes(Self::attributes(attrs));
        let mut out = None;
        query.matches_with(|font| {
            out = Some(Self::blob(font));
            QueryStatus::Stop
        });
        out
    }

    fn fallback(
        &mut self,
        cluster: &str,
        script: ScriptTag,
        attrs: FontAttrs,
        emoji: bool,
    ) -> Vec<FontBlob> {
        let mut out: Vec<FontBlob> = Vec::new();
        let Some(first) = cluster.chars().find(|&c| !ignorable(c)) else {
            return out;
        };
        // Emoji presentation: the emoji family first, as Parley did. Then
        // the platform's fallback list for the script, then the last
        // resort in a stable order; each step only while no face covers
        // the whole cluster.
        if emoji
            && self.collect(
                [QueryFamily::Generic(GenericFamily::Emoji)],
                None,
                first,
                cluster,
                attrs,
                &mut out,
            )
        {
            return out;
        }
        let key = FallbackKey::new(Script::from_bytes(script.0), None);
        if self.collect(
            std::iter::empty(),
            Some(key),
            first,
            cluster,
            attrs,
            &mut out,
        ) {
            return out;
        }
        for name in self.last_resort_order() {
            if self.collect(
                [QueryFamily::Named(&name)],
                None,
                first,
                cluster,
                attrs,
                &mut out,
            ) {
                break;
            }
        }
        out
    }
}

/// Makes system fonts the default source of every new text engine.
/// Idempotent.
pub fn install() {
    craie_text::fonts::set_default_source(|| Box::new(SystemFonts::new()));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use craie_text::TextEngine;
    use craie_text::fonts::{FontAttrs, FontSource, ScriptTag};
    use craie_text::paragraph::{Paragraph, SpanStyle, TextSpec, TextStyle};
    use fontique::{Blob, Collection, CollectionOptions, FallbackKey, GenericFamily, Script};

    use super::SystemFonts;

    macro_rules! font {
        ($name:literal) => {
            &include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/fonts/",
                $name
            ))[..]
        };
    }

    fn collection(files: &[&'static [u8]]) -> Collection {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        for &bytes in files {
            collection.register_fonts(Blob::new(Arc::new(bytes)), None);
        }
        collection
    }

    fn source(files: &[&'static [u8]]) -> SystemFonts {
        SystemFonts::with_collection(collection(files))
    }

    /// Lays `text` out on `source` (family `Noto Sans`) and returns, per
    /// glyph, its cluster byte, id, and the file its face comes from
    /// (index into `files`).
    fn drawn(source: SystemFonts, files: &[&'static [u8]], text: &str) -> Vec<(u32, u16, usize)> {
        let mut e = TextEngine::with_source(Box::new(source));
        let spans = [SpanStyle {
            start: 0,
            style: TextStyle {
                size: 16.0,
                ..TextStyle::default()
            },
        }];
        let p: Paragraph = e.layout_text(
            &TextSpec {
                text,
                family: "Noto Sans",
                spans: &spans,
            },
            None,
        );
        let mut out = Vec::new();
        for r in &p.runs {
            let store = &e.fonts.store;
            let face = store.face_data(store.instance_data(r.font).face);
            let ptr = face.bytes.as_ref().as_ref().as_ptr();
            let file = files
                .iter()
                .position(|f| f.as_ptr() == ptr)
                .unwrap_or(usize::MAX);
            for g in &p.glyphs[r.glyphs.start as usize..r.glyphs.end as usize] {
                out.push((g.cluster, g.id, file));
            }
        }
        out
    }

    /// A face covering the whole cluster wins over faces covering only
    /// its base, wherever it sits in the order: here Noto Sans covers the
    /// dotted circle but not the Hebrew point, and the complete face
    /// (Noto Sans Hebrew) comes later in the last resort, and later still
    /// after a script fallback (Symbols 2) that also lacks the point.
    #[test]
    fn fallback_continues_to_a_face_covering_the_cluster() {
        let files = [
            font!("NotoSans-Regular.ttf"),
            font!("NotoSansHebrew-Regular.ttf"),
            font!("NotoSansSymbols2-Regular.ttf"),
        ];
        let text = "x\u{25CC}\u{05B0}";
        let want = |d: &[(u32, u16, usize)]| {
            assert!(d.iter().all(|&(_, g, _)| g != 0), "{d:?}");
            let marked: Vec<usize> = d.iter().filter(|g| g.0 == 1).map(|g| g.2).collect();
            assert_eq!(marked, [1, 1], "{d:?}");
        };
        want(&drawn(source(&files[..2]), &files, text));
        let mut c = collection(&files);
        let sym = c.family_id("Noto Sans Symbols 2").unwrap();
        c.set_fallbacks(
            FallbackKey::new(Script::from_bytes(*b"Latn"), None),
            [sym].into_iter(),
        );
        want(&drawn(SystemFonts::with_collection(c), &files, text));
    }

    /// Emoji dispatch: a heart with default text presentation, and with
    /// VS15, takes the script's text face (Symbols 2); with VS16 it takes
    /// the emoji family (Noto Emoji). Both faces cover U+2764.
    #[test]
    fn emoji_presentation_picks_the_emoji_family() {
        let files = [
            font!("NotoSans-Regular.ttf"),
            font!("NotoSansSymbols2-Regular.ttf"),
            font!("NotoEmoji-Subset-Regular.ttf"),
        ];
        let mut c = collection(&files);
        let sym = c.family_id("Noto Sans Symbols 2").unwrap();
        let emoji = c.family_id("Noto Emoji").unwrap();
        c.set_fallbacks(
            FallbackKey::new(Script::from_bytes(*b"Latn"), None),
            [sym].into_iter(),
        );
        c.set_generic_families(GenericFamily::Emoji, [emoji].into_iter());
        let text = "a\u{2764} \u{2764}\u{FE0F} \u{2764}\u{FE0E}";
        let d = drawn(SystemFonts::with_collection(c), &files, text);
        let file_at = |byte: u32| d.iter().find(|g| g.0 == byte).unwrap().2;
        assert_eq!(file_at(1), 1, "default text: {d:?}");
        assert_eq!(file_at(5), 2, "VS16: {d:?}");
        assert_eq!(file_at(12), 1, "VS15: {d:?}");
    }

    /// The last-resort choice does not depend on registration (and so
    /// enumeration) order: with no script fallbacks configured, U+25CC,
    /// which five of these families cover, resolves to the same family
    /// both ways.
    #[test]
    fn last_resort_is_independent_of_enumeration_order() {
        let files = [
            font!("NotoSansSymbols2-Regular.ttf"),
            font!("NotoSansHebrew-Regular.ttf"),
            font!("NotoSansArabic-Regular.ttf"),
            font!("NotoSans-Regular.ttf"),
            font!("NotoSansDevanagari-Regular.ttf"),
        ];
        let mut reversed = files;
        reversed.reverse();
        let pick = |files: &[&'static [u8]]| {
            let mut s = source(files);
            let out = s.fallback("\u{25CC}", ScriptTag::of('a'), FontAttrs::default(), false);
            assert_eq!(out.len(), 1);
            out[0].bytes.as_ref().as_ref().as_ptr() as usize
        };
        let a = pick(&files);
        assert_eq!(a, pick(&reversed));
        // By name: "Noto Sans" sorts first.
        assert_eq!(a, files[3].as_ptr() as usize);
    }
}

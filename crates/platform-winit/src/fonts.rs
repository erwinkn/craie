//! Desktop font discovery and fallback (ARCHITECTURE.md §6): the
//! platform side of `craie_text::fonts::FontSource`, over fontique.
//! fontique is reachable only through this adapter.

use std::sync::Arc;

use craie_text::fonts::{FontAttrs, FontBlob, FontSource, ScriptTag, Synthesis};
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

    /// Up to `max` faces of `families` (in order) covering `ch`.
    fn covering<'a>(
        &mut self,
        families: impl IntoIterator<Item = QueryFamily<'a>>,
        fallback: Option<FallbackKey>,
        ch: char,
        attrs: FontAttrs,
        max: usize,
        out: &mut Vec<FontBlob>,
    ) {
        let mut query = self.collection.query(&mut self.cache);
        query.set_families(families);
        query.set_attributes(Self::attributes(attrs));
        if let Some(key) = fallback {
            query.set_fallbacks(key);
        }
        query.matches_with(|font| {
            if Self::covers(font, ch)
                && !out
                    .iter()
                    .any(|b| b.id == font.blob.id() && b.index == font.index)
            {
                out.push(Self::blob(font));
                if out.len() >= max {
                    return QueryStatus::Stop;
                }
            }
            QueryStatus::Continue
        });
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
        ch: char,
        script: ScriptTag,
        attrs: FontAttrs,
        emoji: bool,
    ) -> Vec<FontBlob> {
        let mut out: Vec<FontBlob> = Vec::new();
        // Emoji presentation: the emoji family first, as Parley did.
        if emoji {
            self.covering(
                [QueryFamily::Generic(GenericFamily::Emoji)],
                None,
                ch,
                attrs,
                2,
                &mut out,
            );
        }
        // The platform's fallback list for the script (an ordered list).
        let key = FallbackKey::new(Script::from_bytes(script.0), None);
        self.covering(std::iter::empty(), Some(key), ch, attrs, 4, &mut out);
        if !out.is_empty() {
            return out;
        }
        // Last resort, once per character (the engine caches the answer):
        // the first family in `last_resort_order` that covers it.
        for name in self.last_resort_order() {
            self.covering([QueryFamily::Named(&name)], None, ch, attrs, 1, &mut out);
            if !out.is_empty() {
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

    use craie_text::fonts::{FontAttrs, FontSource, ScriptTag};
    use fontique::{Blob, Collection, CollectionOptions};

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

    fn source(files: &[&'static [u8]]) -> SystemFonts {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        for &bytes in files {
            collection.register_fonts(Blob::new(Arc::new(bytes)), None);
        }
        SystemFonts::with_collection(collection)
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
            let out = s.fallback('\u{25CC}', ScriptTag::of('a'), FontAttrs::default(), false);
            assert_eq!(out.len(), 1);
            out[0].bytes.as_ref().as_ref().as_ptr() as usize
        };
        let a = pick(&files);
        assert_eq!(a, pick(&reversed));
        // By name: "Noto Sans" sorts first.
        assert_eq!(a, files[3].as_ptr() as usize);
    }
}

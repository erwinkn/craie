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

impl SystemFonts {
    pub fn new() -> SystemFonts {
        SystemFonts {
            collection: Collection::new(CollectionOptions {
                shared: true,
                system_fonts: true,
            }),
            cache: SourceCache::default(),
        }
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

    fn fallback(&mut self, ch: char, script: ScriptTag, attrs: FontAttrs) -> Vec<FontBlob> {
        let mut out: Vec<FontBlob> = Vec::new();
        {
            let mut query = self.collection.query(&mut self.cache);
            query.set_families(std::iter::empty::<QueryFamily<'_>>());
            query.set_attributes(Self::attributes(attrs));
            query.set_fallbacks(FallbackKey::new(Script::from_bytes(script.0), None));
            query.matches_with(|font| {
                if Self::covers(font, ch) {
                    out.push(Self::blob(font));
                    if out.len() >= 4 {
                        return QueryStatus::Stop;
                    }
                }
                QueryStatus::Continue
            });
        }
        if !out.is_empty() {
            return out;
        }
        // Last resort, once per character (the engine caches the answer):
        // the first family that covers it.
        let names: Vec<String> = self.collection.family_names().map(str::to_string).collect();
        for name in names {
            let mut query = self.collection.query(&mut self.cache);
            query.set_families([QueryFamily::Named(&name)]);
            query.set_attributes(Self::attributes(attrs));
            let mut hit = None;
            query.matches_with(|font| {
                if Self::covers(font, ch) {
                    hit = Some(Self::blob(font));
                }
                QueryStatus::Stop
            });
            if let Some(b) = hit {
                out.push(b);
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

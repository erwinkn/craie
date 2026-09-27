//! Claims (ARCHITECTURE-update topics 1 and 2): a node declares the
//! discrete events it handles itself, and native skips its own default
//! for them and sends one `CLAIM` event instead.
//!
//! A claim set belongs to a node, or to the window (the window list, id
//! NIL: `useHotkeys`). The facade sends a node's set in a `CLAIMS` op
//! whenever it changes, with a new version; the `CLAIM` event names the
//! version it matched, so JS runs the handler of the declaration that was
//! on screen when the user pressed.
//!
//! A key press is claimed by the first matching claim on the focused node,
//! then its ancestors, then the window list. Nothing matches while an IME
//! composes. The window list does not match while a text input has focus,
//! unless the claim allows it (`chord_flag::IN_INPUT`). Paste, copy and
//! cut are claimed on the focus path (copy and cut on the text selection's
//! domain when nothing has focus); drop and context menu on the path under
//! the pointer, or the focus path for the context-menu keys.

use crate::events::{Key, KeyInput};

/// What a claim claims (bits 0 to 7 of a `CLAIM` event's key).
pub mod claim_kind {
    /// A key chord: key down on it.
    pub const KEY: u8 = 1;
    pub const PASTE: u8 = 2;
    pub const COPY: u8 = 3;
    pub const CUT: u8 = 4;
    /// Files dropped on the node.
    pub const DROP: u8 = 5;
    /// A secondary press, or the ContextMenu key or Shift+F10.
    pub const CONTEXT_MENU: u8 = 6;
}

/// Key claim flags.
pub mod chord_flag {
    /// `key` is a named key's code (`Key::code`), not a character.
    pub const NAMED: u8 = 1 << 0;
    /// The platform's auto-repeat of the key is swallowed, not claimed
    /// again (`repeat: false`).
    pub const NO_REPEAT: u8 = 1 << 1;
    /// A window-list claim that matches while a text input has focus
    /// (`allowInInput`).
    pub const IN_INPUT: u8 = 1 << 2;
    pub const ALL: u8 = NAMED | NO_REPEAT | IN_INPUT;
}

/// One claim. Key claims are a chord: exact modifiers (`Mods` bits; the
/// facade resolves `mod`) plus a named key or a lower-case character.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Claim {
    pub kind: u8,
    pub flags: u8,
    pub mods: u8,
    /// `Key::code` when `NAMED`, else the character's scalar value.
    pub key: u32,
}

impl Claim {
    /// A chord on a named key.
    pub fn named(mods: u8, key: Key) -> Claim {
        Claim {
            kind: claim_kind::KEY,
            flags: chord_flag::NAMED,
            mods,
            key: key.code(),
        }
    }

    /// A chord on a character (lower case).
    pub fn char(mods: u8, c: char) -> Claim {
        Claim {
            kind: claim_kind::KEY,
            flags: 0,
            mods,
            key: c as u32,
        }
    }

    /// A claim of a non-key kind.
    pub fn of(kind: u8) -> Claim {
        Claim {
            kind,
            ..Claim::default()
        }
    }

    pub fn with(mut self, flags: u8) -> Claim {
        self.flags |= flags;
        self
    }

    /// Whether this key claim's chord describes `k`: the same modifiers
    /// exactly, and the same named key or chord character
    /// (`KeyInput::chord_char`).
    pub fn matches(&self, k: &KeyInput) -> bool {
        if self.kind != claim_kind::KEY || k.mods.bits() != self.mods {
            return false;
        }
        if self.flags & chord_flag::NAMED != 0 {
            k.key.code() == self.key
        } else {
            k.key == Key::Unknown && k.chord_char().map(|c| c as u32) == Some(self.key)
        }
    }

    /// Whether the claim is well formed: a known kind and flags, and a
    /// real key for a chord.
    pub fn valid(&self) -> bool {
        match self.kind {
            claim_kind::KEY => {
                self.flags & !chord_flag::ALL == 0
                    && self.mods < 16
                    && if self.flags & chord_flag::NAMED != 0 {
                        Key::from_code(self.key).is_some_and(|k| k != Key::Unknown)
                    } else {
                        char::from_u32(self.key).is_some_and(|c| c != '\0')
                    }
            }
            claim_kind::PASTE..=claim_kind::CONTEXT_MENU => {
                self.flags == 0 && self.mods == 0 && self.key == 0
            }
            _ => false,
        }
    }
}

/// A node's claims, in the order the facade declared them (the first
/// match wins), and the version JS knows them by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaimSet {
    pub version: u32,
    pub claims: Vec<Claim>,
}

/// What a key press found in a claim set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMatch {
    /// Claim `index`: send its event.
    Claim(usize),
    /// The auto-repeat of a `NO_REPEAT` claim: swallowed, no event.
    Swallow,
}

impl ClaimSet {
    /// The first key claim matching `k`. `in_input`: a text input has
    /// focus, so only `IN_INPUT` claims match (the window list).
    pub fn key(&self, k: &KeyInput, in_input: bool) -> Option<KeyMatch> {
        let (i, c) =
            self.claims.iter().enumerate().find(|(_, c)| {
                c.matches(k) && (!in_input || c.flags & chord_flag::IN_INPUT != 0)
            })?;
        Some(if k.repeat && c.flags & chord_flag::NO_REPEAT != 0 {
            KeyMatch::Swallow
        } else {
            KeyMatch::Claim(i)
        })
    }

    /// The index of the claim of `kind` (not a key), if any.
    pub fn find(&self, kind: u8) -> Option<usize> {
        self.claims.iter().position(|c| c.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Mods;

    fn press(char: &str, code: char, mods: u8) -> KeyInput {
        KeyInput {
            char: Some(char.into()),
            code: Some(code),
            mods: Mods::from_bits(mods),
            ..KeyInput::default()
        }
    }

    fn named(key: Key, mods: u8) -> KeyInput {
        KeyInput {
            key,
            mods: Mods::from_bits(mods),
            ..KeyInput::default()
        }
    }

    #[test]
    fn modifiers_match_exactly() {
        let save = Claim::char(Mods::COMMAND, 's');
        assert!(save.matches(&press("s", 's', Mods::COMMAND)));
        assert!(!save.matches(&press("s", 's', Mods::COMMAND | Mods::SHIFT)));
        assert!(!save.matches(&press("s", 's', Mods::CTRL | Mods::META)));
        assert!(!save.matches(&press("s", 's', 0)));
        let enter = Claim::named(0, Key::Enter);
        assert!(enter.matches(&named(Key::Enter, 0)));
        assert!(
            !enter.matches(&named(Key::Enter, Mods::SHIFT)),
            "enter is not shift+enter"
        );
        let back = Claim::named(Mods::SHIFT, Key::Tab);
        assert!(back.matches(&named(Key::Tab, Mods::SHIFT)));
        assert!(!back.matches(&named(Key::Tab, 0)));
    }

    #[test]
    fn chords_match_the_shifted_character() {
        // Shift+O gives "O"; chords are lower case.
        let c = Claim::char(Mods::COMMAND | Mods::SHIFT, 'o');
        assert!(c.matches(&press("O", 'o', Mods::COMMAND | Mods::SHIFT)));
        // Shift+/ gives "?", as on the web: the chord is `shift+?`.
        assert!(Claim::char(Mods::SHIFT, '?').matches(&press("?", '/', Mods::SHIFT)));
        assert!(!Claim::char(Mods::SHIFT, '/').matches(&press("?", '/', Mods::SHIFT)));
        // A named key never matches a character chord.
        assert!(!Claim::char(0, ' ').matches(&named(Key::Space, 0)));
    }

    #[test]
    fn non_latin_layouts_and_alt_match_the_physical_key() {
        // Cyrillic: the C position gives "с" (U+0441).
        let copy = Claim::char(Mods::COMMAND, 'c');
        assert!(copy.matches(&press("с", 'c', Mods::COMMAND)));
        assert!(!Claim::char(Mods::COMMAND, 'с').matches(&press("с", 'c', Mods::COMMAND)));
        // Option+I on macOS gives "ˆ"; the chord is alt+i.
        assert!(Claim::char(Mods::ALT, 'i').matches(&press("ˆ", 'i', Mods::ALT)));
        // A Latin layout keeps its own letters: AZERTY's A position.
        assert!(Claim::char(Mods::COMMAND, 'a').matches(&press("a", 'q', Mods::COMMAND)));
        assert!(!Claim::char(Mods::COMMAND, 'q').matches(&press("a", 'q', Mods::COMMAND)));
        // Accented Latin letters are not a fallback: "é" stays "é".
        assert!(Claim::char(0, 'é').matches(&press("é", '2', 0)));
    }

    #[test]
    fn repeats_are_claimed_unless_the_claim_says_not() {
        let set = ClaimSet {
            version: 3,
            claims: vec![
                Claim::named(0, Key::Down),
                Claim::char(Mods::COMMAND, 'n').with(chord_flag::NO_REPEAT),
            ],
        };
        let mut down = named(Key::Down, 0);
        down.repeat = true;
        assert_eq!(set.key(&down, false), Some(KeyMatch::Claim(0)));
        let mut new = press("n", 'n', Mods::COMMAND);
        assert_eq!(set.key(&new, false), Some(KeyMatch::Claim(1)));
        new.repeat = true;
        assert_eq!(set.key(&new, false), Some(KeyMatch::Swallow));
    }

    #[test]
    fn in_input_filters_to_allowed_claims() {
        let set = ClaimSet {
            version: 1,
            claims: vec![
                Claim::char(0, '/'),
                Claim::char(Mods::COMMAND, 'k').with(chord_flag::IN_INPUT),
            ],
        };
        assert_eq!(set.key(&press("/", '/', 0), true), None);
        assert_eq!(
            set.key(&press("/", '/', 0), false),
            Some(KeyMatch::Claim(0))
        );
        assert_eq!(
            set.key(&press("k", 'k', Mods::COMMAND), true),
            Some(KeyMatch::Claim(1))
        );
    }

    #[test]
    fn validity() {
        assert!(Claim::named(0, Key::F(12)).valid());
        assert!(Claim::of(claim_kind::PASTE).valid());
        assert!(!Claim::of(9).valid());
        assert!(
            !Claim {
                kind: claim_kind::KEY,
                flags: chord_flag::NAMED,
                mods: 0,
                key: 200
            }
            .valid()
        );
        assert!(!Claim::named(0, Key::Unknown).valid());
        assert!(!Claim::char(0, 'a').with(1 << 5).valid());
        assert!(!Claim::char(16, 'a').valid());
    }
}

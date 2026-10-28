//! Turning key presses into bytes for the program: classic xterm sequences,
//! xterm's modifyOtherKeys, and the Kitty keyboard protocol. The last two let
//! programs tell apart keys that classic encoding sends identically, such as
//! Shift+Enter and Enter, or Ctrl+I and Tab.

use gtk::gdk;

/// How the program asked for keys to be encoded.
#[derive(Clone, Copy, Debug, Default)]
pub struct Encoding {
    /// Cursor keys in application mode (DECCKM).
    pub app_cursor: bool,
    /// Kitty keyboard protocol flags.
    pub kitty_flags: u8,
    /// xterm modifyOtherKeys level.
    pub modify_other_keys: u8,
    /// The modifier that makes Left and Right jump a word, sent as Meta+B and
    /// Meta+F, which shells and readline understand without configuration.
    /// None leaves the keys alone, for full-screen programs.
    pub word_jumps: Option<WordJumps>,
}

/// The platform's word-jump modifier: Option on a Mac, Ctrl elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordJumps {
    Alt,
    Ctrl,
}

impl WordJumps {
    pub fn for_platform() -> Self {
        if crate::platform::current().cmd_shortcuts() {
            Self::Alt
        } else {
            Self::Ctrl
        }
    }
}

const DISAMBIGUATE: u8 = 1;
const ALL_KEYS: u8 = 8;

/// The classic Ctrl+key byte, if the key has one.
fn legacy_ctrl(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// The bytes for a key press. `base` is the character the key produces
/// without modifiers (`a` for Shift+A, `1` for Shift+!), when known.
pub fn encode(
    key: gdk::Key,
    state: gdk::ModifierType,
    base: Option<char>,
    encoding: Encoding,
) -> Option<Vec<u8>> {
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    let alt = state.contains(gdk::ModifierType::ALT_MASK);
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let super_key = state.contains(gdk::ModifierType::SUPER_MASK);
    let bits = shift as u8 | (alt as u8) << 1 | (ctrl as u8) << 2 | (super_key as u8) << 3;
    // The modifier parameter shared by all encodings: 1 + modifier bits.
    let modifier = bits + 1;
    let all_keys = encoding.kitty_flags & ALL_KEYS != 0;
    let disambiguate = encoding.kitty_flags & DISAMBIGUATE != 0 || all_keys;
    let other_keys = encoding.modify_other_keys;

    let kitty = |code: u32| {
        Some(String::into_bytes(if bits == 0 {
            format!("\x1b[{code}u")
        } else {
            format!("\x1b[{code};{modifier}u")
        }))
    };
    let modify_other = |code: u32| Some(format!("\x1b[27;{modifier};{code}~").into_bytes());
    let legacy = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        Some(out)
    };
    let cursor = |letter: char| {
        Some(String::into_bytes(if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}")
        } else if encoding.app_cursor {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[{letter}")
        }))
    };
    let tilde = |code: u8| {
        Some(String::into_bytes(if modifier > 1 {
            format!("\x1b[{code};{modifier}~")
        } else {
            format!("\x1b[{code}~")
        }))
    };
    let function = |letter: char| {
        Some(String::into_bytes(if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}")
        } else {
            format!("\x1bO{letter}")
        }))
    };
    // Enter, Tab, Backspace and Escape: classic encoding loses modifiers.
    let special = |code: u32, classic: &[u8]| {
        if all_keys || (disambiguate && (bits != 0 || code == 27)) {
            kitty(code)
        } else if other_keys > 0 && (shift || ctrl || super_key) {
            modify_other(code)
        } else {
            legacy(classic)
        }
    };

    // Programs that asked for exact keys get them.
    let word = |letter: u8| match encoding.word_jumps {
        Some(WordJumps::Alt) if bits == 2 && !disambiguate => Some(vec![0x1b, letter]),
        Some(WordJumps::Ctrl) if bits == 4 && !disambiguate => Some(vec![0x1b, letter]),
        _ => None,
    };

    use gdk::Key;
    match key {
        Key::Return | Key::KP_Enter | Key::ISO_Enter => special(13, b"\r"),
        Key::BackSpace => special(127, if ctrl { b"\x08" } else { b"\x7f" }),
        Key::Tab | Key::KP_Tab | Key::ISO_Left_Tab => {
            if all_keys || (disambiguate && bits != 0) {
                kitty(9)
            } else if shift && !ctrl && !alt {
                Some(b"\x1b[Z".to_vec())
            } else {
                special(9, b"\t")
            }
        }
        Key::Escape => special(27, b"\x1b"),
        Key::Up | Key::KP_Up => cursor('A'),
        Key::Down | Key::KP_Down => cursor('B'),
        Key::Right | Key::KP_Right => word(b'f').or_else(|| cursor('C')),
        Key::Left | Key::KP_Left => word(b'b').or_else(|| cursor('D')),
        Key::Home | Key::KP_Home => cursor('H'),
        Key::End | Key::KP_End => cursor('F'),
        Key::Insert | Key::KP_Insert => tilde(2),
        Key::Delete | Key::KP_Delete => tilde(3),
        Key::Page_Up | Key::KP_Page_Up => tilde(5),
        Key::Page_Down | Key::KP_Page_Down => tilde(6),
        Key::F1 => function('P'),
        Key::F2 => function('Q'),
        // CSI R would look like a cursor position report, so the Kitty
        // protocol sends F3 as CSI 13 ~.
        Key::F3 if disambiguate => tilde(13),
        Key::F3 => function('R'),
        Key::F4 => function('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ => {
            let c = key.to_unicode().filter(|c| !c.is_control())?;
            let base = base.unwrap_or_else(|| c.to_ascii_lowercase());
            if all_keys || (disambiguate && (ctrl || alt || super_key)) {
                return kitty(base as u32);
            }
            if ctrl {
                let ambiguous = shift || legacy_ctrl(c).is_none();
                if other_keys == 2 || (other_keys == 1 && ambiguous) {
                    return modify_other(c as u32);
                }
                return match legacy_ctrl(c) {
                    Some(byte) => legacy(&[byte]),
                    None => legacy(c.to_string().as_bytes()),
                };
            }
            if alt && other_keys == 2 {
                return modify_other(c as u32);
            }
            legacy(c.to_string().as_bytes())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: gdk::ModifierType = gdk::ModifierType::empty();
    const SHIFT: gdk::ModifierType = gdk::ModifierType::SHIFT_MASK;
    const CTRL: gdk::ModifierType = gdk::ModifierType::CONTROL_MASK;
    const ALT: gdk::ModifierType = gdk::ModifierType::ALT_MASK;

    fn key(key: gdk::Key, state: gdk::ModifierType, encoding: Encoding) -> String {
        String::from_utf8(encode(key, state, None, encoding).unwrap()).unwrap()
    }

    #[test]
    fn classic_xterm() {
        let classic = Encoding::default();
        assert_eq!(key(gdk::Key::c, CTRL, classic), "\x03");
        assert_eq!(key(gdk::Key::Up, NONE, classic), "\x1b[A");
        let app = Encoding {
            app_cursor: true,
            ..classic
        };
        assert_eq!(key(gdk::Key::Up, NONE, app), "\x1bOA");
        assert_eq!(key(gdk::Key::Right, CTRL, classic), "\x1b[1;5C");
        assert_eq!(key(gdk::Key::b, ALT, classic), "\x1bb");
        assert_eq!(key(gdk::Key::Delete, NONE, classic), "\x1b[3~");
        assert_eq!(key(gdk::Key::BackSpace, NONE, classic), "\x7f");
        assert_eq!(
            key(gdk::Key::Return, SHIFT, classic),
            "\r",
            "classic loses Shift"
        );
        assert_eq!(key(gdk::Key::ISO_Left_Tab, SHIFT, classic), "\x1b[Z");
    }

    #[test]
    fn kitty_disambiguates() {
        let kitty = Encoding {
            kitty_flags: 1,
            ..Default::default()
        };
        assert_eq!(key(gdk::Key::Return, SHIFT, kitty), "\x1b[13;2u");
        assert_eq!(key(gdk::Key::Return, CTRL, kitty), "\x1b[13;5u");
        assert_eq!(key(gdk::Key::Return, NONE, kitty), "\r");
        assert_eq!(
            key(gdk::Key::i, CTRL, kitty),
            "\x1b[105;5u",
            "not the same as Tab"
        );
        assert_eq!(key(gdk::Key::Tab, NONE, kitty), "\t");
        assert_eq!(key(gdk::Key::ISO_Left_Tab, SHIFT, kitty), "\x1b[9;2u");
        assert_eq!(key(gdk::Key::Escape, NONE, kitty), "\x1b[27u");
        assert_eq!(key(gdk::Key::a, ALT, kitty), "\x1b[97;3u");
        assert_eq!(key(gdk::Key::Up, CTRL, kitty), "\x1b[1;5A");
        assert_eq!(key(gdk::Key::F3, NONE, kitty), "\x1b[13~");
        let all = Encoding {
            kitty_flags: 8,
            ..Default::default()
        };
        assert_eq!(key(gdk::Key::Return, NONE, all), "\x1b[13u");
    }

    #[test]
    fn modify_other_keys() {
        let level1 = Encoding {
            modify_other_keys: 1,
            ..Default::default()
        };
        assert_eq!(key(gdk::Key::Return, SHIFT, level1), "\x1b[27;2;13~");
        assert_eq!(
            key(gdk::Key::c, CTRL, level1),
            "\x03",
            "plain Ctrl+C stays classic"
        );
        assert_eq!(key(gdk::Key::period, CTRL, level1), "\x1b[27;5;46~");
        let level2 = Encoding {
            modify_other_keys: 2,
            ..Default::default()
        };
        assert_eq!(key(gdk::Key::c, CTRL, level2), "\x1b[27;5;99~");
    }

    #[test]
    fn word_jumps_send_meta_b_and_f() {
        let mac = Encoding {
            word_jumps: Some(WordJumps::Alt),
            ..Default::default()
        };
        assert_eq!(key(gdk::Key::Left, ALT, mac), "\x1bb");
        assert_eq!(key(gdk::Key::Right, ALT, mac), "\x1bf");
        assert_eq!(key(gdk::Key::Left, CTRL, mac), "\x1b[1;5D");
        assert_eq!(key(gdk::Key::Left, ALT | SHIFT, mac), "\x1b[1;4D");
        let linux = Encoding {
            word_jumps: Some(WordJumps::Ctrl),
            ..Default::default()
        };
        assert_eq!(key(gdk::Key::Left, CTRL, linux), "\x1bb");
        assert_eq!(key(gdk::Key::Right, CTRL, linux), "\x1bf");
        assert_eq!(key(gdk::Key::Left, ALT, linux), "\x1b[1;3D");
        let kitty = Encoding {
            kitty_flags: 1,
            ..linux
        };
        assert_eq!(
            key(gdk::Key::Left, CTRL, kitty),
            "\x1b[1;5D",
            "asked for exact keys"
        );
        assert_eq!(
            key(gdk::Key::Left, CTRL, Encoding::default()),
            "\x1b[1;5D",
            "full-screen programs"
        );
    }
}

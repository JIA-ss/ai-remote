//! Key names and chords for the `keyboard` tool (and mouse modifiers), and their mapping to enigo.
//!
//! Parsing is pure and platform-independent so it is testable anywhere and fails before anything
//! is injected. Only [`resolve`] (and [`resolve_chord`] through it) touches the OS: on Windows,
//! punctuation is looked up on the foreground window's keyboard layout, because enigo's own lookup
//! (`Key::Unicode`) sends scancode 0 for every shifted character and silently falls back to typing
//! text for unmapped ones. The refused chords are judged after that lookup ([`forbidden`]).

use std::fmt;

use anyhow::{Result, bail};
use enigo::Key;

/// Most chords one call may contain.
pub const MAX_CHORDS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    /// Windows key on Windows, Command on macOS, Super elsewhere.
    Meta,
}

impl Modifier {
    pub fn key(self) -> Key {
        match self {
            Modifier::Ctrl => Key::Control,
            Modifier::Alt => Key::Alt,
            Modifier::Shift => Key::Shift,
            Modifier::Meta => Key::Meta,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Modifier::Ctrl => "ctrl",
            Modifier::Alt => "alt",
            Modifier::Shift => "shift",
            Modifier::Meta if cfg!(windows) => "win",
            Modifier::Meta if cfg!(target_os = "macos") => "cmd",
            Modifier::Meta => "super",
        }
    }
}

/// One key, independent of platform and keyboard layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyName {
    Modifier(Modifier),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Space,
    CapsLock,
    PrintScreen,
    Pause,
    NumLock,
    ScrollLock,
    /// The context-menu key.
    Menu,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MediaPlayPause,
    MediaNext,
    MediaPrev,
    MediaStop,
    /// F1..=F24.
    F(u8),
    /// A character key: a lower-case letter, a digit or punctuation.
    Char(char),
}

/// Names a caller may use; the first name of each key is the canonical one. Matching ignores case,
/// `_`, `-` and spaces, so "Page_Down", "page-down" and "PageDown" are the same key.
const NAMES: &[(&str, KeyName)] = {
    use KeyName as K;
    use Modifier as M;
    &[
        ("ctrl", K::Modifier(M::Ctrl)),
        ("control", K::Modifier(M::Ctrl)),
        ("shift", K::Modifier(M::Shift)),
        ("alt", K::Modifier(M::Alt)),
        ("option", K::Modifier(M::Alt)),
        ("opt", K::Modifier(M::Alt)),
        ("win", K::Modifier(M::Meta)),
        ("windows", K::Modifier(M::Meta)),
        ("super", K::Modifier(M::Meta)),
        ("meta", K::Modifier(M::Meta)),
        ("cmd", K::Modifier(M::Meta)),
        ("command", K::Modifier(M::Meta)),
        ("enter", K::Enter),
        ("return", K::Enter),
        ("kpenter", K::Enter),
        ("tab", K::Tab),
        ("esc", K::Escape),
        ("escape", K::Escape),
        ("backspace", K::Backspace),
        ("delete", K::Delete),
        ("del", K::Delete),
        ("insert", K::Insert),
        ("ins", K::Insert),
        ("home", K::Home),
        ("end", K::End),
        ("pageup", K::PageUp),
        ("pgup", K::PageUp),
        ("prior", K::PageUp),
        ("pagedown", K::PageDown),
        ("pgdn", K::PageDown),
        ("next", K::PageDown),
        ("up", K::Up),
        ("arrowup", K::Up),
        ("uparrow", K::Up),
        ("down", K::Down),
        ("arrowdown", K::Down),
        ("downarrow", K::Down),
        ("left", K::Left),
        ("arrowleft", K::Left),
        ("leftarrow", K::Left),
        ("right", K::Right),
        ("arrowright", K::Right),
        ("rightarrow", K::Right),
        ("space", K::Space),
        ("spacebar", K::Space),
        ("capslock", K::CapsLock),
        ("printscreen", K::PrintScreen),
        ("prtsc", K::PrintScreen),
        ("print", K::PrintScreen),
        ("pause", K::Pause),
        ("numlock", K::NumLock),
        ("scrolllock", K::ScrollLock),
        ("menu", K::Menu),
        ("apps", K::Menu),
        ("contextmenu", K::Menu),
        ("volumeup", K::VolumeUp),
        ("volumedown", K::VolumeDown),
        ("volumemute", K::VolumeMute),
        ("mute", K::VolumeMute),
        ("playpause", K::MediaPlayPause),
        ("mediaplaypause", K::MediaPlayPause),
        ("nexttrack", K::MediaNext),
        ("medianext", K::MediaNext),
        ("prevtrack", K::MediaPrev),
        ("mediaprev", K::MediaPrev),
        ("mediastop", K::MediaStop),
        ("plus", K::Char('+')),
        ("minus", K::Char('-')),
        ("equal", K::Char('=')),
        ("equals", K::Char('=')),
        ("comma", K::Char(',')),
        ("period", K::Char('.')),
        ("dot", K::Char('.')),
        ("slash", K::Char('/')),
        ("backslash", K::Char('\\')),
        ("semicolon", K::Char(';')),
        ("quote", K::Char('\'')),
        ("apostrophe", K::Char('\'')),
        ("grave", K::Char('`')),
        ("backtick", K::Char('`')),
        ("bracketleft", K::Char('[')),
        ("bracketright", K::Char(']')),
    ]
};

impl fmt::Display for KeyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyName::Modifier(m) => f.write_str(m.name()),
            KeyName::F(n) => write!(f, "f{n}"),
            KeyName::Char(c) => write!(f, "{c}"),
            other => {
                let name = NAMES.iter().find(|(_, k)| k == other).map_or("?", |(n, _)| n);
                f.write_str(name)
            }
        }
    }
}

/// Modifiers held while one key is tapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Deduplicated, in press order (ctrl, alt, shift, meta).
    pub modifiers: Vec<Modifier>,
    pub key: KeyName,
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for m in &self.modifiers {
            write!(f, "{}+", m.name())?;
        }
        write!(f, "{}", self.key)
    }
}

/// Parses one key name (case-insensitive) or a single character.
pub fn parse_key_name(raw: &str) -> Result<KeyName> {
    let mut chars = raw.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_control() || c.is_whitespace() {
            bail!("{c:?} is not a key name; use names such as enter, tab or space");
        }
        // A letter means the key, not an upper-case character: shift must be explicit.
        let lower = c.to_lowercase().next().filter(|_| c.to_lowercase().count() == 1).unwrap_or(c);
        return Ok(KeyName::Char(lower));
    }
    let norm: String = raw.chars().filter(|c| !matches!(c, '_' | '-' | ' ')).flat_map(char::to_lowercase).collect();
    if let Some(&(_, key)) = NAMES.iter().find(|(name, _)| *name == norm) {
        return Ok(key);
    }
    if let Some(n) = norm.strip_prefix('f').filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
        return match n.parse::<u8>() {
            Ok(n @ 1..=24) => Ok(KeyName::F(n)),
            _ => bail!("unknown key '{raw}': function keys go from f1 to f24"),
        };
    }
    bail!("unknown key '{raw}'. Use a single character (a, 7, /) or one of: {}", known_names())
}

fn known_names() -> String {
    let mut seen: Vec<KeyName> = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for (name, key) in NAMES {
        if !seen.contains(key) {
            seen.push(*key);
            names.push(name);
        }
    }
    names.push("f1-f24");
    names.join(", ")
}

/// Splits a chord on `+`, treating a trailing `++` (or a lone `+`) as the plus key.
fn chord_parts(token: &str) -> Result<Vec<&str>> {
    if token == "+" {
        return Ok(vec!["+"]);
    }
    let (body, plus_key) = match token.strip_suffix("++") {
        Some(body) => (body, true),
        None => (token, false),
    };
    let mut parts: Vec<&str> = if body.is_empty() { Vec::new() } else { body.split('+').collect() };
    if plus_key {
        parts.push("+");
    }
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        bail!("'{token}' is not a valid chord: join keys with '+' (ctrl+shift+t) and separate chords with spaces");
    }
    Ok(parts)
}

/// Parses a chord such as `ctrl+shift+t`: every part but the last must be a modifier.
pub fn parse_chord(token: &str) -> Result<Chord> {
    let parts = chord_parts(token)?;
    let (last, rest) = parts.split_last().expect("chord_parts never returns an empty list");
    let mut modifiers = Vec::with_capacity(rest.len());
    for part in rest {
        match parse_key_name(part)? {
            KeyName::Modifier(m) => modifiers.push(m),
            _ => bail!(
                "'{part}' in '{token}' is not a modifier: in a chord every key but the last must be ctrl, shift, \
                 alt or win/cmd. To press keys one after another, separate them with spaces"
            ),
        }
    }
    let key = parse_key_name(last)?;
    if let KeyName::Modifier(k) = key {
        modifiers.retain(|m| *m != k);
    }
    modifiers.sort();
    modifiers.dedup();
    Ok(Chord { modifiers, key })
}

/// Parses a space-separated sequence of chords (`"Down Down Return"`, `"ctrl+a ctrl+c"`).
pub fn parse_keys(spec: &str) -> Result<Vec<Chord>> {
    let tokens: Vec<&str> = spec.split_whitespace().collect();
    if tokens.is_empty() {
        bail!("keys is empty: give a chord such as \"ctrl+l\" or a sequence such as \"Down Down Return\"");
    }
    if tokens.len() > MAX_CHORDS {
        bail!("keys has {} chords; at most {MAX_CHORDS} per call", tokens.len());
    }
    tokens.into_iter().map(parse_chord).collect()
}

/// Parses mouse modifier names.
pub fn parse_modifiers(names: &[String]) -> Result<Vec<Modifier>> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        match parse_key_name(name)? {
            KeyName::Modifier(m) => out.push(m),
            _ => bail!("modifier '{name}' is not one of ctrl, shift, alt (option) or win (super, meta, cmd)"),
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Mac,
    Other,
}

impl Os {
    pub const CURRENT: Os = if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Other
    };
}

/// A chord as it will actually be pressed: the key after the keyboard-layout lookup, and every
/// modifier held with it (the chord's own plus any the layout needs to produce that key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pressed {
    pub key: Key,
    /// Deduplicated, in press order (ctrl, alt, shift, meta).
    pub modifiers: Vec<Modifier>,
}

/// Resolves a parsed chord for this platform and the current keyboard layout.
pub fn resolve_chord(chord: &Chord) -> Result<Pressed> {
    let (key, layout) = resolve(chord.key)?;
    let mut modifiers = chord.modifiers.clone();
    modifiers.extend(layout);
    modifiers.sort();
    modifiers.dedup();
    Ok(Pressed { key, modifiers })
}

/// Why `pressed` is refused on `os`, if it is: combinations that lock the machine or sign the user
/// out would cut the controller off from the desktop (and are the owner's call, not an agent's).
/// Extra modifiers do not make them safe, so the check is "contains", not "equals". It runs on the
/// resolved chord, so a character that the layout types with the L key (AltGr+L makes 'ł' on some
/// layouts) cannot slip a forbidden chord past it.
pub fn forbidden(pressed: &Pressed, os: Os) -> Option<&'static str> {
    use Modifier as M;
    let has = |m| pressed.modifiers.contains(&m);
    let is = |named| is_key(pressed.key, named);
    match os {
        Os::Windows if has(M::Meta) && is(Named::L) => Some("it locks the workstation"),
        Os::Windows if has(M::Ctrl) && has(M::Alt) && (is(Named::Delete) || is(Named::End)) => {
            Some("it opens the secure attention screen (lock, sign out, switch user)")
        }
        Os::Mac if has(M::Ctrl) && has(M::Meta) && is(Named::Q) => Some("it locks the screen"),
        Os::Mac if has(M::Shift) && has(M::Meta) && is(Named::Q) => Some("it logs the user out"),
        _ => None,
    }
}

/// Whether `pressed` is the secure attention sequence (Ctrl+Alt+Del or Ctrl+Alt+End, which a
/// remote session maps to it): refused by [`forbidden`], but a host running as a Windows service
/// can send it through the service instead of pressing the keys.
pub fn is_secure_attention(pressed: &Pressed, os: Os) -> bool {
    use Modifier as M;
    let has = |m| pressed.modifiers.contains(&m);
    os == Os::Windows
        && has(M::Ctrl)
        && has(M::Alt)
        && (is_key(pressed.key, Named::Delete) || is_key(pressed.key, Named::End))
}

/// Keys the refusal rules name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    L,
    Q,
    Delete,
    End,
}

/// Whether `key` is `named`, however [`resolve`] (or a layout lookup) spells it.
fn is_key(key: Key, named: Named) -> bool {
    match key {
        Key::Unicode(c) => match named {
            Named::L => c.eq_ignore_ascii_case(&'l'),
            Named::Q => c.eq_ignore_ascii_case(&'q'),
            Named::Delete | Named::End => false,
        },
        Key::Delete => named == Named::Delete,
        Key::End => named == Named::End,
        Key::Other(code) => native_codes(named).contains(&code),
        #[cfg(windows)]
        Key::L => named == Named::L,
        #[cfg(windows)]
        Key::Q => named == Named::Q,
        _ => false,
    }
}

/// Native codes `Key::Other` carries for `named`: virtual keys on Windows, CGKeyCodes on macOS,
/// keysyms elsewhere.
fn native_codes(named: Named) -> &'static [u32] {
    #[cfg(windows)]
    return match named {
        Named::L => &[0x4c],
        Named::Q => &[0x51],
        // VK_DELETE, and VK_DECIMAL: the numpad key that is Del with NumLock off.
        Named::Delete => &[0x2e, 0x6e],
        Named::End => &[0x23],
    };
    #[cfg(target_os = "macos")]
    return match named {
        Named::L => &[0x25],
        Named::Q => &[0x0c],
        Named::Delete => &[0x75],
        Named::End => &[0x77],
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    return match named {
        Named::L => &[0x6c, 0x4c],
        Named::Q => &[0x71, 0x51],
        Named::Delete => &[0xffff, 0xff9f],
        Named::End => &[0xff57, 0xff9c],
    };
}

/// The enigo key for `name`, plus modifiers the current keyboard layout needs to produce it (for
/// example shift for `+` on a US layout).
pub fn resolve(name: KeyName) -> Result<(Key, Vec<Modifier>)> {
    if let KeyName::Char(c) = name {
        return resolve_char(c);
    }
    match named_key(name) {
        Some(key) => Ok((key, Vec::new())),
        None => bail!("the key '{name}' is not available on this platform"),
    }
}

fn named_key(name: KeyName) -> Option<Key> {
    use KeyName as K;
    Some(match name {
        K::Modifier(m) => m.key(),
        K::Enter => Key::Return,
        K::Tab => Key::Tab,
        K::Escape => Key::Escape,
        K::Backspace => Key::Backspace,
        K::Delete => Key::Delete,
        K::Home => Key::Home,
        K::End => Key::End,
        K::PageUp => Key::PageUp,
        K::PageDown => Key::PageDown,
        K::Up => Key::UpArrow,
        K::Down => Key::DownArrow,
        K::Left => Key::LeftArrow,
        K::Right => Key::RightArrow,
        K::Space => Key::Space,
        K::CapsLock => Key::CapsLock,
        K::VolumeUp => Key::VolumeUp,
        K::VolumeDown => Key::VolumeDown,
        K::VolumeMute => Key::VolumeMute,
        K::MediaPlayPause => Key::MediaPlayPause,
        K::MediaNext => Key::MediaNextTrack,
        K::MediaPrev => Key::MediaPrevTrack,
        K::F(n) => function_key(n)?,
        #[cfg(not(target_os = "macos"))]
        K::Insert => Key::Insert,
        #[cfg(not(target_os = "macos"))]
        K::PrintScreen => Key::PrintScr,
        #[cfg(not(target_os = "macos"))]
        K::Pause => Key::Pause,
        #[cfg(not(target_os = "macos"))]
        K::NumLock => Key::Numlock,
        #[cfg(not(target_os = "macos"))]
        K::MediaStop => Key::MediaStop,
        #[cfg(windows)]
        K::ScrollLock => Key::Scroll,
        #[cfg(all(unix, not(target_os = "macos")))]
        K::ScrollLock => Key::ScrollLock,
        #[cfg(windows)]
        K::Menu => Key::Apps,
        _ => return None,
    })
}

fn function_key(n: u8) -> Option<Key> {
    Some(match n {
        1 => Key::F1,
        2 => Key::F2,
        3 => Key::F3,
        4 => Key::F4,
        5 => Key::F5,
        6 => Key::F6,
        7 => Key::F7,
        8 => Key::F8,
        9 => Key::F9,
        10 => Key::F10,
        11 => Key::F11,
        12 => Key::F12,
        13 => Key::F13,
        14 => Key::F14,
        15 => Key::F15,
        16 => Key::F16,
        17 => Key::F17,
        18 => Key::F18,
        19 => Key::F19,
        20 => Key::F20,
        #[cfg(not(target_os = "macos"))]
        21 => Key::F21,
        #[cfg(not(target_os = "macos"))]
        22 => Key::F22,
        #[cfg(not(target_os = "macos"))]
        23 => Key::F23,
        #[cfg(not(target_os = "macos"))]
        24 => Key::F24,
        _ => return None,
    })
}

#[cfg(windows)]
fn resolve_char(c: char) -> Result<(Key, Vec<Modifier>)> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] =
        [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    // Letter and digit virtual keys are what shortcuts match on, on every layout (ctrl+c works on a
    // Cyrillic layout too), so they bypass the layout lookup.
    if c.is_ascii_lowercase() {
        return Ok((LETTERS[usize::from(c as u8 - b'a')], Vec::new()));
    }
    if c.is_ascii_digit() {
        return Ok((DIGITS[usize::from(c as u8 - b'0')], Vec::new()));
    }
    let Some((vk, state)) = crate::platform::char_key(c) else {
        bail!("no key on the current keyboard layout types '{c}'; to enter the character, use text instead of keys");
    };
    let mut extra = Vec::new();
    for (bit, m) in [(1, Modifier::Shift), (2, Modifier::Ctrl), (4, Modifier::Alt)] {
        if state & bit != 0 {
            extra.push(m);
        }
    }
    Ok((Key::Other(u32::from(vk)), extra))
}

#[cfg(not(windows))]
fn resolve_char(c: char) -> Result<(Key, Vec<Modifier>)> {
    Ok((Key::Unicode(c), Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyName as K;
    use Modifier as M;

    fn chord(s: &str) -> Chord {
        parse_chord(s).unwrap()
    }

    #[test]
    fn parses_chords() {
        assert_eq!(chord("ctrl+l"), Chord { modifiers: vec![M::Ctrl], key: K::Char('l') });
        assert_eq!(chord("Shift+Ctrl+T"), Chord { modifiers: vec![M::Ctrl, M::Shift], key: K::Char('t') });
        assert_eq!(chord("cmd+space"), Chord { modifiers: vec![M::Meta], key: K::Space });
        assert_eq!(chord("option+Tab"), Chord { modifiers: vec![M::Alt], key: K::Tab });
        assert_eq!(chord("super+Page_Down"), Chord { modifiers: vec![M::Meta], key: K::PageDown });
        assert_eq!(chord("alt+F4"), Chord { modifiers: vec![M::Alt], key: K::F(4) });
        assert_eq!(chord("ctrl+control+c"), Chord { modifiers: vec![M::Ctrl], key: K::Char('c') });
        assert_eq!(chord("Return"), Chord { modifiers: vec![], key: K::Enter });
        // A modifier as the key itself is a tap of that modifier.
        assert_eq!(chord("win"), Chord { modifiers: vec![], key: K::Modifier(M::Meta) });
        assert_eq!(chord("ctrl+shift"), Chord { modifiers: vec![M::Ctrl], key: K::Modifier(M::Shift) });
        assert_eq!(chord("shift+shift"), Chord { modifiers: vec![], key: K::Modifier(M::Shift) });
    }

    #[test]
    fn parses_plus_and_punctuation() {
        assert_eq!(chord("ctrl++"), Chord { modifiers: vec![M::Ctrl], key: K::Char('+') });
        assert_eq!(chord("+"), Chord { modifiers: vec![], key: K::Char('+') });
        assert_eq!(chord("++"), Chord { modifiers: vec![], key: K::Char('+') });
        assert_eq!(chord("ctrl+plus"), chord("ctrl++"));
        assert_eq!(chord("ctrl+-"), Chord { modifiers: vec![M::Ctrl], key: K::Char('-') });
        assert_eq!(chord("ctrl+/"), Chord { modifiers: vec![M::Ctrl], key: K::Char('/') });
        assert_eq!(chord("ctrl+bracket_left"), Chord { modifiers: vec![M::Ctrl], key: K::Char('[') });
        assert_eq!(chord("7"), Chord { modifiers: vec![], key: K::Char('7') });
    }

    #[test]
    fn parses_names_case_insensitively() {
        for (name, key) in [
            ("ENTER", K::Enter),
            ("esc", K::Escape),
            ("Escape", K::Escape),
            ("BackSpace", K::Backspace),
            ("del", K::Delete),
            ("Page-Up", K::PageUp),
            ("pgdn", K::PageDown),
            ("ArrowLeft", K::Left),
            ("Up", K::Up),
            ("f1", K::F(1)),
            ("F24", K::F(24)),
            ("CONTROL", K::Modifier(M::Ctrl)),
            ("Meta", K::Modifier(M::Meta)),
            ("A", K::Char('a')),
            ("Ü", K::Char('ü')),
        ] {
            assert_eq!(parse_key_name(name).unwrap(), key, "{name}");
        }
    }

    #[test]
    fn parses_sequences() {
        let seq = parse_keys("Down  Down\tReturn").unwrap();
        assert_eq!(seq.iter().map(|c| c.key).collect::<Vec<_>>(), vec![K::Down, K::Down, K::Enter]);
        let seq = parse_keys("ctrl+a ctrl+c").unwrap();
        assert_eq!(seq.len(), 2);
        assert!(parse_keys(&"a ".repeat(MAX_CHORDS)).is_ok());
        assert!(parse_keys(&"a ".repeat(MAX_CHORDS + 1)).is_err());
    }

    #[test]
    fn rejects_bad_specs_with_guidance() {
        let err = |s: &str| parse_keys(s).unwrap_err().to_string();
        assert!(err("").contains("empty"));
        assert!(err("   ").contains("empty"));
        assert!(err("ctrl+").contains("not a valid chord"));
        assert!(err("+a").contains("not a valid chord"));
        assert!(err("a+b").contains("not a modifier"));
        assert!(err("f25").contains("f1 to f24"));
        assert!(err("f0").contains("f1 to f24"));
        let unknown = err("hyper+x");
        assert!(unknown.contains("unknown key 'hyper'") && unknown.contains("pagedown") && unknown.contains("f1-f24"));
    }

    #[test]
    fn displays_canonical_names() {
        assert_eq!(chord("Control+Shift+Page_Down").to_string(), "ctrl+shift+pagedown");
        assert_eq!(chord("Return").to_string(), "enter");
        assert_eq!(chord("alt+F4").to_string(), "alt+f4");
        assert_eq!(chord("ctrl++").to_string(), "ctrl++");
    }

    /// The chord pressed literally (characters as `Key::Unicode`), without a layout lookup.
    fn literal(s: &str) -> Pressed {
        let c = chord(s);
        let key = match c.key {
            K::Char(ch) => Key::Unicode(ch),
            other => named_key(other).unwrap(),
        };
        Pressed { key, modifiers: c.modifiers }
    }

    #[test]
    fn refuses_lock_and_sign_out() {
        let check = |s: &str, os| forbidden(&literal(s), os).is_some();
        assert!(check("win+l", Os::Windows));
        assert!(check("Win+Shift+L", Os::Windows));
        assert!(check("ctrl+alt+del", Os::Windows));
        assert!(check("ctrl+alt+shift+Delete", Os::Windows));
        assert!(check("ctrl+alt+end", Os::Windows));
        assert!(!check("ctrl+l", Os::Windows));
        assert!(!check("ctrl+shift+esc", Os::Windows));
        assert!(!check("alt+delete", Os::Windows));
        // cmd+l is the address bar on macOS, not a lock.
        assert!(!check("cmd+l", Os::Mac));
        assert!(check("ctrl+cmd+q", Os::Mac));
        assert!(check("shift+cmd+q", Os::Mac));
        assert!(check("alt+shift+cmd+q", Os::Mac));
        assert!(!check("cmd+q", Os::Mac));
        assert!(!check("win+l", Os::Other));
    }

    #[test]
    fn parses_mouse_modifiers() {
        let names = ["Shift".to_string(), "ctrl".to_string(), "cmd".to_string(), "control".to_string()];
        assert_eq!(parse_modifiers(&names).unwrap(), vec![M::Ctrl, M::Shift, M::Meta]);
        assert!(parse_modifiers(&["a".to_string()]).unwrap_err().to_string().contains("modifier 'a'"));
    }

    #[test]
    fn resolves_named_keys() {
        assert_eq!(resolve(K::Enter).unwrap(), (Key::Return, vec![]));
        assert_eq!(resolve(K::F(12)).unwrap(), (Key::F12, vec![]));
        assert_eq!(resolve(K::Modifier(M::Meta)).unwrap(), (Key::Meta, vec![]));
    }

    #[cfg(windows)]
    #[test]
    fn refuses_resolved_chords_however_spelled() {
        let refused = |key, modifiers: &[Modifier]| {
            forbidden(&Pressed { key, modifiers: modifiers.to_vec() }, Os::Windows).is_some()
        };
        // Letters resolve to their virtual key.
        let win_l = resolve_chord(&chord("win+l")).unwrap();
        assert_eq!(win_l, Pressed { key: Key::L, modifiers: vec![M::Meta] });
        assert!(forbidden(&win_l, Os::Windows).is_some());
        // A character the layout types as AltGr+L ('ł' on Polish layouts) resolves to VK_L plus ctrl+alt.
        assert!(refused(Key::Other(0x4c), &[M::Ctrl, M::Alt, M::Meta]));
        assert!(!refused(Key::Other(0x4c), &[M::Ctrl, M::Alt]));
        // The numpad Del key, and the Delete / End virtual keys.
        assert!(refused(Key::Other(0x6e), &[M::Ctrl, M::Alt]));
        assert!(refused(Key::Other(0x2e), &[M::Ctrl, M::Alt, M::Shift]));
        assert!(refused(Key::Other(0x23), &[M::Ctrl, M::Alt]));
        assert!(!refused(Key::Other(0x2e), &[M::Ctrl]));
    }

    #[test]
    fn resolve_chord_merges_modifiers() {
        let p = resolve_chord(&chord("shift+ctrl+enter")).unwrap();
        assert_eq!(p, Pressed { key: Key::Return, modifiers: vec![M::Ctrl, M::Shift] });
    }

    #[cfg(windows)]
    #[test]
    fn resolves_letters_and_digits_by_virtual_key() {
        assert_eq!(resolve(K::Char('l')).unwrap(), (Key::L, vec![]));
        assert_eq!(resolve(K::Char('z')).unwrap(), (Key::Z, vec![]));
        assert_eq!(resolve(K::Char('0')).unwrap(), (Key::Num0, vec![]));
        assert_eq!(resolve(K::F(24)).unwrap(), (Key::F24, vec![]));
        assert_eq!(resolve(K::Menu).unwrap(), (Key::Apps, vec![]));
    }
}

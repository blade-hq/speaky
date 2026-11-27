#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Key {
    Character(char),
    Function(u8),
    Named(&'static str),
    #[cfg(windows)]
    Numpad(u8),
    #[cfg(windows)]
    Windows(u8),
}

// VK_NUMPAD0 .. VK_DIVIDE in Windows virtual-key order.
#[cfg(windows)]
pub(super) const NUMPAD_NAMES: [&str; 16] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "*", "+", ",", "-", ".", "/",
];

// Names as reported by GPUI, with the corresponding Windows and Carbon codes.
pub(super) const NAMED: &[(&str, &str, u8, u8)] = &[
    ("space", "Space", 0x20, 0x31),
    ("tab", "Tab", 0x09, 0x30),
    ("enter", "Enter", 0x0d, 0x24),
    ("backspace", "Backspace", 0x08, 0x33),
    ("delete", "Delete", 0x2e, 0x75),
    ("insert", "Insert", 0x2d, 0x72),
    ("home", "Home", 0x24, 0x73),
    ("end", "End", 0x23, 0x77),
    ("pageup", "Page Up", 0x21, 0x74),
    ("pagedown", "Page Down", 0x22, 0x79),
    ("left", "Left", 0x25, 0x7b),
    ("up", "Up", 0x26, 0x7e),
    ("right", "Right", 0x27, 0x7c),
    ("down", "Down", 0x28, 0x7d),
];

impl Key {
    fn parse(name: &str) -> Option<Self> {
        #[cfg(windows)]
        if let Some(hex) = name.strip_prefix("vk") {
            let code = u8::from_str_radix(hex, 16).ok()?;
            return super::windows::is_keyboard_key(code).then_some(Self::Windows(code));
        }
        #[cfg(windows)]
        if let Some(suffix) = name.strip_prefix("numpad") {
            return NUMPAD_NAMES
                .iter()
                .position(|&name| name == suffix)
                .map(|i| Self::Numpad(i as u8));
        }
        if let Some(&(name, _, _, _)) = NAMED.iter().find(|entry| entry.0 == name) {
            return Some(Self::Named(name));
        }
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return (!c.is_control() && !c.is_whitespace())
                .then_some(Self::Character(c.to_ascii_lowercase()));
        }
        let number: u8 = name.strip_prefix('f')?.parse().ok()?;
        (1..=24).contains(&number).then_some(Self::Function(number))
    }

    fn name(self) -> String {
        match self {
            Self::Character(c) => c.to_string(),
            Self::Function(n) => format!("f{n}"),
            Self::Named(name) => name.to_owned(),
            #[cfg(windows)]
            Self::Numpad(i) => format!("numpad{}", NUMPAD_NAMES[i as usize]),
            #[cfg(windows)]
            Self::Windows(code) => format!("vk{code:02x}"),
        }
    }

    fn label(self) -> String {
        match self {
            Self::Named(name) => NAMED
                .iter()
                .find(|entry| entry.0 == name)
                .unwrap()
                .1
                .to_owned(),
            #[cfg(windows)]
            Self::Numpad(i) => format!("NumPad {}", NUMPAD_NAMES[i as usize]),
            #[cfg(windows)]
            Self::Windows(code) => super::windows::key_label(code),
            key => key.name().to_ascii_uppercase(),
        }
    }
}

pub const CTRL: u8 = 1;
pub const ALT: u8 = 2;
pub const SHIFT: u8 = 4;
pub const PLATFORM: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub(super) key: Key,
    pub(super) modifiers: u8,
}

impl Default for Shortcut {
    fn default() -> Self {
        if cfg!(windows) {
            Self::ControlShiftSpace
        } else {
            Self::OptionShiftSpace
        }
    }
}

#[allow(non_upper_case_globals)]
impl Shortcut {
    #[cfg(windows)]
    pub(crate) fn windows_key(self) -> Option<(u32, u32)> {
        super::windows::key_and_modifiers(self).ok()
    }
    pub const OptionShiftSpace: Self = Self {
        key: Key::Named("space"),
        modifiers: ALT | SHIFT,
    };
    pub const ControlShiftSpace: Self = Self {
        key: Key::Named("space"),
        modifiers: CTRL | SHIFT,
    };
    pub const ControlShiftD: Self = Self {
        key: Key::Character('d'),
        modifiers: CTRL | SHIFT,
    };

    pub fn from_key(key: &str, modifiers: u8) -> Result<Self, String> {
        let shortcut = Self::from_event(key, modifiers)?;
        #[cfg(windows)]
        if let Some(numpad) = super::windows::pressed_numpad(key) {
            return Ok(Self {
                key: Key::Numpad(numpad),
                ..shortcut
            });
        }
        shortcut.normalized()
    }

    fn from_event(key: &str, modifiers: u8) -> Result<Self, String> {
        let key =
            Key::parse(key).ok_or_else(|| "请选择字母、数字、符号、功能键或导航键。".to_owned())?;
        Ok(Self { key, modifiers })
    }

    fn normalized(self) -> Result<Self, String> {
        #[cfg(windows)]
        return super::windows::normalize(self);
        #[cfg(target_os = "macos")]
        if let Key::Character(c) = self.key {
            let (_, base, shifted) =
                super::mac_layout::character_key(c, self.modifiers & PLATFORM != 0)?;
            return Ok(Self {
                key: Key::Character(base),
                modifiers: self.modifiers | SHIFT * u8::from(shifted),
            });
        }
        #[cfg(not(windows))]
        Ok(self)
    }

    pub fn matches_release(self, key: &str, modifiers: u8) -> bool {
        Self::from_event(key, modifiers)
            .and_then(Self::normalized)
            .is_ok_and(|released| self.key == released.key)
    }

    #[cfg(windows)]
    pub fn from_windows(code: u8, modifiers: u8) -> Self {
        let character = (code as char).to_ascii_lowercase();
        let key = match code {
            0x30..=0x39 | 0x41..=0x5a
                if super::windows::character_key(character).is_ok_and(|pair| pair == (code, 0)) =>
            {
                Key::Character(character)
            }
            0x60..=0x6f => Key::Numpad(code - 0x60),
            0x70..=0x87 => Key::Function(code - 0x70 + 1),
            _ => NAMED
                .iter()
                .find(|entry| entry.2 == code)
                .map_or(Key::Windows(code), |entry| Key::Named(entry.0)),
        };
        Self { key, modifiers }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "option_shift_space" => Some(Self::OptionShiftSpace),
            "control_shift_space" => Some(Self::ControlShiftSpace),
            "control_shift_d" => Some(Self::ControlShiftD),
            id => {
                let (bits, name) = id.strip_prefix("key:")?.split_once(':')?;
                let modifiers: u8 = bits.parse().ok()?;
                if modifiers > (CTRL | ALT | SHIFT | PLATFORM) {
                    return None;
                }
                Some(Self {
                    key: Key::parse(name)?,
                    modifiers,
                })
            }
        }
    }

    pub fn id(self) -> String {
        match self {
            Self::OptionShiftSpace => "option_shift_space".into(),
            Self::ControlShiftSpace => "control_shift_space".into(),
            Self::ControlShiftD => "control_shift_d".into(),
            other => format!("key:{}:{}", other.modifiers, other.key.name()),
        }
    }

    pub fn label(self) -> String {
        let mut label = String::new();
        let platform = if cfg!(target_os = "macos") {
            "Cmd+"
        } else {
            "Win+"
        };
        let alt = if cfg!(target_os = "macos") {
            "Option+"
        } else {
            "Alt+"
        };
        for (modifier, name) in [
            (CTRL, "Ctrl+"),
            (ALT, alt),
            (SHIFT, "Shift+"),
            (PLATFORM, platform),
        ] {
            if self.modifiers & modifier != 0 {
                label.push_str(name);
            }
        }
        label.push_str(&self.key.label());
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_and_round_trips_single_keys_and_chords() {
        for (key, modifiers, label) in [
            ("f8", 0, "F8"),
            ("k", 0, "K"),
            ("j", CTRL | SHIFT, "Ctrl+Shift+J"),
            ("pageup", CTRL, "Ctrl+Page Up"),
        ] {
            let shortcut = Shortcut::from_key(key, modifiers).unwrap();
            assert_eq!(shortcut.label(), label);
            assert_eq!(Shortcut::from_id(&shortcut.id()), Some(shortcut));
        }
        for legacy in [
            "control_shift_space",
            "control_shift_d",
            "option_shift_space",
        ] {
            assert_eq!(Shortcut::from_id(legacy).unwrap().id(), legacy);
        }
    }

    #[test]
    fn rejects_unknown_bindings_without_losing_separator_characters() {
        for id in ["", "key:0:escape", "key:0:shift", "key:16:k", "key:0:f25"] {
            assert_eq!(Shortcut::from_id(id), None);
        }
        for id in ["key:0::", "key:2:+"] {
            assert_eq!(Shortcut::from_id(id).unwrap().id(), id);
        }
    }
}

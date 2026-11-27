use speaky_core::hotkey::{is_keyboard_key, is_modifier, Shortcut, WINDOWS_MODIFIERS};
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{GetKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX},
    WindowsAndMessaging::{
        MSG, WM_CHAR, WM_DEADCHAR, WM_KEYDOWN, WM_KEYUP, WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN,
        WM_SYSKEYUP,
    },
};

pub(crate) enum Event {
    Preview(Option<Shortcut>),
    Chosen(Shortcut),
}

#[derive(Default)]
pub(crate) struct Keys {
    held: u8,
    candidate: Option<(u8, Shortcut)>,
}

impl Keys {
    pub(crate) fn start() -> Self {
        let held = WINDOWS_MODIFIERS
            .iter()
            .enumerate()
            .fold(0, |held, (index, &(key, _))| {
                held | (u8::from(unsafe { GetKeyState(i32::from(key)) < 0 }) << index)
            });
        Self {
            held,
            candidate: None,
        }
    }

    fn modifiers(&self) -> u8 {
        WINDOWS_MODIFIERS
            .iter()
            .enumerate()
            .fold(0, |bits, (index, &(_, bit))| {
                bits | if self.held & (1 << index) != 0 {
                    bit
                } else {
                    0
                }
            })
    }

    pub(crate) fn message(&mut self, message: &MSG) -> Option<Event> {
        if !matches!(
            message.message,
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
        ) {
            return None;
        }
        let key = u8::try_from(message.wParam).ok()?;
        let flags = message.lParam as usize;
        let key = physical_key(key, flags);
        if !is_keyboard_key(key) {
            return None;
        }
        let down = matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN);
        self.input(key, down, down && flags & (1 << 30) != 0)
    }

    fn input(&mut self, key: u8, down: bool, repeated: bool) -> Option<Event> {
        if repeated {
            return None;
        }
        if let Some(index) = WINDOWS_MODIFIERS.iter().position(|&(vk, _)| vk == key) {
            if down {
                let alone = self.held == 0 && self.candidate.is_none();
                self.held |= 1 << index;
                if alone {
                    return self.preview(key, 0);
                }
                if self.candidate.is_some_and(|(vk, _)| is_modifier(vk)) {
                    self.candidate = None;
                    return Some(Event::Preview(None));
                }
            } else {
                self.held &= !(1 << index);
            }
        } else if down && self.candidate.is_none_or(|(vk, _)| is_modifier(vk)) {
            return self.preview(key, self.modifiers());
        }
        if !down && self.candidate.is_some_and(|(vk, _)| vk == key) {
            return self
                .candidate
                .take()
                .map(|(_, shortcut)| Event::Chosen(shortcut));
        }
        None
    }

    fn preview(&mut self, key: u8, modifiers: u8) -> Option<Event> {
        let shortcut = Shortcut::from_windows(key, modifiers);
        self.candidate = Some((key, shortcut));
        Some(Event::Preview(Some(shortcut)))
    }
}

pub(crate) fn is_keyboard_message(message: u32) -> bool {
    matches!(
        message,
        WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_DEADCHAR
            | WM_SYSCHAR
            | WM_SYSDEADCHAR
    )
}

fn physical_key(key: u8, flags: usize) -> u8 {
    let right = flags & (1 << 24) != 0;
    let scan = ((flags >> 16) & 0xff) as u32;
    match key {
        0x10 => match unsafe { MapVirtualKeyW(scan, MAPVK_VSC_TO_VK_EX) } {
            0xa1 => 0xa1,
            _ => 0xa0,
        },
        0x11 => {
            if right {
                0xa3
            } else {
                0xa2
            }
        }
        0x12 => {
            if right {
                0xa5
            } else {
                0xa4
            }
        }
        // IMEs can replace wParam with VK_PROCESSKEY but retain the physical scan code.
        0xe5 => unsafe {
            MapVirtualKeyW(scan | if right { 0xe000 } else { 0 }, MAPVK_VSC_TO_VK_EX) as u8
        },
        key => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(event: Option<Event>) -> Option<String> {
        match event {
            Some(Event::Chosen(shortcut)) => Some(shortcut.id()),
            _ => None,
        }
    }

    #[test]
    fn chord_keeps_modifiers_when_they_are_released_before_its_main_key() {
        let mut keys = Keys::default();
        keys.input(0xa1, true, false);
        keys.input(0x71, true, false);
        assert!(keys.input(0xa1, false, false).is_none());
        assert_eq!(
            chosen(keys.input(0x71, false, false)).as_deref(),
            Some("key:4:f2")
        );
    }

    #[test]
    fn modifier_alone_is_distinct_from_an_incomplete_modifier_chord() {
        let mut keys = Keys::default();
        keys.input(0xa5, true, false);
        assert_eq!(
            chosen(keys.input(0xa5, false, false)).as_deref(),
            Some("key:0:vka5")
        );
        keys.input(0xa2, true, false);
        keys.input(0xa0, true, false);
        assert!(keys.input(0xa0, false, false).is_none());
        assert!(keys.input(0xa2, false, false).is_none());
    }

    #[test]
    fn repeat_does_not_save_before_release_and_navigation_keys_are_choices() {
        let mut keys = Keys::default();
        assert!(matches!(
            keys.input(0x09, true, false),
            Some(Event::Preview(Some(_)))
        ));
        assert!(keys.input(0x09, true, true).is_none());
        assert_eq!(
            chosen(keys.input(0x09, false, false)),
            Some(Shortcut::from_windows(0x09, 0).id())
        );
        assert!(keys.input(0x09, false, false).is_none());
    }

    #[test]
    fn ime_scan_codes_and_extended_modifiers_keep_their_physical_identity() {
        assert_eq!(physical_key(0x11, 1 << 24), 0xa3);
        assert_eq!(physical_key(0x12, 0), 0xa4);
        assert_eq!(physical_key(0x10, 0x36 << 16), 0xa1);
        assert_eq!(physical_key(0xe5, 0x1e << 16), unsafe {
            MapVirtualKeyW(0x1e, MAPVK_VSC_TO_VK_EX) as u8
        });
        assert_eq!(physical_key(0xe5, (0x1d << 16) | (1 << 24)), 0xa3);
    }
}

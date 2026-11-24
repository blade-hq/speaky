use super::{
    shortcut::{ALT, CTRL, PLATFORM, SHIFT},
    windows::is_keyboard_key,
    Shortcut,
};
use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VSC_TO_VK_EX},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            WM_CHAR, WM_DEADCHAR, WM_KEYDOWN, WM_KEYUP, WM_NCDESTROY, WM_SYSCHAR, WM_SYSDEADCHAR,
            WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    },
};

const CAPTURE_ID: usize = 0x5350;

const MODIFIERS: [(u8, u8); 8] = [
    (0xa0, SHIFT),
    (0xa1, SHIFT),
    (0xa2, CTRL),
    (0xa3, CTRL),
    (0xa4, ALT),
    (0xa5, ALT),
    (0x5b, PLATFORM),
    (0x5c, PLATFORM),
];

pub(crate) enum CaptureEvent {
    Preview(Option<Shortcut>),
    Chosen(Shortcut),
}

#[derive(Default)]
struct Keys {
    held: u8,
    candidate: Option<(u8, Shortcut)>,
}

impl Keys {
    fn modifiers(&self) -> u8 {
        MODIFIERS
            .iter()
            .enumerate()
            .fold(0, |bits, (index, &(_, bit))| {
                if self.held & (1 << index) != 0 {
                    bits | bit
                } else {
                    bits
                }
            })
    }

    fn input(&mut self, key: u8, down: bool, repeated: bool) -> Option<CaptureEvent> {
        if repeated {
            return None;
        }
        let modifier = MODIFIERS.iter().position(|&(vk, _)| vk == key);
        if let Some(index) = modifier {
            if down {
                let alone = self.held == 0 && self.candidate.is_none();
                self.held |= 1 << index;
                if alone {
                    return self.preview(key, 0);
                }
                if self.candidate.is_some_and(|(vk, _)| is_modifier(vk)) {
                    self.candidate = None;
                    return Some(CaptureEvent::Preview(None));
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
                .map(|(_, shortcut)| CaptureEvent::Chosen(shortcut));
        }
        None
    }

    fn preview(&mut self, key: u8, modifiers: u8) -> Option<CaptureEvent> {
        let shortcut = Shortcut::from_windows(key, modifiers);
        self.candidate = Some((key, shortcut));
        Some(CaptureEvent::Preview(Some(shortcut)))
    }
}

pub(super) fn is_modifier(key: u8) -> bool {
    MODIFIERS.iter().any(|&(vk, _)| vk == key)
}

struct Capture {
    keys: Keys,
    events: UnboundedSender<CaptureEvent>,
    attached: bool,
}

pub(crate) struct WindowCapture {
    hwnd: HWND,
    capture: Box<Capture>,
}

impl WindowCapture {
    pub(crate) fn start(
        window: &Window,
    ) -> Result<(Self, UnboundedReceiver<CaptureEvent>), String> {
        let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
            .map_err(|err| err.to_string())?
            .as_raw()
        else {
            return Err("无法打开快捷键编辑器。".into());
        };
        let hwnd = HWND(raw.hwnd.get() as _);
        let (events, receiver) = unbounded_channel();
        let mut capture = Box::new(Capture {
            keys: Keys::default(),
            events,
            attached: true,
        });
        let data = capture.as_mut() as *mut Capture as usize;
        if !unsafe { SetWindowSubclass(hwnd, Some(capture_proc), CAPTURE_ID, data) }.as_bool() {
            return Err("无法捕获 Windows 按键，请重新打开快捷键设置。".into());
        }
        Ok((Self { hwnd, capture }, receiver))
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        if self.capture.attached {
            // The capture and its window both belong to the UI thread.
            let _ = unsafe { RemoveWindowSubclass(self.hwnd, Some(capture_proc), CAPTURE_ID) };
        }
    }
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
        // IMEs can report VK_PROCESSKEY while retaining the original scan code.
        0xe5 => unsafe {
            MapVirtualKeyW(scan | if right { 0xe000 } else { 0 }, MAPVK_VSC_TO_VK_EX) as u8
        },
        key => key,
    }
}

unsafe extern "system" fn capture_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let capture = unsafe { &mut *(data as *mut Capture) };
    match message {
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            if let Ok(key) = u8::try_from(wparam.0) {
                let flags = lparam.0 as usize;
                let key = physical_key(key, flags);
                if is_keyboard_key(key) {
                    let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
                    let repeated = down && flags & (1 << 30) != 0;
                    if let Some(event) = capture.keys.input(key, down, repeated) {
                        let _ = capture.events.send(event);
                    }
                }
            }
            return LRESULT(0);
        }
        WM_CHAR | WM_DEADCHAR | WM_SYSCHAR | WM_SYSDEADCHAR => return LRESULT(0),
        WM_NCDESTROY => {
            capture.attached = false;
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(capture_proc), CAPTURE_ID) };
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_modifier_and_modified_key_are_distinct_choices() {
        let mut keys = Keys::default();
        keys.input(0xa1, true, false);
        assert!(matches!(keys.input(0xa1, false, false),
            Some(CaptureEvent::Chosen(key)) if key.id() == "key:0:vka1"));
        keys.input(0xa1, true, false);
        keys.input(0x71, true, false);
        assert!(keys.input(0xa1, false, false).is_none());
        assert!(matches!(keys.input(0x71, false, false),
            Some(CaptureEvent::Chosen(key)) if key.id() == "key:4:f2"));
        keys.input(0x14, true, false);
        assert!(matches!(keys.input(0x14, false, false),
            Some(CaptureEvent::Chosen(key)) if key.id() == "key:0:vk14"));
    }
}

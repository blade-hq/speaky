#[cfg(test)]
use super::shortcut::NUMPAD_NAMES;
use super::shortcut::{Key, ALT, CTRL, NAMED, PLATFORM, SHIFT};
use super::windows_capture::is_modifier;
use super::{HotkeyEvents, Shortcut};
use std::cell::{Cell, RefCell};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use windows_sys::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, WAIT_OBJECT_0};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, INFINITE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyNameTextW, MapVirtualKeyW, RegisterHotKey, UnregisterHotKey,
    VkKeyScanW, MAPVK_VK_TO_VSC_EX, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_F1,
    VK_NUMPAD0,
};
#[cfg(test)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, MAPVK_VK_TO_CHAR};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, MsgWaitForMultipleObjects, PeekMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, MSG, PM_REMOVE,
    QS_ALLINPUT, QS_HOTKEY, WH_KEYBOARD_LL, WM_HOTKEY,
};

struct RegisteredKey {
    shortcut: Shortcut,
    stop: Arc<OwnedHandle>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(test)]
pub(super) fn numpad_matches(offset: u8, event_key: &str) -> bool {
    // Match exactly the layout-specific character GPUI produces from wParam.
    // In particular, VK_DECIMAL may be reported as '.' or ','.
    let vk = VK_NUMPAD0 + u16::from(offset);
    let code = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_CHAR) } & 0xffff;
    code != 0 && event_key.encode_utf16().eq([code as u16])
}

#[cfg(test)]
fn find_pressed_numpad(event_key: &str, pressed: impl Fn(u16) -> bool) -> Option<u8> {
    if event_key.chars().count() != 1 {
        return None;
    }
    (0..NUMPAD_NAMES.len() as u8).find(|&offset| {
        pressed(VK_NUMPAD0 + u16::from(offset)) && numpad_matches(offset, event_key)
    })
}

#[cfg(test)]
pub(super) fn pressed_numpad(event_key: &str) -> Option<u8> {
    // GetKeyState reflects the message currently being dispatched on the UI thread.
    find_pressed_numpad(event_key, |vk| unsafe { GetKeyState(i32::from(vk)) < 0 })
}

pub(super) fn is_keyboard_key(key: u8) -> bool {
    (0x08..=0xfe).contains(&key) && !matches!(key, 0xe5 | 0xe7)
}

pub(super) fn key_label(key: u8) -> String {
    let label = match key {
        0x13 => "Pause",
        0x14 => "Caps Lock",
        0x1b => "Esc",
        0x2c => "Print Screen",
        0x5b => "左 Win",
        0x5c => "右 Win",
        0x90 => "Num Lock",
        0x91 => "Scroll Lock",
        0xa0 => "左 Shift",
        0xa1 => "右 Shift",
        0xa2 => "左 Ctrl",
        0xa3 => "右 Ctrl",
        0xa4 => "左 Alt",
        0xa5 => "右 Alt",
        _ => {
            let scan = unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC_EX) };
            let flags = ((scan & 0xff) << 16) | (u32::from(scan & 0xff00 == 0xe000) << 24);
            let mut buffer = [0; 64];
            let len =
                unsafe { GetKeyNameTextW(flags as i32, buffer.as_mut_ptr(), buffer.len() as i32) };
            return if len > 0 {
                String::from_utf16_lossy(&buffer[..len as usize])
            } else {
                format!("键 0x{key:02X}")
            };
        }
    };
    label.into()
}

pub(super) fn character_key(c: char) -> Result<(u8, u8), String> {
    let code = u16::try_from(u32::from(c))
        .ok()
        .map(|ch| unsafe { VkKeyScanW(ch) })
        .unwrap_or(-1);
    if code == -1 {
        return Err(format!("当前键盘布局无法注册字符 {c}。"));
    }
    let [key, state] = code.to_le_bytes();
    // VkKeyScanW encodes Shift, Ctrl and Alt in bits 0, 1 and 2.
    let modifiers = SHIFT * (state & 1) | CTRL * ((state >> 1) & 1) | ALT * ((state >> 2) & 1);
    Ok((key, modifiers))
}

#[cfg(test)]
pub(super) fn normalize(mut shortcut: Shortcut) -> Result<Shortcut, String> {
    // GPUI folds Shift+1 into "!" on US keyboards, for example. Recover the
    // base key and required modifiers using the input thread's current layout.
    if let Key::Character(c) = shortcut.key {
        let (vk, implicit) = character_key(c)?;
        if implicit != 0 {
            let base = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_CHAR) } & 0xffff;
            if let Some(base) = char::from_u32(base).filter(|c| !c.is_control()) {
                let base = base.to_ascii_lowercase();
                if character_key(base).is_ok_and(|(key, _)| key == vk) {
                    shortcut.key = Key::Character(base);
                    shortcut.modifiers |= implicit;
                }
            }
        }
    }
    Ok(shortcut)
}

fn key_and_modifiers(shortcut: Shortcut) -> Result<(u32, u32), String> {
    let (key, implicit) = match shortcut.key {
        Key::Character(c) => character_key(c)?,
        Key::Function(n) => (VK_F1 as u8 + n - 1, 0),
        Key::Named(name) => (NAMED.iter().find(|entry| entry.0 == name).unwrap().2, 0),
        Key::Numpad(i) => (VK_NUMPAD0 as u8 + i, 0),
        Key::Windows(code) => (code, 0),
    };
    let mut flags = MOD_NOREPEAT;
    for (bit, flag) in [
        (CTRL, MOD_CONTROL),
        (ALT, MOD_ALT),
        (SHIFT, MOD_SHIFT),
        (PLATFORM, MOD_WIN),
    ] {
        if (shortcut.modifiers | implicit) & bit != 0 {
            flags |= flag;
        }
    }
    Ok((u32::from(key), flags))
}

struct ModifierTap {
    key: u8,
    pressed: [bool; 256],
    alone: bool,
}

impl ModifierTap {
    fn update(&mut self, key: u8, down: bool) -> bool {
        if self.pressed[key as usize] == down {
            return false;
        }
        self.pressed[key as usize] = down;
        if key == self.key {
            if down {
                self.alone = !self
                    .pressed
                    .iter()
                    .enumerate()
                    .any(|(i, down)| i != key as usize && *down);
                return false;
            }
            return std::mem::take(&mut self.alone);
        }
        if down {
            self.alone = false;
        }
        false
    }
}

struct HookState {
    tap: ModifierTap,
    generation: u32,
    events: UnboundedSender<u32>,
}

thread_local! {
    static MODIFIER_HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

unsafe extern "system" fn modifier_proc(code: i32, message: usize, data: isize) -> isize {
    if code == HC_ACTION as i32 {
        let event = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
        if let Ok(key) = u8::try_from(event.vkCode) {
            let down = event.flags & LLKHF_UP == 0;
            MODIFIER_HOOK.with_borrow_mut(|state| {
                if let Some(state) = state {
                    if event.flags & LLKHF_INJECTED != 0 {
                        if down {
                            state.tap.alone = false;
                        }
                    } else if state.tap.update(key, down) {
                        let _ = state.events.send(state.generation);
                    }
                }
            });
        }
    }
    // Leave Shift/Ctrl/Alt available to every application's original shortcuts.
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}

fn watch_modifier(
    key: u8,
    generation: u32,
    events: UnboundedSender<u32>,
    stop: &OwnedHandle,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let mut pressed = [false; 256];
    for code in 0..=u8::MAX {
        if is_keyboard_key(code) && !matches!(code, 0x10..=0x12) {
            pressed[code as usize] = unsafe { GetAsyncKeyState(i32::from(code)) < 0 };
        }
    }
    MODIFIER_HOOK.set(Some(HookState {
        tap: ModifierTap {
            key,
            pressed,
            alone: false,
        },
        generation,
        events,
    }));
    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(modifier_proc),
            GetModuleHandleW(null()),
            0,
        )
    };
    if hook.is_null() {
        let _ = ready.send(Err(format!(
            "无法监听单独的 {}（{}）。",
            key_label(key),
            io::Error::last_os_error()
        )));
    } else {
        if ready.send(Ok(())).is_ok() {
            let stop = stop.as_raw_handle();
            while unsafe { MsgWaitForMultipleObjects(1, &stop, 0, INFINITE, QS_ALLINPUT) }
                == WAIT_OBJECT_0 + 1
            {
                // PeekMessage dispatches the sent input callbacks on this thread.
                let mut message = MSG::default();
                while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {}
            }
        }
        unsafe { UnhookWindowsHookEx(hook) };
    }
    MODIFIER_HOOK.set(None);
}

impl RegisteredKey {
    fn register(
        shortcut: Shortcut,
        generation: u32,
        events: UnboundedSender<u32>,
    ) -> Result<Self, String> {
        let (key, modifiers) = key_and_modifiers(shortcut)?;
        if matches!(key, 0x7b | 0x5b | 0x5c) {
            return Err(format!(
                "{} 由 Windows 保留，请换一个快捷键。",
                shortcut.label()
            ));
        }
        let modifier_only = is_modifier(key as u8);
        if modifier_only && modifiers != MOD_NOREPEAT {
            return Err("只使用修饰键时，请单独按其中一个键。".into());
        }
        let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error().to_string());
        }
        // The worker and its owner share the stop event until the worker has exited.
        let stop = Arc::new(unsafe { OwnedHandle::from_raw_handle(handle) });
        let worker_stop = Arc::clone(&stop);
        let (ready, registered) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("speaky-hotkey".into())
            .spawn(move || {
                if modifier_only {
                    watch_modifier(key as u8, generation, events, &worker_stop, ready);
                    return;
                }
                // A thread hotkey must be registered, polled and released on the same thread.
                const ID: i32 = 1;
                if unsafe { RegisterHotKey(null_mut(), ID, modifiers, key) } == 0 {
                    let error = io::Error::last_os_error();
                    let label = shortcut.label();
                    let message =
                        if error.raw_os_error() == Some(ERROR_HOTKEY_ALREADY_REGISTERED as i32) {
                            format!("{label} 已被其他应用占用。")
                        } else {
                            format!("无法注册 {label}（{error}）。")
                        };
                    let _ = ready.send(Err(message));
                    return;
                }
                if ready.send(Ok(())).is_ok() {
                    let handle = worker_stop.as_raw_handle();
                    loop {
                        let result = unsafe {
                            MsgWaitForMultipleObjects(1, &handle, 0, INFINITE, QS_HOTKEY)
                        };
                        if result != WAIT_OBJECT_0 + 1 {
                            if result != WAIT_OBJECT_0 {
                                log::error!("全局快捷键监听失败：{}", io::Error::last_os_error());
                            }
                            break;
                        }
                        let mut msg = MSG::default();
                        while unsafe {
                            PeekMessageW(&mut msg, null_mut(), WM_HOTKEY, WM_HOTKEY, PM_REMOVE)
                        } != 0
                        {
                            if msg.message == WM_HOTKEY && msg.wParam == ID as usize {
                                let _ = events.send(generation);
                            }
                        }
                    }
                }
                unsafe { UnregisterHotKey(null_mut(), ID) };
            })
            .map_err(|error| error.to_string())?;
        let key = Self {
            shortcut,
            stop,
            worker: Some(worker),
        };
        registered.recv().map_err(|error| error.to_string())??;
        Ok(key)
    }
}

impl Drop for RegisteredKey {
    fn drop(&mut self) {
        unsafe { SetEvent(self.stop.as_raw_handle()) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct GlobalHotkey {
    key: Option<RegisteredKey>,
    events: UnboundedSender<u32>,
    active_id: Rc<Cell<Option<u32>>>,
    generation: u32,
}

impl GlobalHotkey {
    pub fn register(shortcut: Shortcut) -> Result<(Self, HotkeyEvents), String> {
        let (events, receiver) = unbounded_channel();
        let key = RegisteredKey::register(shortcut, 1, events.clone())?;
        let active_id = Rc::new(Cell::new(Some(1)));
        Ok((
            Self {
                key: Some(key),
                events,
                active_id: active_id.clone(),
                generation: 1,
            },
            HotkeyEvents {
                events: receiver,
                active_id,
            },
        ))
    }

    pub fn change(
        &mut self,
        shortcut: Shortcut,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if self
            .key
            .as_ref()
            .is_some_and(|key| key.shortcut == shortcut)
        {
            return persist();
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| "全局快捷键注册次数已达到上限。".to_string())?;
        let key = RegisteredKey::register(shortcut, self.generation, self.events.clone())?;
        persist()?;
        self.active_id.set(Some(self.generation));
        self.key = Some(key);
        Ok(())
    }

    pub fn suspend(&mut self) {
        self.active_id.set(None);
        self.key.take();
    }

    pub fn resume(&mut self, shortcut: Shortcut) -> Result<(), String> {
        self.change(shortcut, || Ok(()))
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        self.active_id.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_modifier_fires_on_release_and_ignores_chords() {
        let mut tap = ModifierTap {
            key: 0xa1,
            pressed: [false; 256],
            alone: false,
        };
        assert!(!tap.update(0xa1, true));
        assert!(!tap.update(0xa1, true));
        assert!(tap.update(0xa1, false));
        assert!(!tap.update(0xa1, false));
        for (key, down) in [
            (0xa0, true),
            (0xa0, false),
            (0xa1, true),
            (0x71, true),
            (0x71, false),
            (0xa1, false),
            (0x41, true),
            (0xa1, true),
            (0x41, false),
            (0xa1, false),
        ] {
            assert!(!tap.update(key, down));
        }
    }

    #[test]
    fn keypad_digits_and_operators_survive_capture_and_reload_with_distinct_virtual_keys() {
        for (text, offset) in [("1", 1), ("+", 11), ("-", 13), ("*", 10), ("/", 15)] {
            let vk = VK_NUMPAD0 + offset;
            let key = find_pressed_numpad(text, |candidate| candidate == vk).unwrap();
            let shortcut = Shortcut {
                key: Key::Numpad(key),
                modifiers: 0,
            };
            assert_eq!(shortcut.label(), format!("NumPad {text}"));
            assert_eq!(Shortcut::from_id(&shortcut.id()), Some(shortcut));
            assert_eq!(
                key_and_modifiers(shortcut).unwrap(),
                (u32::from(vk), MOD_NOREPEAT)
            );
            assert!(numpad_matches(key, text));
            assert_eq!(find_pressed_numpad(text, |_| false), None);
        }
        let main_row = Shortcut::from_id("key:0:1").unwrap();
        let keypad = Shortcut::from_id("key:0:numpad1").unwrap();
        assert_ne!(
            key_and_modifiers(main_row).unwrap().0,
            key_and_modifiers(keypad).unwrap().0
        );
    }

    #[test]
    fn registers_single_function_key_and_legacy_chord_without_repeat() {
        let f8 = Shortcut::from_id("key:0:f8").unwrap();
        assert_eq!(key_and_modifiers(f8).unwrap(), (0x77, MOD_NOREPEAT));
        assert_eq!(
            key_and_modifiers(Shortcut::ControlShiftSpace).unwrap(),
            (0x20, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT)
        );
    }

    #[test]
    fn folded_punctuation_keeps_the_layouts_key_and_shift_state() {
        // The current layout may put '!' on a shifted digit or on an unshifted key.
        let Ok((vk, required)) = character_key('!') else {
            return;
        };
        let raw = Shortcut {
            key: Key::Character('!'),
            modifiers: CTRL,
        };
        let normalized = normalize(raw).unwrap();
        let (registered_key, flags) = key_and_modifiers(normalized).unwrap();
        assert_eq!(registered_key, u32::from(vk));
        assert_eq!(flags & MOD_SHIFT != 0, required & SHIFT != 0);
        assert_eq!(
            flags & (MOD_CONTROL | MOD_NOREPEAT),
            MOD_CONTROL | MOD_NOREPEAT
        );
    }
}

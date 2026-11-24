use std::{cell::Cell, rc::Rc};
use tokio::sync::mpsc::UnboundedReceiver;

#[cfg(target_os = "macos")]
mod mac_layout;
mod shortcut;

pub use shortcut::Shortcut;

pub struct HotkeyEvents {
    events: UnboundedReceiver<u32>,
    active_id: Rc<Cell<Option<u32>>>,
}

impl HotkeyEvents {
    pub async fn recv(&mut self) -> Option<()> {
        while let Some(id) = self.events.recv().await {
            if self.active_id.get() == Some(id) {
                return Some(());
            }
        }
        None
    }
}

#[cfg(target_os = "macos")]
pub use macos::GlobalHotkey;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::GlobalHotkey;
#[cfg(windows)]
mod windows_capture;
#[cfg(windows)]
pub(crate) use windows_capture::{CaptureEvent, WindowCapture};

#[cfg(not(any(target_os = "macos", windows)))]
pub struct GlobalHotkey;

#[cfg(not(any(target_os = "macos", windows)))]
impl GlobalHotkey {
    pub fn register(_: Shortcut) -> Result<(Self, HotkeyEvents), String> {
        Err("全局快捷键目前支持 macOS 和 Windows。".into())
    }

    pub fn change(
        &mut self,
        _: Shortcut,
        _: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        Err("全局快捷键目前支持 macOS 和 Windows。".into())
    }

    pub fn suspend(&mut self) {}
    pub fn resume(&mut self, _: Shortcut) -> Result<(), String> {
        Err("全局快捷键目前支持 macOS 和 Windows。".into())
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct PressGate(Option<u32>);

#[cfg(any(target_os = "macos", test))]
impl PressGate {
    fn update(&mut self, id: u32, pressed: bool) -> bool {
        if pressed {
            self.0.replace(id) != Some(id)
        } else {
            if self.0 == Some(id) {
                self.0 = None;
            }
            false
        }
    }
}

#[cfg(test)]
#[test]
fn mac_press_gate_rearms_only_after_the_same_key_is_released() {
    let mut gate = PressGate::default();
    assert!(gate.update(1, true));
    assert!(!gate.update(1, true));
    assert!(gate.update(2, true)); // New registration, previous key may still be held.
    assert!(!gate.update(1, false));
    assert!(!gate.update(2, true));
    assert!(!gate.update(2, false));
    assert!(gate.update(2, true));
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::rc::Rc;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

    use super::shortcut::{Key, NAMED};
    use super::{HotkeyEvents, PressGate, Shortcut};

    const KEYBOARD_EVENT: u32 = u32::from_be_bytes(*b"keyb");
    const HOTKEY_PRESSED: u32 = 5;
    const HOTKEY_RELEASED: u32 = 6;
    const COMMAND: u32 = 1 << 8;
    const SHIFT: u32 = 1 << 9;
    const OPTION: u32 = 1 << 11;
    const CONTROL: u32 = 1 << 12;
    const EXCLUSIVE: u32 = 1;
    const EVENT_NOT_HANDLED: i32 = -9874;
    const HOTKEY_EXISTS: i32 = -9878;
    const SIGNATURE: u32 = u32::from_be_bytes(*b"Spky");

    thread_local! {
        static EVENTS: RefCell<Option<EventListener>> = const { RefCell::new(None) };
        static NEXT_ID: Cell<u32> = const { Cell::new(1) };
    }

    struct EventListener {
        sender: UnboundedSender<u32>,
        active_id: Rc<Cell<Option<u32>>>,
        press_gate: PressGate,
    }

    #[repr(C)]
    struct EventType {
        class: u32,
        kind: u32,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    #[repr(C)]
    struct EventHotkeyId {
        signature: u32,
        id: u32,
    }

    type CarbonRef = *mut c_void;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn GetApplicationEventTarget() -> CarbonRef;
        fn GetEventKind(event: CarbonRef) -> u32;
        fn InstallEventHandler(
            target: CarbonRef,
            handler: extern "C" fn(CarbonRef, CarbonRef, *mut c_void) -> i32,
            count: usize,
            events: *const EventType,
            context: *mut c_void,
            registration: *mut CarbonRef,
        ) -> i32;
        fn GetEventParameter(
            event: CarbonRef,
            name: u32,
            expected_type: u32,
            actual_type: *mut u32,
            capacity: usize,
            actual_size: *mut usize,
            value: *mut c_void,
        ) -> i32;
        fn RegisterEventHotKey(
            key: u32,
            modifiers: u32,
            hotkey_id: EventHotkeyId,
            target: CarbonRef,
            options: u32,
            registration: *mut CarbonRef,
        ) -> i32;
        fn UnregisterEventHotKey(registration: CarbonRef) -> i32;
        fn RemoveEventHandler(registration: CarbonRef) -> i32;
        fn pthread_main_np() -> std::ffi::c_int;
    }

    struct RegisteredKey {
        shortcut: Shortcut,
        id: u32,
        registration: CarbonRef,
    }

    fn key_and_modifiers(shortcut: Shortcut) -> Result<(u32, u32), String> {
        use super::shortcut;
        let mut flags = 0;
        for (bit, flag) in [
            (shortcut::CTRL, CONTROL),
            (shortcut::ALT, OPTION),
            (shortcut::SHIFT, SHIFT),
            (shortcut::PLATFORM, COMMAND),
        ] {
            if shortcut.modifiers & bit != 0 {
                flags |= flag;
            }
        }
        let key = match shortcut.key {
            Key::Named(name) => u32::from(NAMED.iter().find(|entry| entry.0 == name).unwrap().3),
            Key::Function(n) => {
                // Carbon kVK_F1 through kVK_F20, in function-key order.
                const FUNCTION_KEYS: [u8; 20] = [
                    122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64,
                    79, 80, 90,
                ];
                u32::from(
                    *FUNCTION_KEYS
                        .get(usize::from(n - 1))
                        .ok_or_else(|| format!("macOS 不支持注册 F{n}。"))?,
                )
            }
            Key::Character(c) => {
                let (key, _, shifted) = super::mac_layout::character_key(c, flags & COMMAND != 0)?;
                if shifted {
                    flags |= SHIFT;
                }
                u32::from(key)
            }
        };
        Ok((key, flags))
    }

    impl RegisteredKey {
        fn register(shortcut: Shortcut, target: CarbonRef) -> Result<Self, String> {
            let (key, modifiers) = key_and_modifiers(shortcut)?;
            let id = NEXT_ID.with(|next| {
                let id = next.get();
                let following = id
                    .checked_add(1)
                    .ok_or_else(|| "全局快捷键注册次数已达到上限。".to_owned())?;
                next.set(following);
                Ok::<_, String>(id)
            })?;
            let mut registration = null_mut();
            let status = unsafe {
                RegisterEventHotKey(
                    key,
                    modifiers,
                    EventHotkeyId {
                        signature: SIGNATURE,
                        id,
                    },
                    target,
                    EXCLUSIVE,
                    &mut registration,
                )
            };
            let label = shortcut.label();
            match status {
                0 => Ok(Self {
                    shortcut,
                    id,
                    registration,
                }),
                HOTKEY_EXISTS => Err(format!("{label} 已被其他应用占用。")),
                _ => Err(format!("无法注册 {label}（macOS 状态码 {status}）。")),
            }
        }
    }

    impl Drop for RegisteredKey {
        fn drop(&mut self) {
            unsafe { UnregisterEventHotKey(self.registration) };
        }
    }

    pub struct GlobalHotkey {
        key: Option<RegisteredKey>,
        handler: CarbonRef,
        active_id: Rc<Cell<Option<u32>>>,
    }

    impl GlobalHotkey {
        pub fn register(shortcut: Shortcut) -> Result<(Self, HotkeyEvents), String> {
            if unsafe { pthread_main_np() } == 0 {
                return Err("请在 macOS 主线程注册全局快捷键。".into());
            }
            if EVENTS.with(|events| events.borrow().is_some()) {
                return Err("已经安装全局快捷键监听。".into());
            }
            let target = unsafe { GetApplicationEventTarget() };
            let key = RegisteredKey::register(shortcut, target)?;
            let kinds = [HOTKEY_PRESSED, HOTKEY_RELEASED].map(|kind| EventType {
                class: KEYBOARD_EVENT,
                kind,
            });
            let mut handler = null_mut();
            let status = unsafe {
                InstallEventHandler(
                    target,
                    on_key,
                    kinds.len(),
                    kinds.as_ptr(),
                    null_mut(),
                    &mut handler,
                )
            };
            if status != 0 {
                return Err(format!("安装全局快捷键监听失败（macOS 状态码 {status}）。"));
            }

            let active_id = Rc::new(Cell::new(Some(key.id)));
            let (sender, receiver) = unbounded_channel();
            EVENTS.with(|events| {
                *events.borrow_mut() = Some(EventListener {
                    sender,
                    active_id: active_id.clone(),
                    press_gate: PressGate::default(),
                });
            });
            Ok((
                Self {
                    key: Some(key),
                    handler,
                    active_id: active_id.clone(),
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
            let target = unsafe { GetApplicationEventTarget() };
            let key = RegisteredKey::register(shortcut, target)?;
            persist()?;
            self.active_id.set(Some(key.id));
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
            unsafe { RemoveEventHandler(self.handler) };
            let _ = EVENTS.try_with(|events| events.borrow_mut().take());
        }
    }

    extern "C" fn on_key(_: CarbonRef, event: CarbonRef, _: *mut c_void) -> i32 {
        let mut id = EventHotkeyId {
            signature: 0,
            id: 0,
        };
        let status = unsafe {
            GetEventParameter(
                event,
                u32::from_be_bytes(*b"----"),
                u32::from_be_bytes(*b"hkid"),
                null_mut(),
                std::mem::size_of::<EventHotkeyId>(),
                null_mut(),
                (&mut id as *mut EventHotkeyId).cast(),
            )
        };
        if status != 0 || id.signature != SIGNATURE {
            return EVENT_NOT_HANDLED;
        }
        EVENTS
            .try_with(|events| {
                let mut events = events.borrow_mut();
                let Some(listener) = events.as_mut() else {
                    return EVENT_NOT_HANDLED;
                };
                if listener.active_id.get() != Some(id.id) {
                    return EVENT_NOT_HANDLED;
                }
                let pressed = unsafe { GetEventKind(event) } == HOTKEY_PRESSED;
                if listener.press_gate.update(id.id, pressed) {
                    let _ = listener.sender.send(id.id);
                }
                0
            })
            .unwrap_or(EVENT_NOT_HANDLED)
    }
}

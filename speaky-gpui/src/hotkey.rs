use std::{cell::Cell, rc::Rc};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shortcut {
    #[default]
    OptionShiftSpace,
    ControlShiftSpace,
    ControlShiftD,
}

impl Shortcut {
    pub const ALL: [Self; 3] = [
        Self::OptionShiftSpace,
        Self::ControlShiftSpace,
        Self::ControlShiftD,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OptionShiftSpace => "⌥⇧Space",
            Self::ControlShiftSpace => "⌃⇧Space",
            Self::ControlShiftD => "⌃⇧D",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::OptionShiftSpace => "option_shift_space",
            Self::ControlShiftSpace => "control_shift_space",
            Self::ControlShiftD => "control_shift_d",
        }
    }
}

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

#[cfg(not(target_os = "macos"))]
pub struct GlobalHotkey;

#[cfg(not(target_os = "macos"))]
impl GlobalHotkey {
    pub fn register(_: Shortcut) -> Result<(Self, HotkeyEvents), String> {
        Err("全局快捷键目前仅支持 macOS。".into())
    }

    pub fn change(
        &mut self,
        _: Shortcut,
        _: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        Err("全局快捷键目前仅支持 macOS。".into())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::rc::Rc;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

    use super::{HotkeyEvents, Shortcut};

    const KEYBOARD_EVENT: u32 = u32::from_be_bytes(*b"keyb");
    const HOTKEY_RELEASED: u32 = 6;
    const SPACE: u32 = 0x31;
    const ANSI_D: u32 = 0x02;
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

    impl RegisteredKey {
        fn register(shortcut: Shortcut, target: CarbonRef) -> Result<Self, String> {
            let (key, modifiers) = match shortcut {
                Shortcut::OptionShiftSpace => (SPACE, OPTION | SHIFT),
                Shortcut::ControlShiftSpace => (SPACE, CONTROL | SHIFT),
                Shortcut::ControlShiftD => (ANSI_D, CONTROL | SHIFT),
            };
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
        key: RegisteredKey,
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
            let event = EventType {
                class: KEYBOARD_EVENT,
                kind: HOTKEY_RELEASED,
            };
            let mut handler = null_mut();
            let status = unsafe {
                InstallEventHandler(target, on_release, 1, &event, null_mut(), &mut handler)
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
                });
            });
            Ok((
                Self {
                    key,
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
            if shortcut == self.key.shortcut {
                return persist();
            }
            let target = unsafe { GetApplicationEventTarget() };
            let key = RegisteredKey::register(shortcut, target)?;
            persist()?;
            self.active_id.set(Some(key.id));
            self.key = key;
            Ok(())
        }
    }

    impl Drop for GlobalHotkey {
        fn drop(&mut self) {
            self.active_id.set(None);
            unsafe { RemoveEventHandler(self.handler) };
            let _ = EVENTS.try_with(|events| events.borrow_mut().take());
        }
    }

    extern "C" fn on_release(_: CarbonRef, event: CarbonRef, _: *mut c_void) -> i32 {
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
                let events = events.borrow();
                let Some(listener) = events.as_ref() else {
                    return EVENT_NOT_HANDLED;
                };
                if listener.active_id.get() != Some(id.id) {
                    return EVENT_NOT_HANDLED;
                }
                let _ = listener.sender.send(id.id);
                0
            })
            .unwrap_or(EVENT_NOT_HANDLED)
    }
}

use tokio::sync::mpsc::UnboundedReceiver;

pub const LABEL: &str = "⌥⇧Space";

#[cfg(target_os = "macos")]
pub use macos::GlobalHotkey;

#[cfg(not(target_os = "macos"))]
pub struct GlobalHotkey;

#[cfg(not(target_os = "macos"))]
impl GlobalHotkey {
    pub fn register() -> Result<(Self, UnboundedReceiver<()>), String> {
        Err("全局快捷键目前仅支持 macOS。".into())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

    use super::{UnboundedReceiver, LABEL};

    const KEYBOARD_EVENT: u32 = u32::from_be_bytes(*b"keyb");
    const HOTKEY_RELEASED: u32 = 6;
    const SPACE: u32 = 0x31;
    const SHIFT: u32 = 1 << 9;
    const OPTION: u32 = 1 << 11;
    const EXCLUSIVE: u32 = 1;
    const EVENT_NOT_HANDLED: i32 = -9874;
    const HOTKEY_EXISTS: i32 = -9878;
    const HOTKEY_ID: EventHotkeyId = EventHotkeyId {
        signature: u32::from_be_bytes(*b"Spky"),
        id: 1,
    };

    thread_local! {
        static EVENTS: RefCell<Option<UnboundedSender<()>>> = const { RefCell::new(None) };
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

    pub struct GlobalHotkey {
        key: CarbonRef,
        handler: CarbonRef,
    }

    impl GlobalHotkey {
        pub fn register() -> Result<(Self, UnboundedReceiver<()>), String> {
            if unsafe { pthread_main_np() } == 0 {
                return Err("请在 macOS 主线程注册全局快捷键。".into());
            }
            if EVENTS.with(|events| events.borrow().is_some()) {
                return Err(format!("{LABEL} 已经注册。"));
            }
            let target = unsafe { GetApplicationEventTarget() };
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

            let mut key = null_mut();
            let status = unsafe {
                RegisterEventHotKey(
                    SPACE,
                    SHIFT | OPTION,
                    HOTKEY_ID,
                    target,
                    EXCLUSIVE,
                    &mut key,
                )
            };
            if status != 0 {
                unsafe { RemoveEventHandler(handler) };
                return Err(if status == HOTKEY_EXISTS {
                    format!("{LABEL} 已被其他应用占用。")
                } else {
                    format!("无法注册 {LABEL}（macOS 状态码 {status}）。")
                });
            }

            let (sender, receiver) = unbounded_channel();
            EVENTS.with(|events| *events.borrow_mut() = Some(sender));
            Ok((Self { key, handler }, receiver))
        }
    }

    impl Drop for GlobalHotkey {
        fn drop(&mut self) {
            unsafe {
                UnregisterEventHotKey(self.key);
                RemoveEventHandler(self.handler);
            }
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
        if status != 0 || id != HOTKEY_ID {
            return EVENT_NOT_HANDLED;
        }
        let _ = EVENTS.try_with(|events| {
            if let Some(sender) = events.borrow().as_ref() {
                let _ = sender.send(());
            }
        });
        0
    }
}

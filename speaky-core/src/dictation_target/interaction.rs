//! An opaque editor cannot identify a new conversation through UIA. Any user
//! interaction invalidates its lease, even if the same HWND regains focus.
use super::*;
use std::sync::atomic::AtomicU32;
use std::sync::OnceLock;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, PeekMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PM_REMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SYSKEYDOWN,
    WM_XBUTTONDOWN,
};

pub(super) const INPUT_MARKER: usize = 0x5350_4541;
const INTERRUPTED: &str = "聊天、光标或文字已有操作，已停止自动更新；结果保留在 Speaky 中供复制。";
const GUARD_UNAVAILABLE: &str = "无法监测微信输入焦点，转写已保存在 Speaky 中。";
static EPOCH: AtomicU64 = AtomicU64::new(0);
static STOP_KEY: AtomicU32 = AtomicU32::new(0);
static WATCHER: OnceLock<Result<(), String>> = OnceLock::new();

unsafe extern "system" fn keyboard(code: i32, message: usize, data: isize) -> isize {
    if code == HC_ACTION as i32 && matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let event = &*(data as *const KBDLLHOOKSTRUCT);
        if event.dwExtraInfo != INPUT_MARKER
            && !crate::hotkey::is_modifier(event.vkCode as u8)
            && event.vkCode != 0x1b
        // Escape cancels through the recording state machine.
        {
            let binding = STOP_KEY.load(Ordering::Acquire);
            let modifiers = [
                (VK_MENU, 1),
                (VK_CONTROL, 2),
                (VK_SHIFT, 4),
                (VK_LWIN, 8),
                (VK_RWIN, 8),
            ]
            .into_iter()
            .filter(|(key, _)| GetAsyncKeyState(i32::from(*key)) < 0)
            .fold(0, |flags, (_, flag)| flags | flag);
            if binding == 0 || event.vkCode != binding & 0xffff || modifiers != binding >> 16 {
                EPOCH.fetch_add(1, Ordering::AcqRel);
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, message, data)
}

unsafe extern "system" fn mouse(code: i32, message: usize, data: isize) -> isize {
    if code == HC_ACTION as i32
        && matches!(
            message as u32,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_MOUSEWHEEL
                | WM_MOUSEHWHEEL
        )
    {
        let event = &*(data as *const MSLLHOOKSTRUCT);
        let window = windows_sys::Win32::UI::WindowsAndMessaging::WindowFromPoint(event.pt);
        let mut owner = 0;
        GetWindowThreadProcessId(window, &mut owner);
        // The nonactivating Speaky stop/cancel overlay is part of the gesture.
        if event.dwExtraInfo != INPUT_MARKER && owner != std::process::id() {
            EPOCH.fetch_add(1, Ordering::AcqRel);
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, message, data)
}

fn watch() -> Result<(), String> {
    WATCHER
        .get_or_init(|| {
            let (ready, started) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("speaky-draft-guard".into())
                .spawn(move || unsafe {
                    let module = GetModuleHandleW(std::ptr::null());
                    let keyboard_hook =
                        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
                    let mouse_hook =
                        SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0);
                    if keyboard_hook.is_null() || mouse_hook.is_null() {
                        if !keyboard_hook.is_null() { UnhookWindowsHookEx(keyboard_hook); }
                        if !mouse_hook.is_null() { UnhookWindowsHookEx(mouse_hook); }
                        let _ = ready.send(Err(GUARD_UNAVAILABLE.to_owned()));
                        return;
                    }
                    let mut foreground = GetForegroundWindow();
                    let _ = ready.send(Ok(()));
                    loop {
                        let mut message = MSG::default();
                        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0
                        {
                            TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                        let current = GetForegroundWindow();
                        if foreground != current {
                            EPOCH.fetch_add(1, Ordering::AcqRel);
                            foreground = current;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                })
                .map_err(|_| GUARD_UNAVAILABLE.to_owned())?;
            started
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| GUARD_UNAVAILABLE.to_owned())?
        })
        .clone()
}

#[derive(Clone)]
pub(super) struct Guard {
    focus: Focus,
    epoch: u64,
}

impl Guard {
    pub(super) fn snapshot(focus: Focus) -> Self {
        Self {
            focus,
            epoch: EPOCH.load(Ordering::Acquire),
        }
    }

    pub(super) fn start_watching(self) -> Result<Self, String> {
        watch()?;
        let binding = crate::config::Config::load()
            .ok()
            .and_then(|c| c.core.asr.windows_toggle_hotkey)
            .and_then(|s| crate::hotkey::Shortcut::from_id(&s))
            .unwrap_or_default()
            .windows_key()
            .map_or(0, |(key, modifiers)| key | (modifiers & 0x0f) << 16);
        STOP_KEY.store(binding, Ordering::Release);
        self.check()?;
        Ok(self)
    }

    pub(super) fn check(&self) -> Result<(), String> {
        self.focus.require_current()?;
        if EPOCH.load(Ordering::Acquire) == self.epoch {
            Ok(())
        } else {
            Err(INTERRUPTED.into())
        }
    }

    pub(super) fn check_input(&self) -> Result<(), String> {
        self.check()?;
        if modifiers_down() {
            Err(INTERRUPTED.into())
        } else {
            Ok(())
        }
    }
}

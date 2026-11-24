use crate::window::{instance, wide};
use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{Shell::*, WindowsAndMessaging::*},
};

pub const MESSAGE: u32 = WM_APP + 2;
pub const OPEN: usize = 90;
pub const TOGGLE_FLOAT: usize = 91;
pub const EXIT: usize = 92;

pub fn icon() -> HICON {
    unsafe { LoadIconW(instance(), 1 as _) }
}

pub struct Tray(NOTIFYICONDATAW);

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: MESSAGE,
            hIcon: icon(),
            ..Default::default()
        };
        data.szTip[..7].copy_from_slice(&wide("Speaky"));
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &data);
        }
        Self(data)
    }

    pub fn restore(&self) {
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &self.0);
        }
    }

    pub fn menu(&self, action: &str, floating: bool) -> usize {
        unsafe {
            let m = CreatePopupMenu();
            for (id, title) in [
                (crate::window::RECORD, action),
                (OPEN, "打开主窗口"),
                (crate::window::SETTINGS, "配置识别服务"),
                (
                    TOGGLE_FLOAT,
                    if floating {
                        "隐藏悬浮按钮"
                    } else {
                        "显示悬浮按钮"
                    },
                ),
                (EXIT, "退出 Speaky"),
            ] {
                AppendMenuW(m, MF_STRING, id, wide(title).as_ptr());
            }
            let mut cursor = Default::default();
            GetCursorPos(&mut cursor);
            SetForegroundWindow(self.0.hWnd);
            let id = TrackPopupMenu(
                m,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                cursor.x,
                cursor.y,
                0,
                self.0.hWnd,
                null_mut(),
            ) as usize;
            DestroyMenu(m);
            PostMessageW(self.0.hWnd, WM_NULL, 0, 0);
            id
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.0);
        }
    }
}

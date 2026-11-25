use crate::{config::Config, drafts};
use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use std::{cell::Cell, fs, mem::size_of, rc::Rc};
use windows::Win32::{
    Foundation::{
        GetLastError, SetLastError, ERROR_SUCCESS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    },
    Graphics::Gdi::{
        ClientToScreen, GetMonitorInfoW, MonitorFromPoint, HMONITOR, MONITORINFO,
        MONITOR_DEFAULTTONEAREST, MONITOR_FROM_FLAGS,
    },
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

pub(super) fn active_monitor() -> HMONITOR {
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, MonitorFromWindow, HDC, MONITOR_DEFAULTTONEAREST,
    };
    if std::env::var_os("SPEAKY_START_ON_SECONDARY").is_some_and(|v| v == "1") {
        unsafe extern "system" fn secondary(
            monitor: HMONITOR,
            _: HDC,
            _: *mut RECT,
            data: LPARAM,
        ) -> windows::core::BOOL {
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info).as_bool()
                && info.dwFlags & MONITORINFOF_PRIMARY == 0
            {
                *(data.0 as *mut Option<HMONITOR>) = Some(monitor);
                return false.into();
            }
            true.into()
        }
        let mut found: Option<HMONITOR> = None;
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(secondary),
                LPARAM((&mut found as *mut Option<HMONITOR>) as isize),
            );
        }
        if let Some(monitor) = found {
            return monitor;
        }
    }
    unsafe { MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST) }
}

pub(super) fn monitor_scale(monitor: HMONITOR, fallback: f32) -> f32 {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let (mut x, mut y) = (0, 0);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_ok() && x != 0 {
        x as f32 / 96.
    } else {
        fallback
    }
}

pub(super) fn display_id(monitor: HMONITOR, cx: &gpui::App) -> Option<gpui::DisplayId> {
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC};
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> windows::core::BOOL {
        (*(data.0 as *mut Vec<HMONITOR>)).push(monitor);
        true.into()
    }
    let mut monitors = Vec::<HMONITOR>::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM((&mut monitors as *mut Vec<HMONITOR>) as isize),
        );
    }
    let index = monitors
        .iter()
        .position(|candidate| *candidate == monitor)?;
    cx.displays()
        .into_iter()
        .nth(index)
        .map(|display| display.id())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Position {
    pub(super) x: i32,
    pub(super) y: i32,
}

impl Position {
    pub(super) fn clamp_to(self, work: RECT, width: i32, height: i32) -> Self {
        Self {
            x: self.x.clamp(work.left, (work.right - width).max(work.left)),
            y: self.y.clamp(work.top, (work.bottom - height).max(work.top)),
        }
    }
}

pub(super) fn saved_position(filename: &str) -> Option<Position> {
    let data = fs::read(Config::config_dir().join(filename)).ok()?;
    serde_json::from_slice(&data).ok()
}

#[cfg(test)]
#[test]
fn floating_positions_stay_inside_negative_origin_and_small_work_areas() {
    let work = RECT {
        left: -1920,
        top: 165,
        right: 0,
        bottom: 1197,
    };
    assert_eq!(
        Position { x: 200, y: 1400 }.clamp_to(work, 430, 70),
        Position { x: -430, y: 1127 }
    );
    assert_eq!(
        Position { x: -2100, y: -200 }.clamp_to(work, 430, 70),
        Position { x: -1920, y: 165 }
    );
}

pub(super) fn monitor_work_area(monitor: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then_some(info.rcWork)
}

pub(super) fn work_area(point: POINT, policy: MONITOR_FROM_FLAGS) -> Option<RECT> {
    monitor_work_area(unsafe { MonitorFromPoint(point, policy) })
}

pub(super) fn show(
    window: &Window,
    drag: &Rc<WindowDrag>,
    position: Position,
    width: i32,
    height: i32,
) -> Result<(), String> {
    let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
        .map_err(|e| e.to_string())?
        .as_raw()
    else {
        return Err("无法取得悬浮窗句柄。".into());
    };
    let hwnd = HWND(raw.hwnd.get() as _);
    unsafe {
        // GPUI initially reserves frame space. Use the complete client area
        // and accept mouse events while preserving the foreground editor.
        let frame = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        for (field, value) in [
            (
                GWL_STYLE,
                (frame & !(WS_CAPTION.0 | WS_THICKFRAME.0)) | WS_POPUP.0,
            ),
            (
                GWL_EXSTYLE,
                (style | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)
                    & !(WS_EX_APPWINDOW.0 | WS_EX_TRANSPARENT.0),
            ),
        ] {
            SetLastError(ERROR_SUCCESS);
            if SetWindowLongW(hwnd, field, value as i32) == 0 && GetLastError() != ERROR_SUCCESS {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        // The HWND owns one Rc until WM_NCDESTROY. Both native and GPUI
        // callbacks execute on the same desktop UI thread.
        let data = Rc::into_raw(drag.clone());
        if !SetWindowSubclass(hwnd, Some(pointer_proc), 1, data as usize).as_bool() {
            drop(Rc::from_raw(data));
            return Err("无法配置悬浮窗鼠标操作。".into());
        }
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            position.x,
            position.y,
            width,
            height,
            SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
        .map_err(|e| e.to_string())?;
    }
    keep_visible(hwnd);
    Ok(())
}

fn keep_visible(hwnd: HWND) {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return;
    }
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    let origin = Position {
        x: rect.left,
        y: rect.top,
    };
    let center = POINT {
        x: origin.x + width / 2,
        y: origin.y + height / 2,
    };
    if let Some(work) = work_area(center, MONITOR_DEFAULTTONEAREST) {
        let next = origin.clamp_to(work, width, height);
        if next != origin {
            move_window(hwnd, next);
        }
    }
}

fn move_window(hwnd: HWND, position: Position) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            position.x,
            position.y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

#[derive(Clone, Copy)]
struct Press {
    at: POINT,
    origin: Position,
    physical: bool,
}

pub(super) struct WindowDrag {
    filename: &'static str,
    press: Cell<Option<Press>>,
    dragged: Cell<bool>,
}

impl WindowDrag {
    pub(super) fn new(filename: &'static str) -> Self {
        Self {
            filename,
            press: Cell::new(None),
            dragged: Cell::new(false),
        }
    }

    pub(super) fn did_drag(&self) -> bool {
        self.dragged.get()
    }

    fn update_from_mouse(&self, hwnd: HWND, location: LPARAM) {
        let Some(press) = self.press.get() else {
            return;
        };
        if let Some(cursor) = if press.physical {
            cursor_position()
        } else {
            screen_point(hwnd, location)
        } {
            self.move_to(hwnd, press, cursor);
        }
    }

    fn move_to(&self, hwnd: HWND, press: Press, cursor: POINT) {
        let (dx, dy) = (cursor.x - press.at.x, cursor.y - press.at.y);
        if !self.did_drag()
            && dx.abs() < unsafe { GetSystemMetrics(SM_CXDRAG) }.max(1)
            && dy.abs() < unsafe { GetSystemMetrics(SM_CYDRAG) }.max(1)
        {
            return;
        }
        self.dragged.set(true);
        let mut rect = RECT::default();
        let Some(work) = work_area(cursor, MONITOR_DEFAULTTONEAREST) else {
            return;
        };
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
            return;
        }
        let next = Position {
            x: press.origin.x + dx,
            y: press.origin.y + dy,
        }
        .clamp_to(work, rect.right - rect.left, rect.bottom - rect.top);
        if next.x != rect.left || next.y != rect.top {
            move_window(hwnd, next);
        }
    }

    fn finish(&self, hwnd: HWND) {
        if self.press.take().is_none() {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(hwnd), DRAG_TIMER);
        }
        if self.did_drag() {
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                let position = Position {
                    x: rect.left,
                    y: rect.top,
                };
                let path = Config::config_dir().join(self.filename);
                if let Err(error) = serde_json::to_vec(&position)
                    .map_err(std::io::Error::other)
                    .and_then(|data| drafts::write_atomically(&path, &data))
                {
                    log::warn!("无法保存悬浮窗位置：{error}");
                }
            }
        }
    }
}

const DRAG_TIMER: usize = 0x5370;

unsafe extern "system" fn pointer_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    let pointer = &*(data as *const WindowDrag);
    match msg {
        WM_NCCALCSIZE => return LRESULT(0),
        WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE
            if msg != WM_SETTINGCHANGE || w.0 == SPI_SETWORKAREA.0 as usize =>
        {
            let result = DefSubclassProc(hwnd, msg, w, l);
            keep_visible(hwnd);
            return result;
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let mut rect = RECT::default();
            if let Some(at) =
                screen_point(hwnd, l).filter(|_| GetWindowRect(hwnd, &mut rect).is_ok())
            {
                pointer.dragged.set(false);
                let physical = GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0;
                pointer.press.set(Some(Press {
                    at,
                    origin: Position {
                        x: rect.left,
                        y: rect.top,
                    },
                    physical,
                }));
                // A nonactivating HWND may lose mouse events on a fast drag
                // outside its bounds. Follow the held button without capture.
                if physical {
                    SetTimer(Some(hwnd), DRAG_TIMER, 16, None);
                }
            }
        }
        WM_MOUSEMOVE => pointer.update_from_mouse(hwnd, l),
        WM_TIMER if w.0 == DRAG_TIMER => {
            if GetAsyncKeyState(VK_LBUTTON.0 as i32) >= 0 {
                pointer.finish(hwnd);
            } else if let (Some(press), Some(cursor)) = (pointer.press.get(), cursor_position()) {
                pointer.move_to(hwnd, press, cursor);
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            if let (Some(press), Some(cursor)) = (pointer.press.get(), screen_point(hwnd, l)) {
                pointer.move_to(hwnd, press, cursor);
            }
            pointer.finish(hwnd);
        }
        WM_CANCELMODE | WM_CAPTURECHANGED | WM_DESTROY => pointer.finish(hwnd),
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(pointer_proc), id);
            drop(Rc::from_raw(data as *const WindowDrag));
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, w, l)
}

fn cursor_position() -> Option<POINT> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok().map(|()| point)
}

fn screen_point(hwnd: HWND, location: LPARAM) -> Option<POINT> {
    let mut point = POINT {
        x: location.0 as i16 as i32,
        y: (location.0 >> 16) as i16 as i32,
    };
    unsafe { ClientToScreen(hwnd, &mut point) }
        .as_bool()
        .then_some(point)
}

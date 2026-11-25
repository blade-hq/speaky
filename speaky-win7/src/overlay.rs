use crate::{
    d2d::{fill, rect, Graphics, Surface},
    window,
};
use speaky_core::{audio::VoiceActivity, config::Config, drafts, wave::VoiceWave};
use std::{
    fs,
    mem::size_of,
    ptr::null_mut,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::*,
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON},
        WindowsAndMessaging::*,
    },
};

pub const WIDTH: i32 = 420;
pub const HEIGHT: i32 = 96;

pub struct Layer {
    pub hwnd: HWND,
    pub surface: Surface,
    size: (i32, i32),
    scale: f32,
    file: &'static str,
    drag: Option<(POINT, RECT, bool)>,
}

impl Layer {
    pub fn new(
        owner: HWND,
        graphics: &Graphics,
        width: i32,
        height: i32,
        file: &'static str,
    ) -> Result<Self, String> {
        let surface = graphics.surface(width, height).map_err(|e| e.to_string())?;
        let size = (
            window::px(graphics.scale, width),
            window::px(graphics.scale, height),
        );
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                windows_sys::w!("SpeakyWin7Layer"),
                windows_sys::w!("Speaky"),
                WS_POPUP,
                0,
                0,
                size.0,
                size.1,
                owner,
                null_mut(),
                window::instance(),
                null_mut(),
            );
            if hwnd.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let layer = Self {
                hwnd,
                surface,
                size,
                scale: graphics.scale,
                file,
                drag: None,
            };
            let area = work_area(GetForegroundWindow());
            let persisted = fs::read_to_string(Config::config_dir().join(file))
                .ok()
                .and_then(|pos| {
                    let (x, y) = pos.split_once(',')?;
                    Some((x.parse().ok()?, y.parse().ok()?))
                });
            let (x, y) = persisted.unwrap_or((
                area.right - size.0 - 24,
                area.bottom
                    - size.1
                    - if file == "win7-overlay-position" {
                        76
                    } else {
                        20
                    },
            ));
            layer.position(x, y);
            Ok(layer)
        }
    }

    pub fn show(&self, visible: bool) {
        unsafe {
            ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }

    fn position(&self, x: i32, y: i32) {
        unsafe {
            let proposed = RECT {
                left: x,
                top: y,
                right: x.saturating_add(self.size.0),
                bottom: y.saturating_add(self.size.1),
            };
            let monitor = MonitorFromRect(&proposed, MONITOR_DEFAULTTONEAREST);
            let area = monitor_area(monitor);
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x.clamp(area.left, (area.right - self.size.0).max(area.left)),
                y.clamp(area.top, (area.bottom - self.size.1).max(area.top)),
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    pub fn pointer_down(&mut self) {
        unsafe {
            let mut cursor = POINT::default();
            let mut bounds = RECT::default();
            GetCursorPos(&mut cursor);
            GetWindowRect(self.hwnd, &mut bounds);
            self.drag = Some((cursor, bounds, false));
        }
    }

    pub fn cancel_hit(&self) -> bool {
        unsafe {
            let mut cursor = POINT::default();
            let mut bounds = RECT::default();
            GetCursorPos(&mut cursor);
            GetWindowRect(self.hwnd, &mut bounds);
            (cursor.x - bounds.left) as f32 / self.scale < 48.
        }
    }

    pub fn pointer_update(&mut self, released: bool, overlay: bool) -> bool {
        let Some((start, bounds, mut moved)) = self.drag.take() else {
            return false;
        };
        unsafe {
            let mut cursor = POINT::default();
            GetCursorPos(&mut cursor);
            let dx = cursor.x - start.x;
            let dy = cursor.y - start.y;
            moved |= dx.abs().max(dy.abs()) >= window::px(self.scale, 4);
            if moved {
                self.position(bounds.left + dx, bounds.top + dy);
            }
            // A background window cannot capture beyond its bounds without stealing focus.
            if !released && GetAsyncKeyState(i32::from(VK_LBUTTON)) < 0 {
                self.drag = Some((start, bounds, moved));
                return false;
            }
            if moved {
                let mut bounds = RECT::default();
                GetWindowRect(self.hwnd, &mut bounds);
                let path = Config::config_dir().join(self.file);
                std::thread::spawn(move || {
                    let _ = drafts::write_atomically(
                        &path,
                        format!("{},{}", bounds.left, bounds.top).as_bytes(),
                    );
                });
                return false;
            }
            let x = (cursor.x - bounds.left) as f32 / self.scale;
            let y = (cursor.y - bounds.top) as f32 / self.scale;
            if overlay {
                ((12. ..42.).contains(&x) || (305. ..408.).contains(&x)) && (12. ..42.).contains(&y)
            } else {
                (0. ..self.size.0 as f32 / self.scale).contains(&x) && (0. ..40.).contains(&y)
            }
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}

unsafe fn work_area(hwnd: HWND) -> RECT {
    monitor_area(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST))
}

unsafe fn monitor_area(monitor: HMONITOR) -> RECT {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info) == 0 {
        return RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        };
    }
    info.rcWork
}

pub struct Overlay {
    pub layer: Layer,
    wave: VoiceWave,
    last: Instant,
    slow: u8,
    period: u32,
}

impl Overlay {
    pub fn new(owner: HWND, graphics: &Graphics) -> Result<Self, String> {
        Ok(Self {
            layer: Layer::new(owner, graphics, WIDTH, HEIGHT, "win7-overlay-position")?,
            wave: VoiceWave::default(),
            last: Instant::now(),
            slow: 0,
            period: 16,
        })
    }

    pub fn render(
        &mut self,
        graphics: &Graphics,
        activity: VoiceActivity,
        elapsed: u64,
        caption: &str,
    ) -> windows::core::Result<u32> {
        let now = Instant::now();
        self.wave
            .update_dt(activity, now.duration_since(self.last).as_secs_f32());
        self.last = now;
        let caption: String = caption
            .trim()
            .chars()
            .rev()
            .take(26)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let wave = self.wave;
        self.layer.surface.draw(self.layer.hwnd, |target| {
            fill(
                target,
                rect(0., 0., WIDTH as f32, HEIGHT as f32),
                16.,
                0x171e2d,
                0.94,
            )?;
            fill(target, rect(305., 12., 408., 42.), 9., 0x344558, 1.)?;
            graphics.text(target, "✓ 停止并保存", rect(308., 17., 407., 42.))?;
            graphics.text(target, "✕", rect(15., 17., 42., 42.))?;
            graphics.text(target, &clock(elapsed), rect(56., 13., 118., 38.))?;
            graphics.wave(target, wave, WIDTH as f32, 39.)?;
            graphics.text(
                target,
                if caption.is_empty() {
                    "等待语音…"
                } else {
                    &caption
                },
                rect(15., 68., 405., 93.),
            )
        })?;
        if now.elapsed() >= Duration::from_millis(16) {
            self.slow = self.slow.saturating_add(1);
            if self.slow >= 3 {
                self.period = 33;
            }
        } else {
            self.slow = 0;
        }
        Ok(self.period)
    }
}

pub fn clock(seconds: u64) -> String {
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

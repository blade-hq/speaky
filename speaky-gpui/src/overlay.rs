use crate::audio::VoiceActivity;
#[cfg(windows)]
use crate::windows_floating::{self, Position, WindowDrag};
use gpui::{
    div, point, prelude::*, px, rgb, rgba, size, App, Bounds, Context, Pixels, Size, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use std::{rc::Rc, time::Instant};

mod wave;
use speaky_core::wave::VoiceWave;

const ACCENT: u32 = 0x9eaaff;
const CORNER_RADIUS: f32 = 20.;
#[cfg(windows)]
const POSITION_FILE: &str = "caption-position.json";

pub(super) struct RecordingOverlay {
    started_at: Instant,
    elapsed_seconds: u64,
    panel_size: Size<Pixels>,
    wave: VoiceWave,
    show_controls: bool,
    show_caption: bool,
    caption: String,
    on_finish: Rc<dyn Fn(&mut App)>,
    on_cancel: Rc<dyn Fn(&mut App)>,
    #[cfg(windows)]
    drag: Rc<WindowDrag>,
}

impl RecordingOverlay {
    pub(super) fn open(
        cx: &mut App,
        show_controls: bool,
        show_caption: bool,
        on_finish: impl Fn(&mut App) + 'static,
        on_cancel: impl Fn(&mut App) + 'static,
    ) -> Result<WindowHandle<Self>, String> {
        #[cfg(windows)]
        let display = windows_floating::display_id(windows_floating::active_monitor(), cx)
            .and_then(|id| cx.displays().into_iter().find(|display| display.id() == id))
            .or_else(|| cx.primary_display());
        #[cfg(not(windows))]
        let display = cx.primary_display();
        let display = display.ok_or_else(|| "未找到可显示录音悬浮窗的屏幕。".to_owned())?;
        let screen = display.bounds();
        let panel = if show_caption {
            size(
                px(476.).min(screen.size.width),
                px(if show_controls { 94. } else { 86. }),
            )
        } else {
            size(px(224.).min(screen.size.width), px(48.))
        };
        let bounds = Bounds::new(
            point(
                screen.origin.x + (screen.size.width - panel.width) / 2.,
                screen.origin.y + (screen.size.height - panel.height - px(84.)).max(px(0.)),
            ),
            panel,
        );
        #[cfg(windows)]
        let drag = Rc::new(WindowDrag::new(POSITION_FILE));
        let mut native_result: Result<(), String> = Ok(());
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: None,
                    focus: false,
                    show: cfg!(not(any(target_os = "macos", windows))),
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    display_id: Some(display.id()),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title(if show_controls {
                        "Speaky · 录音"
                    } else {
                        "Speaky · 实时字幕"
                    });
                    #[cfg(windows)]
                    {
                        native_result = show_panel(window, &bounds, &drag, Some(POSITION_FILE));
                    }
                    #[cfg(not(windows))]
                    {
                        native_result = show_panel(window, &bounds, show_controls);
                    }
                    cx.new(|_| Self {
                        started_at: Instant::now(),
                        elapsed_seconds: 0,
                        panel_size: panel,
                        wave: VoiceWave::default(),
                        show_controls,
                        show_caption,
                        caption: if show_caption {
                            "正在聆听…".into()
                        } else {
                            String::new()
                        },
                        on_finish: Rc::new(on_finish),
                        on_cancel: Rc::new(on_cancel),
                        #[cfg(windows)]
                        drag,
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        if let Err(error) = native_result {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return Err(error);
        }
        #[cfg(windows)]
        {
            let executor = cx.background_executor().clone();
            cx.spawn(async move |cx| {
                executor.timer(std::time::Duration::from_millis(120)).await;
                let _ = handle.update(cx, |_, window, _| window.resize(panel));
            })
            .detach();
        }
        Ok(handle)
    }

    pub(super) fn push_activity(
        &mut self,
        activity: VoiceActivity,
        caption: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        // Only the last two lines are visible. Keep the work per update independent
        // of the total length of the recording, including for multibyte text.
        let tail = caption_tail(caption.unwrap_or_default());
        let caption_changed = self.caption != tail;
        if caption_changed {
            self.caption.clear();
            self.caption.push_str(tail);
        }
        let seconds = self.started_at.elapsed().as_secs();
        let timer_changed = self.show_controls && seconds != self.elapsed_seconds;
        self.elapsed_seconds = seconds;
        if self.wave.update(activity) || caption_changed || timer_changed {
            cx.notify();
        }
    }
}

fn caption_tail(text: &str) -> &str {
    match text.char_indices().rev().nth(191) {
        Some((index, _)) => &text[index..],
        None => text,
    }
}

struct StopTooltip;

impl Render for StopTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x46506b))
            .bg(rgb(0x252c40))
            .text_color(rgb(0xeff2ff))
            .text_xs()
            .child("停止并保存")
    }
}

impl Render for RecordingOverlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let seconds = self.elapsed_seconds;
        let timer = format!("{:02}:{:02}", seconds / 60, seconds % 60);
        let on_finish = self.on_finish.clone();
        let on_cancel = self.on_cancel.clone();
        #[cfg(windows)]
        let cancel_drag = self.drag.clone();
        #[cfg(windows)]
        let drag = self.drag.clone();
        let controls = div()
            .w_full()
            .h(px(30.))
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("overlay-cancel")
                    .size(px(26.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .rounded_full()
                    .hover(|s| s.bg(rgba(0xffffff20)))
                    .child(crate::icons::Icon::Close.render(0xc4cce3))
                    .on_click(move |_, _, cx| {
                        #[cfg(windows)]
                        if cancel_drag.did_drag() {
                            return;
                        }
                        let on_cancel = on_cancel.clone();
                        cx.defer(move |cx| on_cancel(cx));
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))
                    .child(div().text_xs().text_color(rgb(0xc4cce3)).child(timer)),
            )
            .child(div().flex_1().min_w(px(0.)).child(wave::element(self.wave)))
            .child(
                div()
                    .id("overlay-finish")
                    .size(px(26.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .rounded_full()
                    .bg(rgba(0x9eaaff26))
                    .hover(|style| style.bg(rgba(0x9eaaff40)))
                    .child(crate::icons::Icon::Check.render(0xeff2ff))
                    .tooltip(|_, cx| cx.new(|_| StopTooltip).into())
                    .on_click(move |_, _, cx| {
                        #[cfg(windows)]
                        if drag.did_drag() {
                            return;
                        }
                        let on_finish = on_finish.clone();
                        cx.defer(move |cx| on_finish(cx));
                    }),
            );
        // A nonactivating HWND can retain GPUI's larger initial viewport. Lay
        // out against the same logical bounds as the native caption window.
        div()
            .w(self.panel_size.width)
            .h(self.panel_size.height)
            .px_3()
            .flex()
            .flex_col()
            .justify_center()
            .gap_1()
            .overflow_hidden()
            .rounded(px(CORNER_RADIUS))
            .border_1()
            .border_color(rgba(0xa6b7ff3c))
            .bg(rgba(0x181c28f0))
            .when(cfg!(windows), |panel| panel.cursor_grab())
            .when(self.show_caption, |panel| {
                panel.child(
                    div()
                        .w_full()
                        .h(px(42.))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .justify_end()
                        .overflow_hidden()
                        .child(
                            div()
                                .w_full()
                                .flex_none()
                                .text_size(px(15.))
                                .line_height(px(21.))
                                .text_color(rgb(0xeff2ff))
                                .child(self.caption.clone()),
                        ),
                )
            })
            .when(self.show_caption && !self.show_controls, |panel| {
                panel.child(wave::element(self.wave))
            })
            .when(self.show_controls, |panel| panel.child(controls))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn show_panel(_: &Window, _: &Bounds<Pixels>, _: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub(super) fn show_panel(
    window: &Window,
    bounds: &Bounds<Pixels>,
    drag: &Rc<WindowDrag>,
    position_file: Option<&str>,
) -> Result<(), String> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL},
        UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
    };

    let monitor = windows_floating::active_monitor();
    let saved = position_file
        .and_then(windows_floating::saved_position)
        .and_then(|position| {
            let point = POINT {
                x: position.x,
                y: position.y,
            };
            let saved_monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
            (saved_monitor == monitor).then_some((position, monitor))
        });
    let work =
        windows_floating::monitor_work_area(monitor).ok_or("无法取得字幕悬浮窗的工作区。")?;
    let (mut dpi_x, mut dpi_y) = (0, 0);
    let scale = if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
        .is_ok()
        && dpi_x != 0
    {
        dpi_x as f32 / 96.
    } else {
        window.scale_factor()
    };
    let device_px = |length: Pixels| (f32::from(length) * scale).round() as i32;
    let width = device_px(bounds.size.width).min(work.right - work.left);
    let height = device_px(bounds.size.height).min(work.bottom - work.top);
    let position = saved
        .map(|(position, _)| position)
        .unwrap_or(Position {
            x: work.left + (work.right - work.left - width) / 2,
            y: work.bottom - height - device_px(px(56.)),
        })
        .clamp_to(work, width, height);
    windows_floating::show(window, drag, position, width, height)
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
pub(super) fn show_panel(
    window: &Window,
    _: &Bounds<Pixels>,
    controls: bool,
) -> Result<(), String> {
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use objc::{
        class, msg_send,
        runtime::{Object, NO, YES},
        sel, sel_impl,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    const HUD_MATERIAL: isize = 13;
    const BEHIND_WINDOW: isize = 0;
    const EFFECT_ACTIVE: isize = 1;
    const WINDOW_BELOW: isize = -1;
    const VIEW_SIZABLE: usize = (1 << 1) | (1 << 4);

    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("无法获取录音悬浮窗的 macOS 句柄。".into());
    };
    let view = handle.ns_view.cast::<Object>().as_ptr();
    unsafe {
        let panel: *mut Object = msg_send![view, window];
        if panel.is_null() {
            return Err("无法打开录音悬浮窗。".into());
        }
        let content: *mut Object = msg_send![panel, contentView];
        let rect: CGRect = msg_send![content, bounds];
        let effect: *mut Object = msg_send![class!(NSVisualEffectView), alloc];
        let effect: *mut Object = msg_send![effect, initWithFrame: rect];
        let _: () = msg_send![effect, setMaterial: HUD_MATERIAL];
        let _: () = msg_send![effect, setBlendingMode: BEHIND_WINDOW];
        let _: () = msg_send![effect, setState: EFFECT_ACTIVE];
        let _: () = msg_send![effect, setAutoresizingMask: VIEW_SIZABLE];
        let dark_aqua: *mut Object = msg_send![
            class!(NSString),
            stringWithUTF8String: c"NSAppearanceNameDarkAqua".as_ptr()
        ];
        let appearance: *mut Object = msg_send![class!(NSAppearance), appearanceNamed: dark_aqua];
        let _: () = msg_send![effect, setAppearance: appearance];
        let _: () = msg_send![effect, setWantsLayer: YES];
        let layer: *mut Object = msg_send![effect, layer];
        let _: () = msg_send![layer, setCornerRadius: f64::from(CORNER_RADIUS)];
        let _: () = msg_send![layer, setMasksToBounds: YES];
        let _: () =
            msg_send![content, addSubview: effect positioned: WINDOW_BELOW relativeTo: view];
        let _: () = msg_send![effect, release];

        let clear: *mut Object = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![panel, setBackgroundColor: clear];
        let _: () = msg_send![panel, setHasShadow: YES];
        let _: () = msg_send![panel, setHidesOnDeactivate: NO];
        let _: () = msg_send![panel, setBecomesKeyOnlyIfNeeded: YES];
        let ignore_mouse = if controls { NO } else { YES };
        let _: () = msg_send![panel, setIgnoresMouseEvents: ignore_mouse];
        // NSFloatingWindowLevel
        let _: () = msg_send![panel, setLevel: 3isize];
        // Cocoa screen origins may be negative. Match the foreground window first;
        // the pointer is only a fallback when accessibility cannot expose its frame.
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        let mouse: CGPoint = msg_send![class!(NSEvent), mouseLocation];
        let mut screen: *mut Object = msg_send![class!(NSScreen), mainScreen];
        let foreground = crate::mac_dictation::focused_window_bounds().and_then(|mut frame| {
            if count == 0 {
                return None;
            }
            let primary: *mut Object = msg_send![screens, objectAtIndex: 0usize];
            let primary: CGRect = msg_send![primary, frame];
            // Accessibility uses a top-left origin; NSScreen uses a bottom-left origin.
            frame.origin.y =
                primary.origin.y + primary.size.height - frame.origin.y - frame.size.height;
            Some(frame)
        });
        let mut best_overlap = 0.;
        let mut pointer_screen = None;
        for index in 0..count {
            let candidate: *mut Object = msg_send![screens, objectAtIndex: index];
            let frame: CGRect = msg_send![candidate, frame];
            if frame.contains(&mouse) {
                pointer_screen = Some(candidate);
            }
            if let Some(foreground) = foreground {
                let width = ((frame.origin.x + frame.size.width)
                    .min(foreground.origin.x + foreground.size.width)
                    - frame.origin.x.max(foreground.origin.x))
                .max(0.);
                let height = ((frame.origin.y + frame.size.height)
                    .min(foreground.origin.y + foreground.size.height)
                    - frame.origin.y.max(foreground.origin.y))
                .max(0.);
                let overlap = width * height;
                if overlap > best_overlap {
                    best_overlap = overlap;
                    screen = candidate;
                }
            }
        }
        if best_overlap == 0. {
            if let Some(pointer_screen) = pointer_screen {
                screen = pointer_screen;
            }
        }
        if !screen.is_null() {
            let work: CGRect = msg_send![screen, visibleFrame];
            let frame: CGRect = msg_send![panel, frame];
            let dimensions = CGSize::new(
                frame.size.width.min(work.size.width),
                frame.size.height.min(work.size.height),
            );
            let origin = CGPoint::new(
                work.origin.x + (work.size.width - dimensions.width) / 2.,
                work.origin.y + 56f64.min((work.size.height - dimensions.height).max(0.)),
            );
            let frame = CGRect::new(&origin, &dimensions);
            let _: () = msg_send![panel, setFrame: frame display: YES];
        }
        let _: () = msg_send![panel, orderFrontRegardless];
    }
    Ok(())
}

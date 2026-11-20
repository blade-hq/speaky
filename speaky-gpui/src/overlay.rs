use gpui::{
    div, point, prelude::*, px, rgb, rgba, size, App, Bounds, Context, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use std::{rc::Rc, time::Instant};

const WAVEFORM_BARS: usize = 20;
const ACCENT: u32 = 0x9eaaff;
const CORNER_RADIUS: f32 = 20.;

pub(super) struct RecordingOverlay {
    started_at: Instant,
    levels: [f32; WAVEFORM_BARS],
    on_finish: Rc<dyn Fn(&mut App)>,
}

impl RecordingOverlay {
    pub(super) fn open(
        cx: &mut App,
        on_finish: impl Fn(&mut App) + 'static,
    ) -> Result<WindowHandle<Self>, String> {
        let display = cx
            .primary_display()
            .ok_or_else(|| "未找到可显示录音悬浮窗的屏幕。".to_owned())?;
        let screen = display.bounds();
        let panel = size(px(224.), px(48.));
        let bounds = Bounds::new(
            point(
                (screen.size.width - panel.width) / 2.,
                (screen.size.height - panel.height - px(84.)).max(px(0.)),
            ),
            panel,
        );
        let mut native_result = Ok(());
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: None,
                    focus: false,
                    show: cfg!(not(target_os = "macos")),
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    display_id: Some(display.id()),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Speaky · 录音");
                    #[cfg(target_os = "macos")]
                    {
                        native_result = show_panel(window);
                    }
                    cx.new(|_| Self {
                        started_at: Instant::now(),
                        levels: [0.; WAVEFORM_BARS],
                        on_finish: Rc::new(on_finish),
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        if let Err(error) = native_result {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return Err(error);
        }
        Ok(handle)
    }

    pub(super) fn push_level(&mut self, level: f32, cx: &mut Context<Self>) {
        self.levels.rotate_left(1);
        self.levels[WAVEFORM_BARS - 1] = level.clamp(0., 1.);
        cx.notify();
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
        let seconds = self.started_at.elapsed().as_secs();
        let timer = format!("{:02}:{:02}", seconds / 60, seconds % 60);
        let on_finish = self.on_finish.clone();
        div()
            .size_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(px(CORNER_RADIUS))
            .border_1()
            .border_color(rgba(0xa6b7ff3c))
            .bg(rgba(0x181c28b8))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))
                    .child(div().text_xs().text_color(rgb(0xc4cce3)).child(timer)),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .children(self.levels.iter().map(|level| {
                        let height = 3. + 21. * (level * 5.).sqrt().min(1.);
                        div().w(px(2.)).h(px(height)).rounded_full().bg(rgb(ACCENT))
                    })),
            )
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
                    .child(div().size(px(10.)).rounded(px(1.5)).bg(rgb(0xeff2ff)))
                    .tooltip(|_, cx| cx.new(|_| StopTooltip).into())
                    .on_click(move |_, _, cx| {
                        let on_finish = on_finish.clone();
                        cx.defer(move |cx| on_finish(cx));
                    }),
            )
    }
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn show_panel(window: &Window) -> Result<(), String> {
    use objc::{
        class, msg_send,
        runtime::{Object, NO, YES},
        sel, sel_impl,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[repr(C)]
    struct Rect {
        origin: [f64; 2],
        size: [f64; 2],
    }

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
        let rect: Rect = msg_send![content, bounds];
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
        let _: () = msg_send![panel, orderFrontRegardless];
    }
    Ok(())
}

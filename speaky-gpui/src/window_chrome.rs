use gpui::{
    canvas, div, point, prelude::*, px, rgb, App, Div, PathBuilder, Stateful, Window,
    WindowControlArea,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
        DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    },
};

use crate::{theme, ActionTooltip, BACKGROUND, INK, SIDEBAR};

pub(super) fn hide_frame_border(window: &Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(handle.hwnd.get() as _);
    let dark_mode = i32::from(matches!(theme::current(), theme::Theme::Dark));
    // COLORREF uses BGR byte order, unlike the RGB values used by GPUI.
    let background = BACKGROUND();
    let caption_color =
        ((background & 0xff) << 16) | (background & 0x00ff00) | ((background >> 16) & 0xff);
    // GPUI keeps a 1–2 pixel nonclient strip above its custom titlebar on Win11.
    // Color that strip using the app theme, independently of the system theme.
    // Keep native resize hit targets, shadows and Snap behavior unchanged.
    const COLOR_NONE: u32 = 0xffff_fffe;
    unsafe {
        if DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark_mode as *const i32).cast(),
            size_of::<i32>() as u32,
        )
        .is_err()
        {
            // Older Win10 releases used attribute 19 for immersive dark mode.
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(19),
                (&dark_mode as *const i32).cast(),
                size_of::<i32>() as u32,
            );
        }
        // Win10 may ignore these Win11 color attributes; that is harmless.
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            (&caption_color as *const u32).cast(),
            size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&COLOR_NONE as *const u32).cast(),
            size_of::<u32>() as u32,
        );
    }
}

pub(super) fn refresh_frames(cx: &mut App) {
    let windows: Vec<_> = cx
        .windows()
        .into_iter()
        // Transparent recording/clipboard popups must retain their own framing.
        .filter(|window| window.downcast::<crate::Speaky>().is_some())
        .collect();
    // Theme changes can originate inside the main window's update callback.
    // Wait until that borrow ends before updating its native frame.
    cx.defer(move |cx| {
        for window in windows {
            let _ = window.update(cx, |_, window, _| hide_frame_border(window));
        }
    });
}

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl Control {
    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Minimize => "最小化",
            Self::Maximize => "最大化",
            Self::Restore => "还原窗口",
            Self::Close => "关闭 Speaky",
        }
    }

    fn glyph(self) -> impl IntoElement {
        let lines: &[&[(f32, f32)]] = match self {
            Self::Minimize => &[&[(3., 8.), (13., 8.)]],
            Self::Maximize => &[&[
                (3.5, 3.5),
                (12.5, 3.5),
                (12.5, 12.5),
                (3.5, 12.5),
                (3.5, 3.5),
            ]],
            Self::Restore => &[
                &[(3., 5.5), (10.5, 5.5), (10.5, 13.), (3., 13.), (3., 5.5)],
                &[(5.5, 5.5), (5.5, 3.), (13., 3.), (13., 10.5), (10.5, 10.5)],
            ],
            Self::Close => &[&[(3.5, 3.5), (12.5, 12.5)], &[(12.5, 3.5), (3.5, 12.5)]],
        };
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let mut path = PathBuilder::stroke(px(1.25));
                for line in lines {
                    let ((x, y), rest) = line.split_first().unwrap();
                    path.move_to(bounds.origin + point(px(*x), px(*y)));
                    for &(x, y) in rest {
                        path.line_to(bounds.origin + point(px(x), px(y)));
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(INK()));
                }
            },
        )
        .size(px(16.))
    }

    fn button(self) -> Stateful<Div> {
        let label = self.label();
        let hover = if matches!(self, Self::Close) {
            match theme::current() {
                theme::Theme::Dark => 0x58343a,
                theme::Theme::Light => 0xf5e1dd,
            }
        } else {
            SIDEBAR()
        };
        div()
            .id(label)
            .w(px(36.))
            .h(px(27.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .hover(move |button| button.bg(rgb(hover)))
            // Native caption hits retain Windows' Snap Layouts and system menu.
            .window_control_area(self.area())
            .tooltip(move |_, cx| cx.new(|_| ActionTooltip(label.into())).into())
            .child(self.glyph())
    }
}

pub(super) fn header(maximized: bool, theme_control: Div) -> Div {
    div()
        .h(px(32.))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .pr_2()
        .child(
            div()
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag),
        )
        .child(theme_control.flex_none())
        .child(Control::Minimize.button())
        .child(
            if maximized {
                Control::Restore
            } else {
                Control::Maximize
            }
            .button(),
        )
        .child(Control::Close.button())
}

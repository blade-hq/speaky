use gpui::{
    canvas, div, point, prelude::*, px, rgb, Div, PathBuilder, Stateful, Window,
    WindowControlArea,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR},
};

use crate::{ActionTooltip, INK, SIDEBAR};

pub(super) fn hide_frame_border(window: &Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    // The system retains its resize hit targets and shadow. Older Windows
    // builds can ignore the color attribute and still use the custom header.
    const COLOR_NONE: u32 = 0xffff_fffe;
    let _ = unsafe {
        DwmSetWindowAttribute(
            HWND(handle.hwnd.get() as _),
            DWMWA_BORDER_COLOR,
            (&COLOR_NONE as *const u32).cast(),
            size_of::<u32>() as u32,
        )
    };
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
                    window.paint_path(path, rgb(INK));
                }
            },
        )
        .size(px(16.))
    }

    fn button(self) -> Stateful<Div> {
        let label = self.label();
        let hover = if matches!(self, Self::Close) {
            0xf5e1dd
        } else {
            SIDEBAR
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

pub(super) fn header(maximized: bool) -> Div {
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

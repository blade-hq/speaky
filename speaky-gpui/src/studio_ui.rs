use gpui::{div, img, prelude::*, px, rgb, rgba, Div, Image, ImageFormat};
use once_cell::sync::Lazy;
use std::sync::Arc;

use crate::{ACCENT, BORDER, INK, RAIL};

static MARK: Lazy<Arc<Image>> = Lazy::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../assets/speaky-mark.png").to_vec(),
    ))
});

pub(crate) fn keycap(label: &str) -> Div {
    div()
        .px_2()
        .py_1()
        .rounded(px(6.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(RAIL))
        .text_color(rgb(INK))
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(label.to_owned())
}

pub(crate) fn mark() -> Div {
    div()
        .size(px(35.))
        .flex_none()
        .child(img(MARK.clone()).size_full())
}

/// A visual signature for the recording surface and the welcome screen.
pub(crate) fn signal(height: f32, strength: f32) -> Div {
    const ENVELOPE: [f32; 33] = [
        0.05, 0.05, 0.08, 0.13, 0.24, 0.36, 0.52, 0.38, 0.16, 0.18, 0.30, 0.53, 0.78, 0.96, 0.83,
        0.60, 0.34, 0.17, 0.28, 0.50, 0.72, 0.85, 0.63, 0.44, 0.18, 0.16, 0.27, 0.37, 0.26, 0.16,
        0.09, 0.05, 0.05,
    ];
    div()
        .relative()
        .h(px(height))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(div().absolute().w_full().h(px(1.)).bg(rgba(0x2858482b)))
        .child(
            div()
                .relative()
                .w_full()
                .max_w(px(340.))
                .px_2()
                .flex()
                .justify_between()
                .items_center()
                .children(ENVELOPE.map(|level| {
                    div()
                        .w(px(2.))
                        .h(px(3. + (height - 5.) * level * strength))
                        .rounded_full()
                        .bg(rgb(ACCENT))
                        .opacity(0.30 + 0.66 * level)
                })),
        )
}

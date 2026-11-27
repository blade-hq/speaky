use gpui::{div, img, prelude::*, px, rgb, Div, Image, ImageFormat};
use once_cell::sync::Lazy;
use std::sync::Arc;

use crate::{BORDER, INK, RAIL};

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
        .border_color(rgb(BORDER()))
        .bg(rgb(RAIL()))
        .text_color(rgb(INK()))
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

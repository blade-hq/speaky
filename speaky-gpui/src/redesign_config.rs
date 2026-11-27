use crate::{
    button,
    settings::{detail, ModelTestFeedback},
    ActionTooltip, ACCENT, BORDER, ERROR, INK, MUTED, SELECTION, SIDEBAR, SURFACE,
};
use gpui::{div, point, prelude::*, px, rgb, rgba, BoxShadow, Div, FontWeight, SharedString};

#[derive(Default)]
pub(super) struct RedesignConfig {
    pub(super) rewrite: bool,
    pub(super) advanced: bool,
    pub(super) rewrite_preferences: bool,
    pub(super) try_download: bool,
}

pub(super) fn catalog() -> Div {
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(14.))
        .border_1()
        .border_color(rgb(BORDER()))
        .bg(rgb(SURFACE()))
}

pub(super) fn header(quality: &'static str, purpose: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(rgb(SIDEBAR()))
        .text_size(px(12.))
        .text_color(rgb(MUTED()))
        .child(div().flex_1().child("模型"))
        .child(div().w(px(55.)).child(quality))
        .child(div().w(px(50.)).child("耗时"))
        .child(div().w(px(76.)).child(purpose))
}

fn badge(label: impl Into<SharedString>, active: bool) -> Div {
    div()
        .flex_none()
        .rounded(px(10.))
        .px_2()
        .py(px(1.))
        .text_size(px(10.))
        .bg(rgb(if active { SELECTION() } else { SIDEBAR() }))
        .text_color(rgb(if active { ACCENT() } else { MUTED() }))
        .child(label.into())
}

pub(super) fn model_row(
    name: String,
    local: bool,
    selected: bool,
    status: &'static str,
    info: impl Into<SharedString>,
    purpose: &'static str,
    feedback: Option<&ModelTestFeedback>,
    actions: Div,
) -> Div {
    let info = info.into();
    let sample = feedback.and_then(|result| result.metrics.as_ref());
    let sample_details_id = SharedString::from(format!("catalog-sample-details-{purpose}-{name}"));
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(rgb(BORDER()))
        .when(selected, |row| row.bg(rgb(SELECTION())))
        .child(
            div()
                .size(px(13.))
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(rgb(if selected { ACCENT() } else { MUTED() }))
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |dot| {
                    dot.child(div().size(px(7.)).rounded_full().bg(rgb(ACCENT())))
                }),
        )
        .child(
            div()
                .min_w(px(0.))
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(rgb(INK()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(badge(
                            if local {
                                "本地模型"
                            } else {
                                "在线模型"
                            },
                            false,
                        ))
                        .when(!status.is_empty(), |row| row.child(badge(status, selected))),
                )
                .when(!info.is_empty(), |row| row.child(detail(info)))
                .child(actions),
        )
        .child({
            let error = feedback.is_some_and(|result| result.notice.is_err());
            let label = if error {
                "失败"
            } else {
                sample.map_or("—", |(label, _)| *label)
            };
            detail(label)
                .w(px(55.))
                .when(error, |label| label.text_color(rgb(ERROR())))
                .id(sample_details_id)
                .when_some(feedback, |label, result| {
                    let message = match &result.notice {
                        Ok(message) | Err(message) => message.clone(),
                    };
                    label.tooltip(move |_, cx| {
                        cx.new(|_| ActionTooltip(message.clone().into())).into()
                    })
                })
        })
        .child(div().w(px(50.)).child(detail(
            sample.map_or_else(|| "—".to_owned(), |(_, elapsed)| elapsed.clone()),
        )))
        .child(div().w(px(76.)).child(detail(purpose)))
}

pub(super) fn loading_notice(message: &'static str, pulse: f32) -> Div {
    let color = crate::theme::choose(0x8ba7ff, 0x248457);
    let alpha = (76. + 172. * pulse) as u8;
    div().flex().child(
        div()
            .min_w(px(0.))
            .rounded_md()
            .border_2()
            .border_color(rgba(crate::theme::rgba(color, alpha)))
            .when(pulse > 0., |notice| {
                notice.shadow(vec![BoxShadow {
                    color: rgba(crate::theme::rgba(color, (65. * pulse) as u8)).into(),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(7.),
                    spread_radius: px(1.),
                }])
            })
            .px_2()
            .py_1()
            .text_size(px(12.))
            .text_color(rgb(ACCENT()))
            .child(message),
    )
}

pub(super) fn action(label: impl Into<SharedString>) -> Div {
    button(label, false).h(px(25.)).px_2().text_size(px(11.))
}

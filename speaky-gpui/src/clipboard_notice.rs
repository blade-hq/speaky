use crate::theme;
use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};
use std::time::Duration;

struct Notice;
impl gpui::Render for Notice {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        #[cfg(target_os = "macos")]
        crate::overlay::apply_native_theme(window);
        #[cfg(not(target_os = "macos"))]
        let _ = window;
        div()
            .w(px(430.))
            .h(px(70.))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(16.))
            .bg(rgb(theme::surface()))
            .text_color(rgb(theme::ink()))
            .text_size(px(14.))
            .child(crate::icons::Icon::Copy.render(theme::accent()))
            .child(crate::CLIPBOARD_FALLBACK_MESSAGE)
    }
}

pub(crate) fn show(cx: &mut App) -> Result<(), String> {
    let bounds = Bounds::centered(None, size(px(430.), px(70.)), cx);
    let mut shown = Ok(());
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: false,
                show: false,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Speaky · 剪贴板");
                shown = crate::overlay::show_panel(window, &bounds, false);
                cx.new(|_| Notice)
            },
        )
        .map_err(|e| e.to_string())?;
    if let Err(error) = shown {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        return Err(error);
    }
    let executor = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        executor.timer(Duration::from_millis(120)).await;
        let _ = handle.update(cx, |_, window, _| window.resize(size(px(430.), px(70.))));
        executor.timer(Duration::from_secs(8)).await;
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
    Ok(())
}

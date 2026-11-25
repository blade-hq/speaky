use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};
use std::time::Duration;

pub(crate) const MESSAGE: &str = "语音识别内容已保存到剪贴板，请手动粘贴";

struct Notice;
impl gpui::Render for Notice {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(430.))
            .h(px(70.))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(16.))
            .bg(rgb(0x21293c))
            .text_color(rgb(0xf0f6f7))
            .text_size(px(14.))
            .child(crate::icons::Icon::Copy.render(0x83e4d0))
            .child(MESSAGE)
    }
}

pub(crate) fn show(cx: &mut App) -> Result<(), String> {
    #[cfg(windows)]
    let display_id =
        crate::windows_floating::display_id(crate::windows_floating::active_monitor(), cx);
    #[cfg(not(windows))]
    let display_id = None;
    let bounds = Bounds::centered(display_id, size(px(430.), px(70.)), cx);
    let mut shown = Ok(());
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                display_id,
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
                #[cfg(windows)]
                {
                    let drag = std::rc::Rc::new(crate::windows_floating::WindowDrag::new(
                        "clipboard-position.json",
                    ));
                    shown = crate::overlay::show_panel(window, &bounds, &drag, None);
                }
                #[cfg(target_os = "macos")]
                {
                    shown = crate::overlay::show_panel(window, &bounds, false);
                }
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

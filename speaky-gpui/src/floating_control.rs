use crate::{
    icons::Icon,
    theme,
    windows_floating::{self, saved_position, Position, WindowDrag},
};
use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, Div, Stateful, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use std::{rc::Rc, time::Instant};
use windows::Win32::{
    Foundation::POINT,
    Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTONULL},
};

const WIDTH: f32 = 288.;
const HEIGHT: f32 = 42.;
const POSITION_FILE: &str = "floating-position.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Idle,
    Retry,
    Recording { delivery_issue: bool },
    Transcribing,
    Busy(&'static str),
}

impl State {
    fn title(self) -> &'static str {
        match self {
            Self::Idle | Self::Retry => "Speaky · 桌面听写",
            Self::Recording { .. } => "Speaky · 录音",
            Self::Transcribing | Self::Busy(_) => "Speaky · 处理中",
        }
    }
}

pub(super) struct FloatingControl {
    state: State,
    started: Instant,
    levels: [f32; 6],
    actions: Actions,
}

#[derive(Clone)]
struct Actions {
    pointer: Rc<WindowDrag>,
    on_toggle: Rc<dyn Fn(&mut App)>,
    on_cancel: Rc<dyn Fn(&mut App)>,
}

impl Actions {
    fn run(&self, cancel: bool, cx: &mut App) {
        if self.pointer.did_drag() {
            return;
        }
        let action = if cancel {
            self.on_cancel.clone()
        } else {
            self.on_toggle.clone()
        };
        cx.defer(move |cx| action(cx));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FloatingStatus {
    Visible,
    Hidden,
    Unavailable,
    Unknown,
}

impl FloatingStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Visible => "已显示",
            Self::Hidden => "已关闭",
            Self::Unavailable => "已开启，但未显示",
            Self::Unknown => "无法读取设置",
        }
    }
}

impl FloatingControl {
    pub(super) fn open(
        cx: &mut App,
        on_toggle: impl Fn(&mut App) + 'static,
        on_cancel: impl Fn(&mut App) + 'static,
    ) -> Result<WindowHandle<Self>, String> {
        let pointer = Rc::new(WindowDrag::new(POSITION_FILE));
        let display_id = windows_floating::display_id(preferred_monitor(), cx);
        let initial_bounds = Bounds::centered(display_id, size(px(WIDTH), px(HEIGHT)), cx);
        let mut shown = Ok(());
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(initial_bounds)),
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
                    window.set_window_title(State::Idle.title());
                    shown = install(window, &pointer);
                    cx.new(|_| Self {
                        state: State::Idle,
                        started: Instant::now(),
                        levels: [0.; 6],
                        actions: Actions {
                            pointer,
                            on_toggle: Rc::new(on_toggle),
                            on_cancel: Rc::new(on_cancel),
                        },
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        if let Err(error) = shown {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return Err(error);
        }
        let executor = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            executor.timer(std::time::Duration::from_millis(120)).await;
            let _ = handle.update(cx, |_, window, _| {
                window.resize(size(px(WIDTH), px(HEIGHT)))
            });
        })
        .detach();
        Ok(handle)
    }

    pub(super) fn sync(
        &mut self,
        state: State,
        level: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let recording = matches!(state, State::Recording { .. });
        if state != self.state {
            if recording && !matches!(self.state, State::Recording { .. }) {
                self.started = Instant::now();
                self.levels.fill(0.);
            }
            self.state = state;
            window.set_window_title(state.title());
        } else if !recording {
            return;
        }
        if recording {
            self.levels.rotate_left(1);
            self.levels[5] = level.clamp(0., 1.);
        }
        cx.notify();
    }
}

impl Render for FloatingControl {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        control_bar(
            "floating-control",
            self.state,
            self.levels,
            self.started.elapsed().as_secs(),
            Some(self.actions.clone()),
        )
    }
}

pub(super) fn tour_preview() -> Div {
    div().flex().flex_col().gap_2().children(
        [
            ("平时", "tour-floating-idle", State::Idle, [0.; 6], 0),
            (
                "录音中",
                "tour-floating-recording",
                State::Recording {
                    delivery_issue: false,
                },
                [0.1, 0.16, 0.04, 0.14, 0.2, 0.06],
                3,
            ),
        ]
        .map(|(label, id, state, levels, seconds)| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(40.))
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(rgb(theme::muted()))
                        .child(label),
                )
                .child(control_bar(id, state, levels, seconds, None))
        }),
    )
}

fn control_bar(
    id: &'static str,
    state: State,
    levels: [f32; 6],
    seconds: u64,
    actions: Option<Actions>,
) -> Stateful<Div> {
    let recording = matches!(state, State::Recording { .. });
    let delivery_issue = matches!(
        state,
        State::Recording {
            delivery_issue: true
        }
    );
    let label = match state {
        State::Idle => "开始听写",
        State::Recording { .. } if delivery_issue => "未写入输入框",
        State::Recording { .. } => "停止并保存",
        State::Retry => "重试录音",
        State::Transcribing => "正在转写…",
        State::Busy(reason) => reason,
    };
    let enabled = !matches!(state, State::Transcribing | State::Busy(_));
    let cancel = actions.clone();
    let finish = actions.clone();
    div()
        .id(id)
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .gap_1p5()
        .px_2()
        .rounded_full()
        .bg(rgb(theme::surface()))
        .border_1()
        .border_color(rgb(theme::border()))
        .text_size(px(12.))
        .text_color(rgb(theme::ink()))
        .when(recording, |bar| {
            bar.child(
                div()
                    .id("floating-cancel")
                    .size(px(27.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .child(Icon::Close.render(theme::muted()))
                    .when_some(cancel, |button, actions| {
                        button
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::sidebar())))
                            .on_click(move |_, _, cx| actions.run(true, cx))
                    }),
            )
        })
        .child(
            div()
                .flex_none()
                .size(px(25.))
                .rounded_full()
                .bg(rgb(if recording {
                    theme::choose(0x683c4b, 0xf8e3df)
                } else {
                    theme::selection()
                }))
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::Microphone.render(if recording {
                    theme::error()
                } else {
                    theme::accent()
                })),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .when(delivery_issue, |text| text.text_color(rgb(theme::error())))
                .child(label),
        )
        .when(recording, |bar| {
            bar.child(div().flex_none().text_size(px(11.)).child(format!(
                "{:02}:{:02}",
                seconds / 60,
                seconds % 60
            )))
        })
        .when(recording && !delivery_issue, |bar| {
            bar.child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(1.))
                    .items_center()
                    .children(levels.into_iter().map(|level| {
                        div()
                            .w(px(2.))
                            .h(px(3. + 13. * (level * 5.).sqrt().min(1.)))
                            .rounded_full()
                            .bg(rgb(theme::accent()))
                    })),
            )
        })
        .when(!recording, |bar| {
            bar.child(
                div()
                    .ml_1()
                    .flex_none()
                    .flex()
                    .gap(px(2.))
                    .children((0..2).map(|_| {
                        div().flex().flex_col().gap(px(2.)).children(
                            (0..3)
                                .map(|_| div().size(px(2.)).rounded_full().bg(rgb(theme::muted()))),
                        )
                    })),
            )
        })
        .when(recording, |bar| {
            bar.child(
                div()
                    .id("floating-finish")
                    .size(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(theme::accent()))
                    .child(Icon::Check.render(theme::accent_ink()))
                    .when_some(finish, |button, actions| {
                        button
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::choose(0x7c9aff, 0x1f473a))))
                            .on_click(move |_, _, cx| actions.run(false, cx))
                    }),
            )
        })
        .when(enabled && !recording, |bar| {
            bar.when_some(actions, |bar, actions| {
                bar.cursor_pointer()
                    .hover(|b| b.bg(rgb(theme::sidebar())))
                    .on_click(move |_, _, cx| actions.run(false, cx))
            })
        })
}

fn install(window: &Window, pointer: &Rc<WindowDrag>) -> Result<(), String> {
    let monitor = preferred_monitor();
    let dpi = windows_floating::monitor_scale(monitor, window.scale_factor());
    let work = windows_floating::monitor_work_area(monitor).ok_or("无法取得桌面工作区。")?;
    let width = ((WIDTH * dpi).round() as i32).min(work.right - work.left);
    let height = ((HEIGHT * dpi).round() as i32).min(work.bottom - work.top);
    let margin = (24. * dpi).round() as i32;
    let position = saved_position(POSITION_FILE)
        .filter(|position| monitor_at(*position) == monitor)
        .unwrap_or(Position {
            x: work.right - width - margin,
            y: work.bottom - height - margin,
        })
        .clamp_to(work, width, height);
    windows_floating::show(window, pointer, position, width, height)
}

fn monitor_at(position: Position) -> HMONITOR {
    unsafe {
        MonitorFromPoint(
            POINT {
                x: position.x,
                y: position.y,
            },
            MONITOR_DEFAULTTONULL,
        )
    }
}

fn preferred_monitor() -> HMONITOR {
    if std::env::var_os("SPEAKY_START_ON_SECONDARY").is_none_or(|value| value != "1") {
        if let Some(position) = saved_position(POSITION_FILE) {
            let monitor = monitor_at(position);
            if !monitor.0.is_null() {
                return monitor;
            }
        }
    }
    windows_floating::active_monitor()
}

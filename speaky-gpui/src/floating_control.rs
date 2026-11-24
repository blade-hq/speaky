use crate::{
    icons::Icon,
    windows_floating::{self, saved_position, work_area, Position, WindowDrag},
};
use gpui::{
    div, point, prelude::*, px, rgb, size, App, Bounds, Context, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use std::{rc::Rc, time::Instant};
use windows::Win32::{
    Foundation::POINT,
    Graphics::Gdi::{MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY},
};

const WIDTH: f32 = 168.;
const HEIGHT: f32 = 42.;
const POSITION_FILE: &str = "floating-position.json";
const ACCENT: u32 = 0x83e4d0;
const INK: u32 = 0xf0f6f7;

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
    pointer: Rc<WindowDrag>,
    on_toggle: Rc<dyn Fn(&mut App)>,
}

impl FloatingControl {
    pub(super) fn open(
        cx: &mut App,
        on_toggle: impl Fn(&mut App) + 'static,
    ) -> Result<WindowHandle<Self>, String> {
        let pointer = Rc::new(WindowDrag::new(POSITION_FILE));
        let mut shown = Ok(());
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(WIDTH), px(HEIGHT)),
                    ))),
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
                        pointer,
                        on_toggle: Rc::new(on_toggle),
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        if let Err(error) = shown {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return Err(error);
        }
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
        let recording = matches!(self.state, State::Recording { .. });
        let delivery_issue = matches!(
            self.state,
            State::Recording {
                delivery_issue: true
            }
        );
        let label = match self.state {
            State::Idle => "开始听写",
            State::Recording { .. } if delivery_issue => "未写入输入框",
            State::Recording { .. } => "停止并保存",
            State::Retry => "重试录音",
            State::Transcribing => "正在转写…",
            State::Busy(reason) => reason,
        };
        let enabled = !matches!(self.state, State::Transcribing | State::Busy(_));
        let pointer = self.pointer.clone();
        let toggle = self.on_toggle.clone();
        let seconds = self.started.elapsed().as_secs();
        div()
            .id("floating-control")
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .gap_1p5()
            .px_2()
            .rounded_full()
            .bg(rgb(0x21293c))
            .border_1()
            .border_color(rgb(0x52607e))
            .text_size(px(12.))
            .text_color(rgb(INK))
            .child(
                div()
                    .flex_none()
                    .size(px(25.))
                    .rounded_full()
                    .bg(rgb(if recording { 0x683c4b } else { 0x384461 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        if recording {
                            Icon::Stop
                        } else {
                            Icon::Microphone
                        }
                        .render(if recording { 0xffb6bd } else { ACCENT }),
                    ),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .when(delivery_issue, |text| text.text_color(rgb(0xffb1a2)))
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
                        .children(self.levels.iter().map(|level| {
                            div()
                                .w(px(2.))
                                .h(px(3. + 13. * (level * 5.).sqrt().min(1.)))
                                .rounded_full()
                                .bg(rgb(ACCENT))
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
                                (0..3).map(|_| div().size(px(2.)).rounded_full().bg(rgb(0x8792ad))),
                            )
                        })),
                )
            })
            .when(enabled, |bar| {
                bar.cursor_pointer()
                    .hover(|b| b.bg(rgb(0x303b54)))
                    .on_click(move |_, _, cx| {
                        if !pointer.did_drag() {
                            let toggle = toggle.clone();
                            cx.defer(move |cx| toggle(cx));
                        }
                    })
            })
    }
}

fn install(window: &Window, pointer: &Rc<WindowDrag>) -> Result<(), String> {
    let dpi = window.scale_factor();
    let (width, height) = ((WIDTH * dpi).round() as i32, (HEIGHT * dpi).round() as i32);
    let saved = saved_position(POSITION_FILE).and_then(|position| {
        work_area(
            POINT {
                x: position.x,
                y: position.y,
            },
            MONITOR_DEFAULTTONULL,
        )
        .map(|work| position.clamp_to(work, width, height))
    });
    let position = match saved {
        Some(position) => position,
        None => {
            let work = work_area(POINT::default(), MONITOR_DEFAULTTOPRIMARY)
                .ok_or("无法取得桌面工作区。")?;
            let margin = (24. * dpi).round() as i32;
            Position {
                x: work.right - width - margin,
                y: work.bottom - height - margin,
            }
            .clamp_to(work, width, height)
        }
    };
    windows_floating::show(window, pointer, position, width, height)
}

use gpui::{
    canvas, div, fill, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, size,
    AnyElement, Bounds, Context, Div, FontWeight, Pixels, Stateful,
};

use crate::{studio_ui, Page, Speaky, ACCENT, INK, MUTED, SIDEBAR};

use super::{effects, motion::Frame, GuideAction as Action, GuideButton, PracticeIssue, Step};

const LIGHT: u32 = 0xf1fff6;

impl Speaky {
    pub(super) fn story_card(
        &self,
        step: Step,
        width: Pixels,
        max_height: Option<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frame = self.tour.frame;
        let epoch = self.tour.transition;
        let hero = step == Step::Welcome;
        let compact = max_height.is_some();
        let surface = if hero {
            linear_gradient(
                145.,
                linear_color_stop(rgb(0x284d3c), 0.),
                linear_color_stop(rgb(0x0f2824), 1.),
            )
        } else {
            linear_gradient(
                143.,
                linear_color_stop(rgb(crate::SURFACE()), 0.),
                linear_color_stop(rgb(crate::BACKGROUND()), 1.),
            )
        };
        let panel = div()
            .id("tour-card")
            .track_focus(&self.tour.focus)
            .occlude()
            .w(width)
            .p(if hero { px(22.) } else { px(12.) })
            .flex()
            .flex_col()
            .gap(if hero { px(12.) } else { px(7.) })
            .rounded(px(if hero { 22. } else { 13. }))
            .bg(surface)
            .border_1()
            .border_color(rgb(if hero { 0x7eab94 } else { crate::BORDER() }))
            .when(!compact, |panel| panel.shadow_lg())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(rgb(if hero { 0xccefdc } else { MUTED() }))
                    .child(step.label())
                    .child(div().flex_1())
                    .child(guide_button(
                        "tour-skip",
                        (if hero { "跳过引导" } else { "退出引导" }, Action::StopTour),
                        false,
                        hero,
                        epoch,
                        cx,
                    )),
            )
            .child(progress(frame, hero));

        if hero {
            return panel
                .child(self.welcome_card(frame))
                .child(div().flex().justify_center().child(guide_button(
                    "tour-start",
                    ("开始体验", Action::Next),
                    true,
                    true,
                    epoch,
                    cx,
                )))
                .into_any_element();
        }

        if self.page != step.page() {
            let (title, text) = match (step, self.page) {
                (Step::Record, Page::Models) => {
                    ("准备识别模型", "选择本地模型或检查识别服务，然后回到练习。")
                }
                (Step::Record, Page::Settings) => ("检查输入设置", "选好麦克风后，回到练习。"),
                _ => ("继续设置", "返回引导正在讲解的控件。"),
            };
            return panel
                .child(heading(title, text))
                .child(action_row(
                    [("tour-return", ("返回此步骤", Action::Return), true)],
                    epoch,
                    cx,
                ))
                .into_any_element();
        }

        let body = match step {
            Step::Welcome => unreachable!(),
            Step::Shortcut => self.shortcut_guide(cx),
            Step::Models => self.models_guide(cx),
            Step::Floating => self.floating_guide(cx),
            Step::Record => self.practice_guide(cx),
            Step::Finish => self.finish_guide(cx),
        };
        let practice_busy = step == Step::Record
            && (self.recording.is_some()
                || self.starting
                || self.tour.preparing
                || self.transcribing
                || self.unsaved.is_some());
        let next = if step == Step::Finish {
            ("进入首页", Action::Finish)
        } else {
            ("下一步", Action::Next)
        };
        panel
            .child(
                div()
                    .id(("tour-card-body", step as usize))
                    .min_h(px(0.))
                    .when_some(max_height, |body, height| {
                        body.max_h((height - px(102.)).max(px(50.)))
                            .overflow_y_scroll()
                    })
                    .child(body),
            )
            .when(!practice_busy, |panel| {
                panel.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .child(guide_button(
                            "tour-back",
                            ("上一步", Action::Back),
                            false,
                            false,
                            epoch,
                            cx,
                        ))
                        .child(div().flex_1())
                        .child(guide_button("tour-next", next, true, false, epoch, cx)),
                )
            })
            .into_any_element()
    }

    fn welcome_card(&self, frame: Frame) -> Div {
        let mark = frame.part((25., 240.));
        let title = frame.part((85., 365.));
        let caption = frame.part((155., 420.));
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .pt_2()
            .child(
                div()
                    .relative()
                    .top(px(7. * (1. - mark)))
                    .opacity(mark)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(studio_ui::mark())
                    .child(
                        div()
                            .text_color(rgb(0xddf5e6))
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .child("Speaky"),
                    ),
            )
            .child(
                div()
                    .relative()
                    .top(px(6. * (1. - title)))
                    .opacity(title)
                    .text_color(rgb(LIGHT))
                    .text_size(px(32.))
                    .line_height(px(42.))
                    .font_weight(FontWeight::BOLD)
                    .child("让声音，落成文字"),
            )
            .child(
                div()
                    .relative()
                    .top(px(5. * (1. - caption)))
                    .opacity(caption)
                    .text_color(rgb(0xc9e1d3))
                    .text_size(px(13.))
                    .child("设好听写方式，再说一句试试。"),
            )
            .child(
                div()
                    .w_full()
                    .mt_2()
                    .px_4()
                    .child(effects::signal(frame, 48.)),
            )
    }

    fn shortcut_guide(&self, cx: &mut Context<Self>) -> Div {
        let registered = self.v30_registered_shortcut();
        let binding = registered
            .map(str::to_owned)
            .or_else(|| {
                self.guide_shortcut()
                    .map(|shortcut| format!("{}（未注册）", shortcut.label()))
            })
            .unwrap_or_else(|| "尚未注册".into());
        let preparing = self.guide_shortcut_capturing();
        let verified = self.tour.shortcut_verified;
        let detail = if preparing {
            "按下新的组合键，松开后保存。".into()
        } else if verified {
            "已收到你的快捷键。再按一次也可以验证。".into()
        } else if let Some(error) = self.guide_shortcut_error() {
            error.to_owned()
        } else if self.tour.shortcut_waiting {
            format!("现在按一次 {binding}，检查是否触发。")
        } else {
            "以后在输入框里按它开始听写，再按一次结束。".into()
        };
        let mut body = heading("设置听写快捷键", detail);
        body = body.child(status_line("当前快捷键", binding, verified));
        if !preparing && !verified {
            let mut choices = vec![(
                "tour-key-edit",
                ("重新录入", Action::CaptureShortcut),
                registered.is_none(),
            )];
            if registered.is_some() && !self.tour.shortcut_waiting {
                choices.insert(
                    0,
                    ("tour-key-keep", ("保留当前", Action::KeepShortcut), true),
                );
            }
            body = body.child(action_row(choices, self.tour.transition, cx));
        }
        #[cfg(target_os = "macos")]
        if !self.mac_dictation_access {
            body = body.child(action_row(
                [(
                    "tour-access",
                    ("允许写入其他应用", Action::Accessibility),
                    false,
                )],
                self.tour.transition,
                cx,
            ));
        }
        body
    }

    fn models_guide(&self, cx: &mut Context<Self>) -> Div {
        let (recognition, rewrite) = self.guide_model_status();
        let rewrites = self.redesign_config.rewrite;
        heading(
            "选择识别与改写",
            if rewrites {
                "AI 改写可以整理措辞和结构，按需开启。"
            } else {
                "语音转文字把声音变成文字。AI 改写可以再整理措辞。"
            },
        )
        .child(status_line(
            "语音转文字",
            recognition,
            self.guide_model_ready(),
        ))
        .child(status_line("AI 改写", rewrite, false))
        .child(action_row(
            [if rewrites {
                (
                    "tour-model-asr",
                    ("查看语音模型", Action::ModelTab(false)),
                    false,
                )
            } else {
                (
                    "tour-model-rewrite",
                    ("查看 AI 改写", Action::ModelTab(true)),
                    false,
                )
            }],
            self.tour.transition,
            cx,
        ))
    }

    fn floating_guide(&self, _cx: &mut Context<Self>) -> Div {
        #[cfg(windows)]
        {
            use crate::floating_control::{tour_preview, FloatingStatus};
            let status = self.tour_floating_status();
            let mut body = heading(
                "桌面悬浮按钮",
                "可在设置里随时更改。关闭后仍能用快捷键听写。",
            );
            body = body.child(tour_preview()).child(status_line(
                "当前状态",
                status.label(),
                status == FloatingStatus::Visible,
            ));
            if !self.starting && self.recording.is_none() && !self.transcribing {
                body = body.child(action_row(
                    [
                        (
                            "tour-floating-show",
                            ("显示悬浮按钮", Action::Floating(true)),
                            status != FloatingStatus::Visible,
                        ),
                        (
                            "tour-floating-hide",
                            (
                                if status == FloatingStatus::Visible {
                                    "关闭悬浮按钮"
                                } else {
                                    "保持关闭"
                                },
                                Action::Floating(false),
                            ),
                            false,
                        ),
                    ],
                    self.tour.transition,
                    _cx,
                ));
            }
            body
        }
        #[cfg(not(windows))]
        {
            heading(
                "使用快捷键听写",
                "此系统没有桌面悬浮按钮，可以使用快捷键或首页录音。",
            )
        }
    }

    fn practice_guide(&self, cx: &mut Context<Self>) -> Div {
        if self.practice_result_ready() {
            return heading(
                "你刚说的话，变成了文字",
                if self.tour.copied {
                    "已复制当前版本。原声和文字可在历史记录里找到。"
                } else {
                    "在结果区复制文字，也可以稍后在历史记录里找回。"
                },
            );
        }
        let record = ("tour-record-retry", ("重新录音", Action::Record), true);
        let models = ("tour-models", ("识别模型", Action::Models), false);
        let devices = ("tour-devices", ("输入设备", Action::Devices), false);
        let privacy = ("tour-privacy", ("麦克风权限", Action::Privacy), false);
        let epoch = self.tour.transition;
        if self.unsaved.is_some() {
            return heading("音频还在内存中", "点上方的「重试保存」后继续。");
        }
        if self.transcribing {
            return heading("正在转写整段录音", "原声已保存。识别完成后会显示转写结果。");
        }
        if self.recording.is_some() {
            return heading("说句话试试", "说完后，按同一个快捷键或点击「停止并保存」。");
        }
        if self.starting || self.tour.preparing {
            return heading("正在准备录音", "麦克风和识别模型就绪后就能开始说话。");
        }
        if self.recognition_model_busy() || self.diagnostics.checking {
            return heading("正在检查设备或模型", "准备好后便可开始录音。");
        }
        match self.tour.issue {
            Some(PracticeIssue::Microphone) => {
                heading("没听清你说的话", "检查输入设备或系统麦克风权限，再试一次。")
                    .child(action_row([record, devices, privacy], epoch, cx))
            }
            Some(PracticeIssue::Model) => heading(
                "识别模型还没就绪",
                "选择可用的本地模型或检查识别服务后，再开始录音。",
            )
            .child(action_row([record, models], epoch, cx)),
            Some(PracticeIssue::Transcription) => heading(
                "这段录音还没有转写结果",
                "原声已保存。检查识别模型后，可以转写同一段录音。",
            )
            .child(action_row(
                [
                    (
                        "tour-transcribe-retry",
                        ("重试转写", Action::RetryTranscription),
                        true,
                    ),
                    models,
                    ("tour-record-again", ("再说一句", Action::Record), false),
                ],
                epoch,
                cx,
            )),
            None => {
                let mut text = match self.v30_registered_shortcut() {
                    Some(shortcut) => format!("按 {shortcut} 或点击「开始录音」，然后说一句话。"),
                    None => "点击「开始录音」，然后说一句话。".into(),
                };
                #[cfg(windows)]
                if self.tour_floating_status() == crate::floating_control::FloatingStatus::Visible {
                    text.push_str("也可以点桌面悬浮按钮。");
                }
                heading("录下你的第一句话", std::mem::take(&mut text))
            }
        }
    }

    fn finish_guide(&self, cx: &mut Context<Self>) -> Div {
        let (recognition, rewrite) = self.guide_model_status();
        let shortcut = self.v30_registered_shortcut().unwrap_or("未注册");
        #[cfg(windows)]
        let floating = self.tour_floating_status().label();
        #[cfg(not(windows))]
        let floating = "此系统不支持";
        let feature = |id, title, detail: &'static str, page, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_1()
                .child(guide_button(
                    id,
                    (title, Action::OpenFeature(page)),
                    false,
                    false,
                    self.tour.transition,
                    cx,
                ))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(MUTED()))
                        .child(detail),
                )
        };
        heading("设置完成", "随时开始下一段听写。")
            .child(status_line("听写快捷键", shortcut.to_owned(), false))
            .child(status_line("语音转文字", recognition, false))
            .child(status_line("AI 改写", rewrite, false))
            .child(status_line("悬浮按钮", floating, false))
            .child(
                div().flex().flex_wrap().gap_3().children(
                    [
                        ("tour-open-history", "历史", "找回录音和转写", Page::History),
                        (
                            "tour-open-terms",
                            "词典",
                            "管理常用词和替换规则",
                            Page::Terms,
                        ),
                        ("tour-open-files", "转录文件", "导入已有音频", Page::Files),
                    ]
                    .map(|(id, title, description, page)| {
                        feature(id, title, description, page, cx)
                    }),
                ),
            )
    }
}

type CardAction = (&'static str, GuideButton, bool);

fn heading(title: &'static str, detail: impl Into<gpui::SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(MUTED()))
                .child(detail.into()),
        )
}

fn status_line(label: &'static str, detail: impl Into<gpui::SharedString>, ready: bool) -> Div {
    div()
        .flex()
        .items_start()
        .gap_2()
        .text_size(px(12.))
        .child(div().flex_none().text_color(rgb(MUTED())).child(label))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(if ready {
                    crate::theme::success()
                } else {
                    INK()
                }))
                .child(detail.into()),
        )
}

fn action_row(
    actions: impl IntoIterator<Item = CardAction>,
    epoch: u64,
    cx: &mut Context<Speaky>,
) -> Div {
    div().flex().items_center().flex_wrap().gap_2().children(
        actions
            .into_iter()
            .map(|(id, button, primary)| guide_button(id, button, primary, false, epoch, cx)),
    )
}

fn progress(frame: Frame, hero: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let gap = px(5.);
            let width = (bounds.size.width - gap * (Step::ALL.len() - 1)) / Step::ALL.len() as f32;
            for i in 0..Step::ALL.len() {
                let origin = point(bounds.left() + (width + gap) * i, bounds.top());
                let full = Bounds::new(origin, size(width, bounds.size.height));
                window.paint_quad(
                    fill(full, rgb(if hero { 0x436454 } else { crate::BORDER() }))
                        .corner_radii(px(3.)),
                );
                let filled = (frame.progress + 1. - i as f32).clamp(0., 1.);
                if filled > 0. {
                    let color = if hero { 0xacedd1 } else { ACCENT() };
                    let part = Bounds::new(origin, size(width * filled, bounds.size.height));
                    window.paint_quad(fill(part, rgb(color)).corner_radii(px(3.)));
                }
            }
        },
    )
    .w_full()
    .h(px(3.))
}

fn guide_button(
    id: &'static str,
    (label, action): GuideButton,
    primary: bool,
    hero: bool,
    epoch: u64,
    cx: &mut Context<Speaky>,
) -> Stateful<Div> {
    let (bg, ink, hover) = match (primary, hero) {
        (true, true) => (0xafebcd, 0x142d25, 0xd0f8e2),
        (true, false) => (
            ACCENT(),
            crate::ACCENT_INK(),
            crate::theme::choose(0x7c9aff, 0x1f473a),
        ),
        (false, true) => (0x27493d, 0xe1f4e9, 0x3c6351),
        (false, false) => (SIDEBAR(), INK(), crate::theme::choose(0x404956, 0xdcebe1)),
    };
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(if primary { 32. } else { 25. }))
        .px(if primary { px(13.) } else { px(8.) })
        .rounded(px(6.))
        .bg(rgb(bg))
        .text_color(rgb(ink))
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(hover)))
        .focusable()
        .tab_stop(true)
        .focus(move |style| {
            style
                .border_1()
                .border_color(rgb(if hero { 0xd7f7e7 } else { ACCENT() }))
        })
        .on_click(
            cx.listener(move |view, _, window, cx| {
                view.run_guide_action(epoch, action, window, cx)
            }),
        )
}

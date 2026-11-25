use gpui::{
    canvas, div, fill, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, size,
    AnyElement, Bounds, Context, Div, FontWeight, Pixels, Stateful,
};

use crate::{icons::Icon, studio_ui, Page, Speaky, ACCENT, ERROR, INK, MUTED, SIDEBAR};

use super::{
    effects,
    motion::{Frame, Scene},
    GuideAction as Action, GuideButton, Microphone, Step, STEPS,
};

const FOREST: u32 = 0x173d32;
const PALE: u32 = 0xdceee3;
const LIGHT: u32 = 0xf1fff6;

struct Copy {
    title: &'static str,
    text: &'static str,
    icon: Icon,
    tools: [Option<GuideButton>; 2],
    notice: Option<Notice>,
}

struct Notice {
    text: &'static str,
    issue: bool,
    link: Option<GuideButton>,
}

impl Copy {
    fn new(icon: Icon, title: &'static str, text: &'static str) -> Self {
        Self {
            icon,
            title,
            text,
            tools: [None; 2],
            notice: None,
        }
    }

    fn tool(mut self, label: &'static str, action: Action) -> Self {
        let slot = if self.tools[0].is_none() { 0 } else { 1 };
        self.tools[slot] = Some((label, action));
        self
    }

    fn notice(mut self, text: &'static str, issue: bool, link: Option<GuideButton>) -> Self {
        self.notice = Some(Notice { text, issue, link });
        self
    }
}

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
                linear_color_stop(rgb(0xffffff), 0.),
                linear_color_stop(rgb(0xf0f6f1), 1.),
            )
        };
        let title_color = if hero { LIGHT } else { INK };
        let opacity = if frame.scene == Scene::Exit {
            1. - frame.card()
        } else {
            0.77 + 0.23 * frame.card()
        };
        let panel = div()
            .id("tour-card")
            .track_focus(&self.tour.focus)
            .occlude()
            .w(width)
            .when_some(max_height, |panel, height| {
                panel.max_h(height).overflow_y_scroll()
            })
            .p(if hero {
                px(22.)
            } else if compact {
                px(11.)
            } else {
                px(16.)
            })
            .flex()
            .flex_col()
            .gap(if hero { px(13.) } else { px(8.) })
            .rounded(px(if hero { 22. } else { 16. }))
            .bg(surface)
            .border_1()
            .border_color(rgb(if hero { 0x7eab94 } else { 0xbad1c1 }))
            .when(!compact, |panel| panel.shadow_lg())
            .opacity(opacity)
            .when(frame.scene == Scene::Exit, |panel| {
                panel.relative().top(px(-7. * frame.focus()))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(if hero { 0xccefdc } else { ACCENT }))
                            .child("使用引导"),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_px()
                            .rounded(px(4.))
                            .bg(rgb(if hero { 0x345b4b } else { 0xe7f0e9 }))
                            .text_color(rgb(if hero { 0xceedd9 } else { 0x416754 }))
                            .text_size(px(11.))
                            .child(format!("{:02} / {:02}", step as usize + 1, STEPS.len())),
                    )
                    .child(div().flex_1())
                    .child(guide_button(
                        "tour-skip",
                        ("跳过", Action::StopTour),
                        false,
                        hero,
                        epoch,
                        cx,
                    ))
                    .child(guide_button(
                        "tour-close",
                        ("×", Action::StopTour),
                        false,
                        hero,
                        epoch,
                        cx,
                    )),
            )
            .child(progress(frame, hero));

        let panel = if hero {
            panel.child(self.welcome_card(frame))
        } else {
            let words = self.story_copy(step, frame.scene);
            let intro = frame.content();
            let sample = (step == Step::Result && self.page == Page::Home && !compact)
                .then(|| self.transcript_strip(frame));
            panel
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .size(px(36.))
                                .flex_none()
                                .rounded(px(9.))
                                .bg(rgb(PALE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.25 + 0.75 * frame.part((110., 390.)))
                                .child(words.icon.render(ACCENT)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .relative()
                                        .top(px(5. * (1. - intro)))
                                        .opacity(0.23 + 0.77 * intro)
                                        .text_color(rgb(title_color))
                                        .text_size(px(21.))
                                        .line_height(px(26.))
                                        .font_weight(FontWeight::BOLD)
                                        .child(words.title),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .top(px(6. * (1. - intro)))
                                        .opacity(0.15 + 0.85 * intro)
                                        .text_size(px(13.))
                                        .line_height(px(18.))
                                        .text_color(rgb(MUTED))
                                        .child(words.text),
                                ),
                        ),
                )
                .when(step == Step::Microphone && !compact, |panel| {
                    panel.child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded(px(7.))
                            .bg(rgb(0xe9f2eb))
                            .child(effects::signal(frame, 23., false)),
                    )
                })
                .children(sample)
                .when(step == Step::Language, |panel| {
                    let selected = crate::config::Config::load()
                        .unwrap_or_default()
                        .core
                        .asr
                        .language;
                    panel.child(
                        div().flex().flex_wrap().gap_1().children(
                            speaky_core::session::LANGUAGES
                                .into_iter()
                                .map(|(code, label)| {
                                    guide_button(
                                        label,
                                        (label, Action::Language(code)),
                                        selected == code,
                                        false,
                                        epoch,
                                        cx,
                                    )
                                }),
                        ),
                    )
                })
                .when_some(words.notice, |panel, notice| {
                    panel.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded(px(7.))
                            .px_2()
                            .py_1()
                            .bg(rgb(if notice.issue { 0xfaeee9 } else { SIDEBAR }))
                            .text_color(rgb(if notice.issue { ERROR } else { ACCENT }))
                            .text_size(px(12.))
                            .child(div().flex_1().min_w(px(0.)).child(notice.text))
                            .when_some(notice.link, |row, action| {
                                row.child(guide_button(
                                    "tour-recovery",
                                    action,
                                    false,
                                    false,
                                    epoch,
                                    cx,
                                ))
                            }),
                    )
                })
                .when(words.tools.iter().any(Option::is_some), |panel| {
                    panel.child(
                        div().flex().items_center().gap_1p5().flex_wrap().children(
                            words
                                .tools
                                .into_iter()
                                .enumerate()
                                .filter_map(|(i, choice)| {
                                    choice.map(|action| {
                                        guide_button(
                                            if i == 0 { "tour-tool" } else { "tour-tool-2" },
                                            action,
                                            false,
                                            false,
                                            epoch,
                                            cx,
                                        )
                                        .bg(rgb(PALE))
                                    })
                                }),
                        ),
                    )
                })
        };

        let next = match step {
            Step::Welcome => "开始引导  ›",
            Step::Finish => "开始使用  ›",
            Step::Record if self.recording.is_some() => "保存并继续  ›",
            _ => "下一步  ›",
        };
        panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(!hero, |row| {
                        row.child(guide_button(
                            "tour-back",
                            ("‹  上一步", Action::Back),
                            false,
                            false,
                            epoch,
                            cx,
                        ))
                    })
                    .when(hero && self.recording.is_some(), |row| {
                        row.child(guide_button(
                            "tour-stop-active",
                            ("停止并保存", Action::Record),
                            false,
                            hero,
                            epoch,
                            cx,
                        ))
                    })
                    .child(div().flex_1())
                    .child(guide_button(
                        "tour-next",
                        (next, Action::Next),
                        true,
                        hero,
                        epoch,
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn welcome_card(&self, frame: Frame) -> Div {
        let mark = frame.part((45., 355.));
        let title = frame.part((205., 560.));
        let caption = frame.part((385., 680.));
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .pt_2()
            .child(
                div()
                    .relative()
                    .top(px(8. * (1. - mark)))
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
                    .top(px(7. * (1. - title)))
                    .opacity(title)
                    .text_color(rgb(LIGHT))
                    .text_size(px(34.))
                    .line_height(px(45.))
                    .font_weight(FontWeight::BOLD)
                    .child("让声音，落成文字"),
            )
            .child(
                div()
                    .relative()
                    .top(px(6. * (1. - caption)))
                    .opacity(caption)
                    .text_color(rgb(0xc9e1d3))
                    .text_size(px(13.))
                    .line_height(px(20.))
                    .child("欢迎使用 Speaky，试着录下你的第一句话。"),
            )
            .child(
                div()
                    .w_full()
                    .mt_2()
                    .px_4()
                    .child(effects::signal(frame, 45., true)),
            )
    }

    fn transcript_strip(&self, frame: Frame) -> Div {
        let live = !self.transcribing && self.recording.is_none() && self.saved_recording.is_none();
        let actual = self
            .result
            .as_ref()
            .filter(|_| live)
            .map(|(_, text)| text.as_str());
        let (label, text) = match actual {
            Some(text) => ("转写", text),
            None if self.transcribing => ("状态", "正在转写，录音已保存。"),
            _ => ("示例", "刚才说的话，现在可以复制了。"),
        };
        let text: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(25)
            .collect();
        let reveal = if frame.scene == Scene::Transcript {
            frame.part((440., 1040.))
        } else {
            1.
        };
        let visible: String = text
            .chars()
            .take((text.chars().count() as f32 * reveal).ceil() as usize)
            .collect();
        div()
            .h(px(29.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .rounded(px(6.))
            .bg(rgb(0xe5efe7))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(ACCENT))
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(rgb(INK))
                    .child(visible),
            )
    }

    fn story_copy(&self, step: Step, scene: Scene) -> Copy {
        let expected = self.page == step.page()
            || (step == Step::Model && self.page == Page::Settings)
            || (step == Step::Result && self.page == Page::History);
        let mut words = if !expected {
            Copy::new(Icon::Guide, "继续引导", "返回刚才的控件，或继续下一步。")
                .tool("返回本步", Action::Locate)
        } else {
            match step {
                Step::Welcome => unreachable!("the welcome uses its own layout"),
                Step::Permissions => {
                    let words = Copy::new(Icon::Microphone, "开通所需权限", "允许麦克风访问。macOS 还需允许辅助功能，才能向其他应用输入文字。")
                        .tool("允许并检测麦克风", Action::CheckMicrophone);
                    #[cfg(target_os = "macos")]
                    let words = words.tool("允许辅助功能", Action::Accessibility);
                    words
                }
                Step::Shortcut => Copy::new(Icon::Settings, "熟悉听写快捷键", "按一次开始，再按一次停止并保存。录音中可以切换应用；Esc 取消转写并保留音频。")
                    .tool("设置快捷键", Action::Shortcut),
                Step::Language => Copy::new(Icon::Model, "选择常用识别语言", "推荐选择你主要使用的语言；经常切换语言时选择自动检测。"),
                Step::Microphone => self.microphone_copy(),
                Step::Model => self.model_copy(),
                Step::Record if self.recording.is_some() => Copy::new(
                    Icon::Microphone,
                    "说一句，看看文字",
                    "转写会实时更新，说完可以停止并保存。",
                )
                .tool("停止并保存", Action::Record),
                Step::Record
                    if self.transcribing
                        || self.unsaved.is_some()
                        || self.recognition_model_busy() =>
                {
                    Copy::new(
                        Icon::Microphone,
                        "录下第一句话",
                        "当前操作结束后就可以录音。",
                    )
                }
                Step::Record => {
                    let words = Copy::new(
                        Icon::Microphone,
                        "录下第一句话",
                        "试着说「你好，Speaky」，看看文字出现。",
                    )
                    .tool("开始录音", Action::Record);
                    if self.tour.record_issue {
                        words.notice(
                            "请检查麦克风权限或输入设备。",
                            true,
                            Some(("系统设置", Action::Privacy)),
                        )
                    } else {
                        words
                    }
                }
                Step::Result => self.result_copy(scene),
                Step::Practice => Copy::new(Icon::Guide, "试一次真实输入", "切到任意应用的输入框，按听写快捷键说一句，再按一次结束。文字会写入停止时的输入框，由你决定发送。完成后返回继续。")
                    .tool("查看或更改快捷键", Action::Shortcut),
                Step::Finish => Copy::new(
                    Icon::Home,
                    "工作台已就绪",
                    "随时开始下一段录音，在历史里找回原声和文字。",
                ),
            }
        };
        if self.recording.is_some() && step != Step::Record {
            words.tools = [Some(("停止并保存", Action::Record)), None];
        }
        words
    }

    fn microphone_copy(&self) -> Copy {
        let words = Copy::new(
            Icon::Microphone,
            "让 Speaky 听见你",
            "测试麦克风时说一句，确认有声音输入。",
        );
        let privacy = Some(("系统设置", Action::Privacy));
        match self.tour.microphone {
            Microphone::Untested => words.tool("测试麦克风", Action::CheckMicrophone),
            Microphone::Checking => {
                words.notice("正在检查；如弹出权限提示，请允许访问。", false, None)
            }
            Microphone::Ready => Copy::new(
                Icon::Microphone,
                "已检测到声音",
                "麦克风可以使用，接下来选择识别模型。",
            ),
            Microphone::Denied => words.tool("重试", Action::CheckMicrophone).notice(
                "请在系统设置里允许 Speaky 访问麦克风。",
                true,
                privacy,
            ),
            Microphone::Quiet => words.tool("重试", Action::CheckMicrophone).notice(
                "未检测到声音，请检查权限或输入设备。",
                true,
                privacy,
            ),
            Microphone::Unavailable => words.tool("重试", Action::CheckMicrophone).notice(
                "无法读取音频，请检查输入设备和权限。",
                true,
                privacy,
            ),
        }
    }

    fn model_copy(&self) -> Copy {
        if self.page == Page::Home {
            if self.model_picker.guide_has_no_models() {
                return Copy::new(
                    Icon::Model,
                    "连接识别服务",
                    "没有可用的本地模型时，可以接入已有服务。",
                )
                .tool("配置服务", Action::Service);
            }
            if self.local_model_switching() {
                return Copy::new(
                    Icon::Model,
                    "选择识别模型",
                    "模型正在加载，准备好后即可录音。",
                );
            }
            let words = Copy::new(
                Icon::Model,
                "选择识别模型",
                "选一个模型开始，以后可以从这里随时切换。",
            );
            return if !self.model_picker.guide_is_open() && self.model_switch_blocker().is_none() {
                words.tool("打开模型列表", Action::Models)
            } else {
                words
            };
        }
        let words = Copy::new(
            Icon::Model,
            "选择识别服务",
            "在设置中配置服务地址和模型，以后可以更换。",
        );
        #[cfg(any(windows, target_os = "macos"))]
        if self.settings.guide_can_configure_service()
            && !self.transcribing
            && self.recording.is_none()
            && !self.recognition_model_busy()
        {
            return words.tool("配置服务", Action::Service);
        }
        words
    }

    fn result_copy(&self, scene: Scene) -> Copy {
        if scene == Scene::Archive {
            return Copy::new(
                Icon::History,
                "在历史里找回录音",
                if self.archive.is_empty() {
                    "保存后，录音会在这里出现。"
                } else {
                    "从这里回听原声、复制文字或再次转写。"
                },
            );
        }
        let can_copy = self.recording.is_none()
            && !self.transcribing
            && self.saved_recording.is_none()
            && self.result.is_some();
        let mut words = if scene == Scene::HistoryLink {
            Copy::new(
                Icon::History,
                "需要时，从历史找回",
                "打开录音历史，就能回听原声或复制转写。",
            )
        } else {
            Copy::new(
                Icon::Copy,
                "声音变成了文字",
                "在工作台看转写结果，原声留在录音历史里。",
            )
        };
        if can_copy {
            words = words.tool("复制文字", Action::Copy);
        }
        words = words.tool("查看历史", Action::History);
        if self.tour.copied {
            words.notice("已复制到剪贴板。", false, None)
        } else if self.saved_recording.is_some() && !self.transcribing {
            words.notice("录音已保存，点工作台里的「转写」重试。", false, None)
        } else {
            words
        }
    }
}

fn progress(frame: Frame, hero: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let gap = px(5.);
            let width = (bounds.size.width - gap * (STEPS.len() - 1)) / STEPS.len() as f32;
            for i in 0..STEPS.len() {
                let origin = point(bounds.left() + (width + gap) * i, bounds.top());
                let full = Bounds::new(origin, size(width, bounds.size.height));
                window.paint_quad(
                    fill(full, rgb(if hero { 0x436454 } else { 0xd7e5db })).corner_radii(px(3.)),
                );
                let filled = (frame.progress + 1. - i as f32).clamp(0., 1.);
                if filled > 0. {
                    let active = (frame.progress - i as f32).abs() < 0.63;
                    let color = if hero {
                        0xacedd1
                    } else if active {
                        0x29644e
                    } else {
                        0x88ad9a
                    };
                    let part = Bounds::new(origin, size(width * filled, bounds.size.height));
                    window.paint_quad(fill(part, rgb(color)).corner_radii(px(3.)));
                }
            }
        },
    )
    .w_full()
    .h(px(4.))
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
        (true, false) => (FOREST, LIGHT, 0x275747),
        (false, true) => (0x27493d, 0xe1f4e9, 0x3c6351),
        (false, false) => (0xeaf1eb, 0x244639, 0xddebe1),
    };
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(if primary { 31. } else { 25. }))
        .px(if primary { px(13.) } else { px(8.) })
        .rounded(px(6.))
        .bg(rgb(bg))
        .text_color(rgb(ink))
        .text_size(px(if id == "tour-close" { 15. } else { 12. }))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(hover)))
        .focusable()
        .tab_stop(true)
        .focus(move |style| {
            style
                .border_1()
                .border_color(rgb(if hero { 0xd7f7e7 } else { ACCENT }))
        })
        .on_click(
            cx.listener(move |view, _, window, cx| {
                view.run_guide_action(epoch, action, window, cx)
            }),
        )
}

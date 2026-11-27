use super::*;
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub(super) struct LocalStamp {
    pub(super) year: u16,
    pub(super) month: u16,
    pub(super) day: u16,
    pub(super) hour: u16,
    pub(super) minute: u16,
    pub(super) serial: i64,
}

#[cfg(windows)]
pub(super) fn local_stamp(time: SystemTime) -> Option<LocalStamp> {
    use windows::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
    };
    let unix = time.duration_since(UNIX_EPOCH).ok()?;
    let ticks = 116_444_736_000_000_000_u64
        .checked_add(unix.as_secs().checked_mul(10_000_000)?)?
        .checked_add(u64::from(unix.subsec_nanos() / 100))?;
    let file = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&file, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(LocalStamp {
        year: local.wYear,
        month: local.wMonth,
        day: local.wDay,
        hour: local.wHour,
        minute: local.wMinute,
        serial: date_serial(local.wYear, local.wMonth, local.wDay),
    })
}

#[cfg(not(windows))]
pub(super) fn local_stamp(time: SystemTime) -> Option<LocalStamp> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let serial = i64::try_from(seconds / 86_400).ok()?;
    let (year, month, day) = civil_date(serial);
    Some(LocalStamp {
        year: u16::try_from(year).ok()?,
        month,
        day,
        serial,
        hour: ((seconds / 3600) % 24) as u16,
        minute: ((seconds / 60) % 60) as u16,
    })
}

#[cfg(any(windows, test))]
fn date_serial(year: u16, month: u16, day: u16) -> i64 {
    let month = i64::from(month);
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year = year.rem_euclid(400);
    let month = if month > 2 { month - 3 } else { month + 9 };
    era * 146_097 + year * 365 + year / 4 - year / 100 + (153 * month + 2) / 5 + i64::from(day)
        - 1
        - 719_468
}

fn civil_date(serial: i64) -> (i64, u16, u16) {
    let shifted = serial + 719_468;
    let day = shifted.rem_euclid(146_097);
    let year = (day - day / 1460 + day / 36_524 - day / 146_096) / 365;
    let day = day - (365 * year + year / 4 - year / 100);
    let month = (5 * day + 2) / 153;
    let day = day - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    (
        year + 400 * shifted.div_euclid(146_097) + i64::from(month <= 2),
        month as u16,
        day as u16,
    )
}

#[derive(Default)]
pub(super) struct RedesignHome {
    raw_recent: bool,
    check: Option<Check>,
    last_capture_second: Option<u64>,
}

#[derive(Clone, Copy)]
enum Check {
    Microphone,
    Asr,
    Rewrite,
}

#[derive(Default)]
struct Insights {
    chars: u64,
    timed_chars: u64,
    millis: u64,
    days: BTreeMap<i64, u64>,
}

impl Insights {
    fn collect(archive: &[drafts::Entry]) -> Self {
        let mut total = Self::default();
        for entry in archive {
            let drafts::State::Transcribed { text } = &entry.state else {
                continue;
            };
            let result = entry
                .organized
                .as_ref()
                .filter(|value| value.source == *text)
                .map_or(text.as_str(), |value| value.text.as_str());
            let count = result
                .chars()
                .filter(|character| character.is_alphanumeric())
                .count() as u64;
            if count == 0 {
                continue;
            }
            total.chars += count;
            if entry.duration_millis > 0 {
                total.millis += entry.duration_millis;
                total.timed_chars += count;
            }
            if let Some(date) = entry.recorded_at.or(entry.modified).and_then(local_stamp) {
                *total.days.entry(date.serial).or_default() += count;
            }
        }
        total
    }

    fn streaks(&self, today: i64) -> (usize, usize) {
        let last = if self.days.contains_key(&today) {
            today
        } else {
            today - 1
        };
        let current = (0..)
            .take_while(|days| self.days.contains_key(&(last - days)))
            .count();
        let (mut previous, mut run, mut longest) = (None, 0, 0);
        for &day in self.days.keys() {
            run = if previous == Some(day - 1) {
                run + 1
            } else {
                1
            };
            longest = longest.max(run);
            previous = Some(day);
        }
        (current, longest)
    }
}

fn ratio_card(label: &'static str, value: String, detail: &'static str) -> Div {
    card()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(MUTED()))
                .child(label),
        )
        .child(
            div()
                .text_size(px(26.))
                .line_height(px(34.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(MUTED()))
                .child(detail),
        )
}

fn stat_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{} 小时 {} 分", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{} 分 {:02} 秒", seconds / 60, seconds % 60)
    } else {
        format!("{seconds} 秒")
    }
}

fn home_insights(stats: Insights) -> Div {
    let speed = stats
        .timed_chars
        .saturating_mul(60_000)
        .checked_div(stats.millis);
    let saved = (stats.timed_chars.saturating_mul(60_000) / 40).saturating_sub(stats.millis);
    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(17.))
                .child("洞察"),
        )
        .child(
            div()
                .flex()
                .gap_3()
                .child(ratio_card(
                    "口述字数",
                    stats.chars.to_string(),
                    "以有效转写文字计",
                ))
                .child(ratio_card(
                    "总口述时间",
                    stat_duration(stats.millis / 1000),
                    "有效转写对应录音",
                )),
        )
        .child(
            div()
                .flex()
                .gap_3()
                .child(ratio_card(
                    "平均口述速度",
                    speed.map_or_else(|| "—".into(), |v| format!("{v} 字/分")),
                    "按录音时长计算",
                ))
                .child(ratio_card(
                    "节省时间",
                    stat_duration(saved / 1000),
                    "按打字 40 字/分估算",
                )),
        )
        .child(heatmap(&stats))
}

fn heatmap(stats: &Insights) -> Div {
    let body = card().flex().flex_col().gap_3().child(
        div()
            .flex()
            .justify_between()
            .items_center()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("听写活动"))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED()))
                    .child(if cfg!(windows) {
                        "最近 20 周"
                    } else {
                        "最近 20 周 · UTC"
                    }),
            ),
    );
    let Some(today) = local_stamp(SystemTime::now()).map(|time| time.serial) else {
        return body.child(div().text_color(rgb(MUTED())).child("无法取得日历日期。"));
    };
    let start = today - (today + 3).rem_euclid(7) - 19 * 7;
    let (current, longest) = stats.streaks(today);
    let headings = [
        ("活跃天数", stats.days.len()),
        ("当前连续", current),
        ("最长连续", longest),
    ];
    body.child(
        div()
            .flex()
            .gap_3()
            .children(headings.map(|(label, count)| {
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{count} 天")),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED()))
                            .child(label),
                    )
            })),
    )
    .child(
        div().flex().flex_col().gap_1().children(
            ["一", "二", "三", "四", "五", "六", "日"]
                .into_iter()
                .enumerate()
                .map(|(row, day)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .w(px(16.))
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(rgb(MUTED()))
                                .child(day),
                        )
                        .children((0..20).map(|week| {
                            let serial = start + row as i64 + week * 7;
                            let count = stats.days.get(&serial).copied().unwrap_or_default();
                            let level = match count {
                                0 => 0,
                                1..=499 => 1,
                                500..=1499 => 2,
                                1500..=2999 => 3,
                                _ => 4,
                            };
                            let (year, month, day) = civil_date(serial);
                            let tip = format!("{year} 年 {month} 月 {day} 日 · {count} 字");
                            div()
                                .id(("v30-heatmap", serial as usize))
                                .h(px(13.))
                                .flex_1()
                                .min_w(px(0.))
                                .rounded(px(3.))
                                .bg(rgb(if serial > today {
                                    RAIL()
                                } else {
                                    crate::theme::heat()[level]
                                }))
                                .when(serial <= today, |cell| {
                                    cell.tooltip(move |_, cx| {
                                        cx.new(|_| ActionTooltip(tip.clone().into())).into()
                                    })
                                })
                        }))
                }),
        ),
    )
    .child(
        div()
            .flex()
            .justify_between()
            .items_center()
            .text_size(px(11.))
            .text_color(rgb(MUTED()))
            .child({
                let (_, first, _) = civil_date(start);
                let (_, last, _) = civil_date(today);
                format!("{first} 月至 {last} 月")
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child("更少")
                    .children(
                        crate::theme::heat()
                            .map(|color| div().size(px(10.)).rounded(px(2.)).bg(rgb(color))),
                    )
                    .child("更多"),
            ),
    )
}

impl Speaky {
    pub(super) fn v30_home(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let active = self.recording.is_some() || self.starting;
        let practice = self.tour.at(Step::Record);
        let preparing = practice && self.tour.preparing;
        let blocked = !active
            && (preparing
                || self.transcribing
                || self.unsaved.is_some()
                || self.recognition_model_busy());
        let show_saved = self
            .saved_recording
            .as_ref()
            .is_some_and(|path| !practice || self.tour.is_practice_draft(path));
        let settings = config::Config::load().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_wrap()
                    .children(self.tour.mark(Anchor::Workspace))
                    .child(page_heading(
                        "说话，不要打字",
                        "用快捷键在其他应用听写，或在这里直接开始录音。",
                    ))
                    .child(
                        div()
                            .relative()
                            .flex()
                            .items_center()
                            .when(practice, |row| row.flex_wrap())
                            .gap_2()
                            .children(self.tour.mark(Anchor::Record))
                            .when(practice && !active && !blocked, |row| {
                                row.child(self.v30_shortcut_control(cx))
                            })
                            .when(active, |row| {
                                row.child(
                                    button("取消", false).id("v30-cancel-recording").on_click(
                                        cx.listener(|view, _, _, cx| view.cancel_recording(cx)),
                                    ),
                                )
                            })
                            .child(
                                action(true)
                                    .id("record-action")
                                    .relative()
                                    .h(px(34.))
                                    .px_3()
                                    .gap_2()
                                    .child(
                                        if active { Icon::Stop } else { Icon::Microphone }
                                            .render(ACCENT_INK()),
                                    )
                                    .child(if preparing {
                                        "正在准备录音"
                                    } else if self.starting {
                                        "正在打开麦克风"
                                    } else if active {
                                        "停止并保存"
                                    } else {
                                        "开始录音"
                                    })
                                    .children(self.tour.mark(Anchor::RecordButton))
                                    .when(!blocked && self.studio_controls_active(), |button| {
                                        button.focusable().tab_stop(true).on_click(cx.listener(
                                            |view, _, _, cx| {
                                                if view.tour.at(Step::Record) {
                                                    view.toggle_practice_recording(cx);
                                                } else {
                                                    view.toggle_recording(
                                                        RecordingStart::Button,
                                                        cx,
                                                    );
                                                }
                                            },
                                        ))
                                    })
                                    .when(blocked, |button| button.opacity(0.5).cursor_default()),
                            ),
                    ),
            )
            .when(
                active || self.transcribing || show_saved || self.practice_result_ready(),
                |home| home.child(self.v30_capture(show_saved, cx)),
            )
            .child(
                div()
                    .flex()
                    .gap_4()
                    .when(!wide, |layout| layout.flex_col())
                    .child(home_insights(Insights::collect(&self.archive)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.))
                            .gap_3()
                            .when(wide, |column| column.w(px(232.)).flex_none())
                            .when(!practice || active || blocked, |column| {
                                column.child(self.v30_shortcuts(&settings, cx))
                            })
                            .child(self.v30_runtime(&settings, cx))
                            .child(self.v30_recent(cx))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(MUTED()))
                                            .child(concat!("领铄 · ", env!("CARGO_PKG_VERSION"))),
                                    )
                                    .child(
                                        button("全部历史记录", false)
                                            .id("v30-all-history")
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.open_page(Page::History, cx)
                                            })),
                                    ),
                            ),
                    ),
            )
    }

    fn v30_capture(&self, show_saved: bool, cx: &mut Context<Self>) -> Div {
        let guide_practice = self.tour.at(Step::Record);
        let guide_result = self.practice_result_ready();
        let seconds = self.recording.as_ref().map_or(0, |recording| {
            recording.recorder.sample_count() as u64 / 16_000
        });
        let status = if guide_result {
            "本次转写"
        } else if self.recording.is_some() {
            "实时转写"
        } else if self.starting {
            "正在准备麦克风"
        } else if self.transcribing {
            "正在转写"
        } else if show_saved {
            "录音已保存"
        } else {
            "最近转写"
        };
        let caption = self
            .tour
            .practice_result()
            .or_else(|| self.recording.as_ref().and_then(Recording::caption))
            .or_else(|| {
                ((!guide_practice || show_saved) && !self.finalizing_preview.is_empty())
                    .then_some(self.finalizing_preview.as_str())
            })
            .unwrap_or_else(|| {
                if self.starting {
                    "正在打开输入设备…"
                } else if self.transcribing {
                    "正在识别整段录音…"
                } else if guide_practice && show_saved {
                    "可重新转写已保存的录音。"
                } else {
                    "等待语音…"
                }
            });
        card()
            .relative()
            .flex()
            .flex_col()
            .gap_2()
            .children(self.tour.mark(Anchor::Transcript))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .flex_wrap()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(status))
                    .when(!guide_result && self.recording.is_some(), |row| {
                        row.child(self.header_wave.clone()).child(
                            div().text_color(rgb(MUTED())).child(format!(
                                "{:02}:{:02}",
                                seconds / 60,
                                seconds % 60
                            )),
                        )
                    })
                    .when_some(
                        self.saved_recording.as_ref().filter(|_| {
                            !guide_result
                                && show_saved
                                && !self.transcribing
                                && !self.recognition_model_busy()
                        }),
                        |row, draft| {
                            let draft = draft.clone();
                            row.child(
                                button(
                                    if guide_practice {
                                        "重试转写"
                                    } else {
                                        "转写"
                                    },
                                    false,
                                )
                                .id("transcribe-saved")
                                .when(guide_practice, |button| button.focusable().tab_stop(true))
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.transcribe(
                                            draft.clone(),
                                            RecordingDestination::Archive,
                                            cx,
                                        )
                                    },
                                )),
                            )
                        },
                    )
                    .when(guide_result, |row| {
                        row.child(
                            div()
                                .flex()
                                .items_center()
                                .flex_wrap()
                                .gap_2()
                                .when(self.tour.has_rewrite(), |actions| {
                                    actions.children([(false, "原文"), (true, "改写")].map(
                                        |(rewrite, label)| {
                                            button(label, self.tour.showing_rewrite() == rewrite)
                                                .id(("practice-version", u32::from(rewrite)))
                                                .focusable()
                                                .tab_stop(true)
                                                .on_click(cx.listener(move |view, _, _, cx| {
                                                    view.select_practice_version(rewrite, cx);
                                                }))
                                        },
                                    ))
                                })
                                .child(
                                    button(
                                        if self.tour.copied {
                                            "已复制"
                                        } else {
                                            "复制文字"
                                        },
                                        false,
                                    )
                                    .id("practice-copy-result")
                                    .focusable()
                                    .tab_stop(true)
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.copy_practice_result(cx)),
                                    ),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .id("transcript")
                    .max_h(px(152.))
                    .overflow_y_scroll()
                    .track_scroll(&self.transcript_scroll)
                    .text_size(px(14.))
                    .child(
                        div()
                            .when_some(self.tour.result_appearance(), |text, (opacity, offset)| {
                                text.relative().top(offset).opacity(opacity)
                            })
                            .child(caption.to_owned()),
                    ),
            )
            .when_some(
                self.recording.as_ref().and_then(|recording| {
                    #[cfg(windows)]
                    {
                        recording.delivery_issue.as_ref()
                    }
                    #[cfg(not(windows))]
                    {
                        let _ = recording;
                        None::<&String>
                    }
                }),
                |card, issue| card.child(div().text_color(rgb(ERROR())).child(issue.clone())),
            )
            .when_some(
                self.saved_recording
                    .as_deref()
                    .filter(|_| show_saved && !self.transcribing)
                    .and_then(|path| self.transcript_feedback_for(path)),
                |card, message| card.child(crate::settings::detail(message.to_owned())),
            )
    }

    pub(super) fn v30_registered_shortcut(&self) -> Option<&str> {
        self.hotkey_status.strip_prefix("全局快捷键 ")
    }

    fn v30_shortcut_control(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let shortcut = self.v30_registered_shortcut().unwrap_or("快捷键未注册");
        button(shortcut.to_owned(), false)
            .id("home-edit-shortcut")
            .relative()
            .children(self.tour.mark(Anchor::Shortcut))
            .when(self.tour.at(Step::Record), |key| key.cursor_default())
            .when(!self.tour.at(Step::Record), |key| {
                key.focusable().tab_stop(true).on_click(
                    cx.listener(|view, _, window, cx| view.open_shortcut_settings(window, cx)),
                )
            })
    }

    fn v30_shortcuts(&self, settings: &config::Config, cx: &mut Context<Self>) -> Div {
        let language = speaky_core::session::LANGUAGES
            .iter()
            .find(|(code, _)| *code == settings.core.asr.language)
            .map_or("自动识别", |(_, label)| *label);
        card()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED()))
                    .child("口述（任意输入框）"),
            )
            .child(self.v30_shortcut_control(cx))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED()))
                    .child("取消转写（保留原音）"),
            )
            .child(studio_ui::keycap("Esc"))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED()))
                    .child("推荐识别语言"),
            )
            .child(
                button(language, false)
                    .id("v30-home-language")
                    .on_click(cx.listener(|view, _, _, cx| view.open_page(Page::Settings, cx))),
            )
    }

    fn v30_runtime(&self, settings: &config::Config, cx: &mut Context<Self>) -> Div {
        let mic = self.microphone_label(&settings.core.asr);
        let level = self.diagnostic_level();
        #[cfg(not(target_vendor = "win7"))]
        let asr = model_picker::selected_label(settings);
        let rewrite = settings.llm.organizer_service();
        let llm = if settings.llm.enabled && !rewrite.model.is_empty() {
            rewrite.model.as_str()
        } else {
            "未启用改写"
        };
        let busy = self.redesign_home.check.is_some()
            || self.recording.is_some()
            || self.starting
            || self.transcribing;
        card()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("运行状态"))
                    .child(
                        button(
                            if self.redesign_home.check.is_some() {
                                "检测中…"
                            } else {
                                "全链路体检"
                            },
                            false,
                        )
                        .id("v30-check-all")
                        .when(!busy, |button| {
                            button.on_click(cx.listener(|view, _, _, cx| view.v30_start_check(cx)))
                        })
                        .when(busy, |button| button.opacity(0.55)),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED()))
                    .child(self.diagnostic_permission()),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap_1()
                    .justify_between()
                    .children(self.tour.mark(Anchor::Input))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .child(mic.to_owned()),
                    )
                    .child(
                        button(
                            if self.diagnostics.checking {
                                "检测中"
                            } else {
                                "测麦"
                            },
                            false,
                        )
                        .id("test-microphone")
                        .on_click(cx.listener(|view, _, _, cx| view.test_microphone(cx))),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(rgb(BORDER()))
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative((level * 6.).sqrt().clamp(0., 1.)))
                            .rounded_full()
                            .bg(rgb(crate::theme::success())),
                    ),
            )
            .when(!self.diagnostic_message().is_empty(), |card| {
                card.child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(MUTED()))
                        .child(self.diagnostic_message().to_owned()),
                )
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(self.tour.mark(Anchor::Model))
                    .child({
                        #[cfg(target_vendor = "win7")]
                        let selector = self.model_picker_button(cx);
                        #[cfg(not(target_vendor = "win7"))]
                        let selector = div().text_size(px(12.)).child(asr);
                        div().min_w(px(0.)).flex_1().child(selector)
                    })
                    .child(button("测试", false).id("v30-home-test-asr").on_click(
                        cx.listener(|view, _, _, cx| view.check_recognition_service(cx)),
                    )),
            )
            .when(cfg!(target_vendor = "win7"), |body| {
                body.children(self.model_picker_panel(cx))
                    .children(self.model_load_notice(false, cx))
            })
            .when_some(self.settings_notice(), |body, result| {
                let (text, color) = match result {
                    Ok(text) => (text, MUTED()),
                    Err(text) => (text, ERROR()),
                };
                body.child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(color))
                        .child(text.clone()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(rgb(if settings.llm.enabled {
                                INK()
                            } else {
                                crate::theme::warning()
                            }))
                            .child(llm.to_owned()),
                    )
                    .child(
                        button("测试", false)
                            .id("v30-home-test-llm")
                            .on_click(cx.listener(|view, _, _, cx| view.benchmark(cx))),
                    ),
            )
            .child(self.benchmark_result())
    }

    fn v30_recent(&self, cx: &mut Context<Self>) -> Div {
        let result = self.result.as_ref();
        let recording = result.and_then(|(target, _)| match target {
            history::Target::Draft(path) => self.archive.iter().find(|entry| &entry.path == path),
            history::Target::History(_) => None,
        });
        let source = recording.and_then(|entry| match &entry.state {
            drafts::State::Transcribed { text } => Some(text.as_str()),
            _ => None,
        });
        let current = source
            .or_else(|| result.map(|(_, text)| text.as_str()))
            .unwrap_or_default();
        let raw = recording
            .and_then(|entry| entry.original_text.as_deref())
            .unwrap_or(current);
        let rewritten =
            recording.and_then(|entry| crate::redesign_library::current_rewrite(entry, source));
        let show_raw = self.redesign_home.raw_recent || rewritten.is_none();
        let chosen = match rewritten {
            Some(_) if show_raw => raw,
            Some(text) => text,
            None => current,
        };
        let target = result.map(|(target, _)| target.clone());
        let preview = if chosen.is_empty() {
            "还没有转写记录。".into()
        } else {
            chosen.chars().take(115).collect::<String>()
                + if chosen.chars().count() > 115 {
                    "…"
                } else {
                    ""
                }
        };
        card()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("最近转写"))
                    .when(rewritten.is_some(), |row| {
                        row.child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    button("润色稿", !show_raw)
                                        .id("v30-recent-polished")
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.redesign_home.raw_recent = false;
                                            cx.notify();
                                        })),
                                )
                                .child(button("原文", show_raw).id("v30-recent-raw").on_click(
                                    cx.listener(|view, _, _, cx| {
                                        view.redesign_home.raw_recent = true;
                                        cx.notify();
                                    }),
                                )),
                        )
                    }),
            )
            .child(
                div()
                    .id("v30-recent-text")
                    .cursor_pointer()
                    .text_size(px(12.))
                    .line_height(px(20.))
                    .hover(|text| text.text_color(rgb(ACCENT())))
                    .child(preview)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if let Some(target) = &target {
                            view.show_library_target(target.clone(), window, cx);
                        } else {
                            view.open_page(Page::History, cx);
                        }
                    })),
            )
            .when_some(
                recording
                    .filter(|entry| self.saved_recording.as_ref() != Some(&entry.path))
                    .and_then(|entry| self.transcript_feedback_for(&entry.path)),
                |card, message| card.child(crate::settings::detail(message.to_owned())),
            )
            .when_some(recording, |card, entry| {
                let path = entry.path.clone();
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED()))
                                .child(stat_duration(entry.duration_seconds)),
                        )
                        .child(icon_button(Icon::Play, "播放原音").on_click(cx.listener(
                            move |view, _, _, cx| {
                                view.stop_playback();
                                match playback::Playback::start(&path) {
                                    Ok(play) => {
                                        view.playback = Some(play);
                                        view.clear_feedback(Page::Home);
                                    }
                                    Err(error) => {
                                        view.set_feedback(Page::Home, format!("播放失败：{error}"));
                                    }
                                }
                                cx.notify();
                            },
                        ))),
                )
            })
            .when(!chosen.is_empty(), |card| {
                let displayed = chosen.to_owned();
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon_button(Icon::Copy, "复制文本").on_click(cx.listener(
                            move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(displayed.clone()));
                            },
                        )))
                        .when_some(result, |row, (target, text)| {
                            row.children(self.correction_button(
                                "edit-recent",
                                target.clone(),
                                text,
                                cx,
                            ))
                        }),
                )
            })
    }

    fn v30_start_check(&mut self, cx: &mut Context<Self>) {
        if self.redesign_home.check.is_some()
            || self.recording.is_some()
            || self.starting
            || self.transcribing
            || self.recognition_checking()
            || self.settings.polish_checking
        {
            return;
        }
        self.redesign_home.check = Some(Check::Microphone);
        self.test_microphone(cx);
    }

    pub(super) fn v30_tick(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Home {
            let second = self
                .recording
                .as_ref()
                .map(|r| r.recorder.sample_count() as u64 / 16_000);
            if second != self.redesign_home.last_capture_second {
                self.redesign_home.last_capture_second = second;
                cx.notify();
            }
        }
        if self.recording.is_some() || self.starting || self.transcribing {
            self.redesign_home.check = None;
            return;
        }
        match self.redesign_home.check {
            Some(Check::Microphone) if !self.diagnostics.checking => {
                self.redesign_home.check = Some(Check::Asr);
                self.check_recognition_service(cx);
            }
            Some(Check::Asr) if !self.recognition_checking() => {
                if self.organizer_enabled {
                    self.redesign_home.check = Some(Check::Rewrite);
                    self.benchmark(cx);
                } else {
                    self.redesign_home.check = None;
                }
                cx.notify();
            }
            Some(Check::Rewrite) if !self.settings.polish_checking => {
                self.redesign_home.check = None;
                cx.notify();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{civil_date, date_serial, Insights};

    #[test]
    fn local_day_calendar_crosses_month_and_year_boundaries() {
        assert_eq!(date_serial(1970, 1, 1), 0);
        for (year, month, day) in [(2024, 2, 29), (2026, 11, 27), (2027, 1, 1)] {
            assert_eq!(
                civil_date(date_serial(year, month, day)),
                (i64::from(year), month, day)
            );
        }
    }

    #[test]
    fn streak_starts_yesterday_if_today_is_empty() {
        let mut insights = Insights::default();
        for day in [1, 2, 3, 6, 7] {
            insights.days.insert(day, 18);
        }
        assert_eq!(insights.streaks(8), (2, 3));
        assert_eq!(insights.streaks(9), (0, 3));
    }
}

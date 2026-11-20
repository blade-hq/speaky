mod audio;
mod config;
mod drafts;
mod engines;
mod history;
mod hotkey;
mod icons;
mod overlay;
#[cfg(target_os = "macos")]
mod service_ui;
mod settings;
mod terms;
#[cfg(target_os = "macos")]
mod terms_ui;

use gpui::{
    div, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba, size, App,
    Application, Bounds, BoxShadow, ClipboardItem, Context, Div, FontWeight, KeyBinding, Menu,
    MenuItem, MouseButton, ScrollHandle, Stateful, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions,
};
use icons::Icon;
use std::{path::PathBuf, process::Child, time::Duration};

const SIDEBAR: u32 = 0x252b3e;
const INK: u32 = 0xeef0fa;
const MUTED: u32 = 0xa4abcd;
const BORDER: u32 = 0x3a435d;
const ACCENT: u32 = 0x9eaaff;
const ACCENT_INK: u32 = 0x171d39;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    History,
    Terms,
    Settings,
}

gpui::actions!(speaky, [OpenSettings, Quit]);

const SILENT_INPUT: &str =
    "录音没有收到声音。请检查 macOS「声音 → 输入」的电平；在虚拟机中还需检查宿主机的麦克风共享。";

fn is_silent_recording(wav: &[u8]) -> bool {
    wav.starts_with(b"RIFF")
        && wav.get(8..12) == Some(b"WAVE")
        && wav.get(36..40) == Some(b"data")
        && wav
            .get(44..)
            .is_some_and(|samples| samples.iter().all(|&byte| byte == 0))
}

#[cfg(test)]
mod tests {
    use super::is_silent_recording;

    #[test]
    fn rejects_zero_pcm_without_rejecting_quiet_pcm_or_unknown_formats() {
        let mut wav = [0; 48];
        wav[..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        wav[36..40].copy_from_slice(b"data");
        assert!(is_silent_recording(&wav));
        wav[46] = 1;
        assert!(!is_silent_recording(&wav));
        assert!(!is_silent_recording(&[0; 48]));
    }
}

#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../Info.plist");

struct Recording {
    recorder: audio::AudioRecorder,
    draft: PathBuf,
    preview_enabled: bool,
    preview_cursor: usize,
    preview_pending: bool,
    preview: String,
    generation: uuid::Uuid,
}

struct Speaky {
    recording: Option<Recording>,
    global_hotkey: Option<hotkey::GlobalHotkey>,
    hotkey_status: String,
    status: String,
    result: String,
    transcribing: bool,
    transcript_scroll: ScrollHandle,
    page: Page,
    archive: Vec<drafts::Entry>,
    saved_recording: Option<PathBuf>,
    unsaved: Option<(PathBuf, Vec<u8>)>,
    overlay: Option<WindowHandle<overlay::RecordingOverlay>>,
    playback: Option<Child>,
    invalid_terms: Option<String>,
    settings: settings::SettingsPage,
}

impl Speaky {
    fn new(hotkey: Option<hotkey::GlobalHotkey>, hotkey_status: String) -> Self {
        Self {
            recording: None,
            global_hotkey: hotkey,
            hotkey_status,
            status: String::new(),
            result: history::recent(1)
                .into_iter()
                .next()
                .map(|item| item.text)
                .unwrap_or_default(),
            transcribing: false,
            transcript_scroll: ScrollHandle::new(),
            page: Page::Home,
            archive: drafts::list(),
            saved_recording: None,
            unsaved: None,
            overlay: None,
            playback: None,
            invalid_terms: None,
            settings: settings::SettingsPage::default(),
        }
    }

    fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording.is_some() {
            self.stop_recording(cx);
        } else {
            self.begin_recording(drafts::new_path(), false, cx);
        }
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(mut recording) = self.recording.take() else {
            return;
        };
        self.close_overlay(cx);
        let audio = recording.recorder.stop();
        if audio.is_empty() {
            self.status = "没有收到音频，请检查麦克风权限。".into();
        } else {
            self.persist(recording.draft, audio);
        }
        cx.notify();
    }

    fn persist(&mut self, draft: PathBuf, audio: Vec<u8>) {
        let silent = is_silent_recording(&audio);
        match drafts::save(&draft, &audio) {
            Ok(()) => {
                self.archive = drafts::list();
                self.saved_recording = Some(draft);
                self.status = if silent {
                    format!("录音已保存。{SILENT_INPUT}")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.status = format!("保存失败：{error}。音频仍在内存，请重试保存。");
                self.unsaved = Some((draft, audio));
            }
        }
    }

    fn begin_recording(&mut self, draft: PathBuf, resume: bool, cx: &mut Context<Self>) {
        if self.recording.is_some() || self.transcribing || self.unsaved.is_some() {
            return;
        }
        let result = (|| {
            let asr = config::Config::load()
                .map_err(|error| error.to_string())?
                .core
                .asr;
            let mut recorder = audio::AudioRecorder::new_with_name(
                asr.audio_device,
                asr.audio_device_name.as_deref(),
                asr.audio_gain,
            )?;
            let mut preview_cursor = 0;
            if resume {
                let wav = std::fs::read(&draft).map_err(|error| error.to_string())?;
                recorder.resume_wav(&wav)?;
                preview_cursor = (wav.len() - 44) / 2;
            } else {
                recorder.start()?;
            }
            Ok::<_, String>((recorder, preview_cursor))
        })();
        match result {
            Ok((recorder, preview_cursor)) => {
                self.close_overlay(cx);
                self.stop_playback();
                self.transcript_scroll.set_offset(point(px(0.), px(0.)));
                self.recording = Some(Recording {
                    recorder,
                    draft,
                    preview_enabled: false,
                    preview_cursor,
                    preview_pending: false,
                    preview: String::new(),
                    generation: uuid::Uuid::new_v4(),
                });
                self.status.clear();
                let finish = cx.weak_entity();
                match overlay::RecordingOverlay::open(cx, move |cx| {
                    let _ = finish.update(cx, Speaky::stop_recording);
                }) {
                    Ok(overlay) => self.overlay = Some(overlay),
                    Err(error) => self.status = format!("录音中，悬浮窗未能打开：{error}"),
                }
            }
            Err(error) => self.status = error,
        }
        cx.notify();
    }

    fn close_overlay(&mut self, cx: &mut App) {
        if let Some(handle) = self.overlay.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn stop_playback(&mut self) {
        if let Some(mut playback) = self.playback.take() {
            let _ = playback.kill();
            let _ = playback.wait();
        }
    }

    fn checkpoint(&mut self, cx: &mut Context<Self>) {
        if let Some(recording) = &mut self.recording {
            if let Err(error) = drafts::save(&recording.draft, &recording.recorder.wav_snapshot()) {
                self.status = format!("保存录音草稿失败：{error}");
                cx.notify();
            }
            if recording.preview_enabled && !recording.preview_pending {
                if let Some((next, audio)) = recording
                    .recorder
                    .wav_preview_since(recording.preview_cursor)
                {
                    recording.preview_cursor = next;
                    if is_silent_recording(&audio) {
                        if recording.preview.is_empty()
                            && recording.recorder.get_audio_level() == 0.0
                        {
                            self.status = SILENT_INPUT.into();
                            cx.notify();
                        }
                    } else {
                        if self.status == SILENT_INPUT {
                            self.status.clear();
                            cx.notify();
                        }
                        let draft = recording.draft.clone();
                        self.preview(draft, audio, cx);
                    }
                }
            }
        }
    }

    fn activity(&self) -> Div {
        let mut counts = [0_usize; 7];
        for entry in &self.archive {
            if let Some(days) = entry
                .modified
                .and_then(|time| time.elapsed().ok())
                .map(|elapsed| elapsed.as_secs() / 86400)
                .filter(|days| *days < 7)
            {
                counts[6 - days as usize] += 1;
            }
        }
        let highest = (*counts.iter().max().unwrap_or(&0)).max(1) as f32;
        card()
            .child(div().text_sm().child("近七天"))
            .child(
                div()
                    .mt_3()
                    .flex()
                    .gap_2()
                    .children(counts.into_iter().enumerate().map(|(index, count)| {
                        let label = if index == 6 {
                            "24 小时".into()
                        } else {
                            format!("{} 天前", 6 - index)
                        };
                        let tooltip = format!("{label} · {count} 条录音");
                        div()
                            .id(("activity-day", index))
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(
                                div().h(px(37.)).flex().items_end().child(
                                    div()
                                        .w(px(20.))
                                        .h(px(3. + 32. * count as f32 / highest))
                                        .rounded_full()
                                        .bg(rgb(if count == 0 { BORDER } else { ACCENT })),
                                ),
                            )
                            .child(div().text_xs().text_color(rgb(MUTED)).child(label))
                            .tooltip(move |_, cx| {
                                cx.new(|_| ActionTooltip(tooltip.clone().into())).into()
                            })
                    })),
            )
    }

    fn preview(&mut self, draft: PathBuf, audio: Vec<u8>, cx: &mut Context<Self>) {
        let Some(recording) = self.recording.as_mut().filter(|recording| {
            recording.draft == draft && recording.preview_enabled && !recording.preview_pending
        }) else {
            return;
        };
        recording.preview_pending = true;
        let generation = recording.generation;
        let task = cx.background_spawn(async move {
            let config = config::Config::load().map_err(|error| error.to_string())?;
            let engine = engines::create_engine(&config)
                .ok_or_else(|| "请先配置语音识别服务。".to_owned())?;
            terms::transcribe(engine.as_ref(), &audio, &config.core.asr.language)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                let Some(recording) = view.recording.as_mut().filter(|recording| {
                    recording.draft == draft && recording.generation == generation
                }) else {
                    return;
                };
                recording.preview_pending = false;
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        if !recording.preview.is_empty() {
                            recording.preview.push(' ');
                        }
                        recording.preview.push_str(text.trim());
                        view.transcript_scroll.scroll_to_bottom();
                        cx.notify();
                    }
                    Ok(_) => {}
                    Err(error) => log::debug!("Recording preview failed: {error}"),
                }
            });
        })
        .detach();
    }

    fn transcribe(&mut self, draft: PathBuf, cx: &mut Context<Self>) {
        if self.recording.is_some() || self.transcribing || self.unsaved.is_some() {
            return;
        }
        self.transcribing = true;
        self.stop_playback();
        let completed_draft = draft.clone();
        self.status.clear();
        let task = cx.background_spawn(async move {
            let audio = std::fs::read(&draft).map_err(|error| error.to_string())?;
            if is_silent_recording(&audio) {
                return Err(SILENT_INPUT.to_owned());
            }
            let transcribe = || {
                let config = config::Config::load().map_err(|error| error.to_string())?;
                let engine = engines::create_engine(&config)
                    .ok_or_else(|| "请先配置语音识别服务。".to_owned())?;
                let text = terms::transcribe(engine.as_ref(), &audio, &config.core.asr.language)
                    .map_err(|error| error.to_string())?;
                if text.trim().is_empty() {
                    return Err("未识别到文字。".to_owned());
                }
                drafts::set_state(&draft, drafts::State::Transcribed { text: text.clone() })
                    .map_err(|error| format!("保存转写结果失败：{error}"))?;
                history::add(&text, engine.name(), false);
                Ok(text)
            };
            transcribe().map_err(|error| format!("{error}\n录音已保存至 {}", draft.display()))
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.transcribing = false;
                view.archive = drafts::list();
                match result {
                    Ok(text) => {
                        if view.saved_recording.as_ref() == Some(&completed_draft) {
                            view.saved_recording = None;
                        }
                        view.result = text;
                        view.transcript_scroll.set_offset(point(px(0.), px(0.)));
                        view.status.clear();
                    }
                    Err(error) => view.status = error,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn button(label: impl Into<gpui::SharedString>, primary: bool) -> Div {
    action(primary).px_2().child(label.into())
}

fn action(primary: bool) -> Div {
    div()
        .h(px(25.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(if primary {
            rgb(ACCENT).into()
        } else {
            rgba(0xb3c1ff13)
        })
        .text_color(rgb(if primary { ACCENT_INK } else { INK }))
        .text_size(px(11.))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(if primary { 0xbdc5ff } else { BORDER })))
}

struct ActionTooltip(gpui::SharedString);

impl Render for ActionTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(SIDEBAR))
            .text_color(rgb(0xffffff))
            .text_size(px(11.))
            .child(self.0.clone())
    }
}

fn icon_button(icon: Icon, label: &'static str) -> Stateful<Div> {
    action(false)
        .id(label)
        .w(px(25.))
        .child(icon.render(INK))
        .tooltip(move |_, cx| cx.new(|_| ActionTooltip(label.into())).into())
}

fn card() -> Div {
    div()
        .p_3()
        .rounded(px(15.))
        .border_1()
        .border_color(rgba(0xc1ceff20))
        .bg(rgba(0x343e5c83))
        .shadow(vec![BoxShadow {
            color: rgba(0x05091655).into(),
            offset: point(px(0.), px(9.)),
            blur_radius: px(24.),
            spread_radius: px(-3.),
        }])
}

fn metric(label: &'static str, value: String) -> Div {
    div()
        .flex_1()
        .px_2()
        .child(
            div()
                .text_size(px(19.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(label))
}

fn history_row() -> Div {
    div()
        .group("history-row")
        .flex()
        .items_start()
        .gap_2()
        .px_2p5()
        .py_1p5()
        .border_b_1()
        .border_color(rgba(0xc1ceff18))
        .hover(|row| row.bg(rgba(0xaabaff12)))
}

fn history_meta(label: String) -> Div {
    div()
        .w(px(62.))
        .flex_none()
        .pt(px(4.))
        .text_size(px(11.))
        .line_height(px(15.))
        .text_color(rgb(MUTED))
        .child(label)
}

fn history_actions() -> Div {
    div().flex_none().flex().items_center().gap_1()
}

impl Speaky {
    fn home(&self, cx: &mut Context<Self>) -> Div {
        let completed = self
            .archive
            .iter()
            .filter(|entry| matches!(entry.state, drafts::State::Transcribed { .. }));
        let seconds = self
            .archive
            .iter()
            .map(|entry| entry.duration_seconds)
            .sum::<u64>();
        let characters: usize = completed
            .clone()
            .map(|entry| match &entry.state {
                drafts::State::Transcribed { text } => text.chars().count(),
                _ => 0,
            })
            .sum();
        let recording = self.recording.is_some();
        let preview = self
            .recording
            .as_ref()
            .map(|recording| recording.preview.as_str())
            .unwrap_or_default();
        let show_preview = self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.preview_enabled);
        let saved_recording = self
            .saved_recording
            .as_ref()
            .filter(|_| !recording && !self.transcribing && self.unsaved.is_none());

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("听写"),
                    )
                    .when_some(
                        self.hotkey_status.strip_prefix("全局快捷键 "),
                        |row, shortcut| {
                            row.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .rounded_md()
                                    .text_size(px(11.))
                                    .text_color(rgb(MUTED))
                                    .child(shortcut.to_owned()),
                            )
                        },
                    )
                    .when(!self.transcribing && self.unsaved.is_none(), |row| {
                        let label = if recording {
                            "停止并保存"
                        } else {
                            "开始录音"
                        };
                        let icon = if recording {
                            Icon::Stop
                        } else {
                            Icon::Microphone
                        };
                        row.child(
                            action(true)
                                .id("record-action")
                                .size(px(32.))
                                .child(icon.render(ACCENT_INK))
                                .tooltip(move |_, cx| {
                                    cx.new(|_| ActionTooltip(label.into())).into()
                                })
                                .on_click(cx.listener(|view, _, _, cx| view.toggle_recording(cx))),
                        )
                    }),
            )
            .child(
                card()
                    .min_h(px(108.))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(if recording && !show_preview {
                                "录音中"
                            } else if self.transcribing {
                                "转写中…"
                            } else if show_preview {
                                "听写预览"
                            } else if saved_recording.is_some() {
                                "录音已保存"
                            } else {
                                "最近转写"
                            })
                            .when(recording && !show_preview, |row| {
                                row.child(
                                    button("开启转写预览", false).id("enable-preview").on_click(
                                        cx.listener(|view, _, _, cx| {
                                            if let Some(recording) = &mut view.recording {
                                                recording.preview_enabled = true;
                                                cx.notify();
                                            }
                                        }),
                                    ),
                                )
                            })
                            .when_some(saved_recording, |row, draft| {
                                let draft = draft.clone();
                                row.child(button("转写", false).id("transcribe-saved").on_click(
                                    cx.listener(move |view, _, _, cx| {
                                        view.transcribe(draft.clone(), cx)
                                    }),
                                ))
                            })
                            .when(
                                !recording
                                    && !self.transcribing
                                    && self.saved_recording.is_none()
                                    && !self.result.is_empty(),
                                |row| {
                                    row.child(icon_button(Icon::Copy, "复制文本").on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|view, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                view.result.clone(),
                                            ));
                                        }),
                                    ))
                                },
                            ),
                    )
                    .child(
                        div()
                            .id("transcript")
                            .mt_2()
                            .max_h(px(174.))
                            .overflow_y_scroll()
                            .track_scroll(&self.transcript_scroll)
                            .when(
                                if recording || self.transcribing {
                                    preview.is_empty()
                                } else {
                                    self.result.is_empty()
                                },
                                |text| text.text_color(rgb(MUTED)),
                            )
                            .child(
                                if self.transcribing
                                    || saved_recording.is_some()
                                    || (recording && !show_preview)
                                {
                                    String::new()
                                } else if show_preview {
                                    if preview.is_empty() {
                                        "等待识别…".into()
                                    } else {
                                        preview.to_owned()
                                    }
                                } else if self.result.is_empty() {
                                    "暂无转写".into()
                                } else {
                                    self.result.clone()
                                },
                            ),
                    ),
            )
            .child(
                card()
                    .flex()
                    .items_center()
                    .child(metric("转写字符", characters.to_string()))
                    .child(div().w(px(1.)).h(px(29.)).bg(rgb(BORDER)))
                    .child(metric("录音", self.archive.len().to_string()))
                    .child(div().w(px(1.)).h(px(29.)).bg(rgb(BORDER)))
                    .child(metric(
                        "总时长",
                        format!("{}:{:02}", seconds / 60, seconds % 60),
                    )),
            )
            .child(self.activity())
    }

    fn history_page(&self, cx: &mut Context<Self>) -> Div {
        let older_text = history::recent(usize::MAX)
            .into_iter()
            .filter(|item| {
                !self.archive.iter().any(|entry| {
                matches!(&entry.state, drafts::State::Transcribed { text } if text == &item.text)
            })
            })
            .collect::<Vec<_>>();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("录音历史"),
            )
            .when(self.transcribing, |body| {
                body.child(div().text_color(rgb(MUTED)).child("正在转写…"))
            })
            .when(self.archive.is_empty() && older_text.is_empty(), |body| {
                body.child(card().text_color(rgb(MUTED)).child("暂无录音"))
            })
            .when(!self.archive.is_empty() || !older_text.is_empty(), |body| {
                body.child(
                    div()
                        .overflow_hidden()
                        .rounded(px(11.))
                        .border_1()
                        .border_color(rgba(0xc1ceff29))
                        .bg(rgba(0x232c47a0))
                        .children(self.archive.iter().enumerate().map(|(index, entry)| {
                            let play = entry.path.clone();
                            let retry = entry.path.clone();
                            let resume = entry.path.clone();
                            let reveal = entry.path.clone();
                            let (label, text, pending_action) = match &entry.state {
                                drafts::State::Draft => ("未转写", None, "转写"),
                                drafts::State::Cancelled => ("转写已取消", None, "重试"),
                                drafts::State::Transcribed { text } => ("", Some(text.clone()), ""),
                            };
                            let date = entry
                                .modified
                                .and_then(|time| time.elapsed().ok())
                                .map(|elapsed| match elapsed.as_secs() {
                                    0..=59 => "刚刚".to_owned(),
                                    60..=3599 => format!("{} 分钟前", elapsed.as_secs() / 60),
                                    3600..=86399 => format!("{} 小时前", elapsed.as_secs() / 3600),
                                    _ => format!("{} 天前", elapsed.as_secs() / 86400),
                                })
                                .unwrap_or_default();
                            history_row()
                                .id(("recording", index))
                                .child(history_meta(format!(
                                    "{date}\n{}:{:02}",
                                    entry.duration_seconds / 60,
                                    entry.duration_seconds % 60
                                )))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .pt(px(4.))
                                        .when(text.is_none(), |body| body.text_color(rgb(MUTED)))
                                        .child(text.clone().unwrap_or_else(|| label.into())),
                                )
                                .child(
                                    history_actions()
                                        .when(text.is_some(), |actions| {
                                            actions
                                                .invisible()
                                                .group_hover("history-row", |style| style.visible())
                                        })
                                        .child(icon_button(Icon::Play, "播放录音").on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |view, _, _, cx| {
                                                if view.recording.is_none()
                                                    && view.unsaved.is_none()
                                                {
                                                    view.stop_playback();
                                                    match std::process::Command::new(
                                                        "/usr/bin/afplay",
                                                    )
                                                    .arg(&play)
                                                    .spawn()
                                                    {
                                                        Ok(child) => {
                                                            view.playback = Some(child);
                                                            view.status.clear();
                                                        }
                                                        Err(error) => {
                                                            view.status =
                                                                format!("播放失败：{error}")
                                                        }
                                                    }
                                                    cx.notify();
                                                }
                                            }),
                                        ))
                                        .child(
                                            if text.is_some() {
                                                icon_button(Icon::Retry, "重新转写")
                                            } else {
                                                button(pending_action, false)
                                                    .id(("transcribe-recording", index))
                                            }
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |view, _, _, cx| {
                                                    view.transcribe(retry.clone(), cx);
                                                }),
                                            ),
                                        )
                                        .when(
                                            !matches!(
                                                entry.state,
                                                drafts::State::Transcribed { .. }
                                            ),
                                            |row| {
                                                row.child(button("继续录音", false).on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |view, _, _, cx| {
                                                        view.begin_recording(
                                                            resume.clone(),
                                                            true,
                                                            cx,
                                                        );
                                                    }),
                                                ))
                                            },
                                        )
                                        .child(
                                            icon_button(Icon::Folder, "在访达中显示录音")
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |_, _, _, cx| {
                                                        cx.reveal_path(&reveal)
                                                    }),
                                                ),
                                        )
                                        .when_some(text, |row, text| {
                                            row.child(
                                                icon_button(Icon::Copy, "复制文本").on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |_, _, _, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(text.clone()),
                                                        );
                                                    }),
                                                ),
                                            )
                                        }),
                                )
                        }))
                        .children(older_text.into_iter().enumerate().map(|(index, item)| {
                            history_row()
                                .id(("previous-transcript", index))
                                .child(history_meta("过往转写".into()))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .pt(px(4.))
                                        .child(item.text.clone()),
                                )
                                .child(
                                    history_actions()
                                        .invisible()
                                        .group_hover("history-row", |style| style.visible())
                                        .child(icon_button(Icon::Copy, "复制文本").on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |_, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    item.text.clone(),
                                                ));
                                            }),
                                        )),
                                )
                        })),
                )
            })
    }

    #[cfg(target_os = "macos")]
    fn edit_terms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self
            .invalid_terms
            .clone()
            .map_or_else(terms::load, Ok)
            .map_err(|error| error.to_string())
            .and_then(|source| terms_ui::edit(window, &source));
        match editor {
            Ok(receiver) => cx
                .spawn(async move |view, cx| {
                    let (edited, result) = match receiver.await {
                        Ok(Ok(Some(edited))) => {
                            let result = terms::save(&edited).map_err(|error| error.to_string());
                            (Some(edited), result)
                        }
                        Ok(Ok(None)) => return,
                        Ok(Err(error)) => (None, Err(error)),
                        Err(_) => (None, Err("编辑窗口已关闭。".into())),
                    };
                    let _ = view.update(cx, |view, cx| {
                        view.status = match result {
                            Ok(()) => {
                                view.invalid_terms = None;
                                String::new()
                            }
                            Err(error) => {
                                if edited.is_some() {
                                    view.invalid_terms = edited;
                                }
                                format!("保存失败：{error}")
                            }
                        };
                        cx.notify();
                    });
                })
                .detach(),
            Err(error) => {
                self.status = format!("无法打开常用词编辑器：{error}");
                cx.notify();
            }
        }
    }

    fn terms_page(&self, cx: &mut Context<Self>) -> Div {
        let display = terms::load().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("常用词"),
            )
            .child(
                card()
                    .when(display.is_empty(), |card| card.text_color(rgb(MUTED)))
                    .child(if display.is_empty() {
                        "暂无常用词".into()
                    } else {
                        display
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(cfg!(target_os = "macos"), |row| {
                        row.child(button("编辑词表", true).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_view, _, _window, _cx| {
                                #[cfg(target_os = "macos")]
                                _view.edit_terms(_window, _cx);
                            }),
                        ))
                    })
                    .child(button("粘贴并保存", false).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            if let Some(source) =
                                cx.read_from_clipboard().and_then(|item| item.text())
                            {
                                view.status = match terms::save(&source) {
                                    Ok(()) => String::new(),
                                    Err(error) => format!("保存失败：{error}"),
                                };
                                cx.notify();
                            }
                        }),
                    ))
                    .child(icon_button(Icon::Copy, "复制词表").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| {
                            if let Ok(source) = terms::load() {
                                cx.write_to_clipboard(ClipboardItem::new_string(source));
                            }
                        }),
                    )),
            )
    }
}

impl Render for Speaky {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, status_color) = match (self.page, self.settings_notice()) {
            _ if self.unsaved.is_some() => (self.status.as_str(), 0xffb1a2),
            (Page::Settings, Some(Ok(message))) => (message.as_str(), ACCENT),
            (Page::Settings, Some(Err(message))) => (message.as_str(), 0xffb1a2),
            _ if self.status.is_empty()
                && self.global_hotkey.is_none()
                && self.page == Page::Home =>
            {
                (self.hotkey_status.as_str(), MUTED)
            }
            _ => (self.status.as_str(), MUTED),
        };
        div()
            .size_full()
            .flex()
            .bg(linear_gradient(
                152.,
                linear_color_stop(rgba(0x303a58dc), 0.),
                linear_color_stop(rgba(0x121622ef), 1.),
            ))
            .text_color(rgb(INK))
            .text_size(px(12.))
            .line_height(px(17.))
            .child(
                div()
                    .my_2()
                    .ml_2()
                    .w(px(122.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_2()
                    .pb_2()
                    .rounded(px(16.))
                    .border_1()
                    .border_color(rgba(0xb9c5f020))
                    .bg(rgba(0x141a2cb3))
                    .child(
                        div()
                            .h(px(64.))
                            .flex()
                            .items_end()
                            .px_2()
                            .pb_2()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0xffffff))
                            .child("Speaky")
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.start_window_move();
                            }),
                    )
                    .children(
                        [
                            (Page::Home, "首页", Icon::Home),
                            (Page::History, "历史", Icon::History),
                            (Page::Terms, "常用词", Icon::Terms),
                            (Page::Settings, "设置", Icon::Settings),
                        ]
                        .map(|(page, label, icon)| {
                            let selected = self.page == page;
                            let color = if selected { ACCENT } else { MUTED };
                            div()
                                .h(px(28.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .rounded(px(10.))
                                .text_color(rgb(color))
                                .when(selected, |row| row.bg(rgba(0x9eaaff25)))
                                .child(icon.render(color))
                                .child(label)
                                .cursor_pointer()
                                .hover(|row| row.bg(rgba(0x9eaaff30)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |view, _, _, cx| {
                                        view.open_page(page, cx);
                                    }),
                                )
                        }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .child(div().h(px(33.)).flex_none().on_mouse_down(
                        MouseButton::Left,
                        |_, window, _| {
                            window.start_window_move();
                        },
                    ))
                    .when(
                        !status.is_empty() || self.unsaved.is_some() || self.playback.is_some(),
                        |page| {
                            page.child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_4()
                                    .py_1()
                                    .text_size(px(11.))
                                    .text_color(rgb(status_color))
                                    .child(div().flex_1().min_w(px(0.)).child(status.to_owned()))
                                    .when(self.playback.is_some(), |row| {
                                        row.child(button("停止播放", false).on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|view, _, _, cx| {
                                                view.stop_playback();
                                                cx.notify();
                                            }),
                                        ))
                                    })
                                    .when(self.unsaved.is_some(), |row| {
                                        row.child(button("重试保存", true).on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|view, _, _, cx| {
                                                if let Some((draft, audio)) = view.unsaved.take() {
                                                    view.persist(draft, audio);
                                                    cx.notify();
                                                }
                                            }),
                                        ))
                                    }),
                            )
                        },
                    )
                    .child(
                        div()
                            .id(self.page as usize)
                            .flex_1()
                            .min_h(px(0.))
                            .min_w(px(0.))
                            .overflow_y_scroll()
                            .px_4()
                            .pt_2()
                            .pb_3()
                            .child(match self.page {
                                Page::Home => self.home(cx),
                                Page::History => self.history_page(cx),
                                Page::Terms => self.terms_page(cx),
                                Page::Settings => self.settings_page(cx),
                            }),
                    ),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(790.), px(610.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(665.), px(435.))),
                    window_background: WindowBackgroundAppearance::Blurred,
                    titlebar: Some(TitlebarOptions {
                        title: Some("Speaky".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(18.), px(18.))),
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    window.on_window_should_close(cx, |window, _| {
                        window.minimize_window();
                        false
                    });
                    cx.new(|cx| {
                        let shortcut = config::Config::load()
                            .as_ref()
                            .map(settings::selected_shortcut)
                            .unwrap_or_default();
                        let (hotkey, hotkey_status) = match hotkey::GlobalHotkey::register(shortcut)
                        {
                            Ok((handle, presses)) => {
                                settings::listen_for_hotkey(presses, cx);
                                (Some(handle), format!("全局快捷键 {}", shortcut.label()))
                            }
                            Err(error) => (None, error),
                        };
                        let executor = cx.background_executor().clone();
                        cx.spawn(async move |entity, cx| loop {
                            executor.timer(Duration::from_secs(2)).await;
                            if entity.update(cx, Speaky::checkpoint).is_err() {
                                break;
                            }
                        })
                        .detach();
                        let executor = cx.background_executor().clone();
                        cx.spawn(async move |entity, cx| loop {
                            executor.timer(Duration::from_millis(65)).await;
                            if entity
                                .update(cx, |view, cx| {
                                    if let (Some(recording), Some(handle)) =
                                        (&view.recording, view.overlay)
                                    {
                                        let level = recording.recorder.get_audio_level();
                                        let _ = handle
                                            .update(cx, |panel, _, cx| panel.push_level(level, cx));
                                    }
                                    if view.playback.as_mut().is_some_and(|child| {
                                        matches!(child.try_wait(), Ok(Some(_)))
                                    }) {
                                        view.playback = None;
                                        cx.notify();
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        })
                        .detach();
                        Speaky::new(hotkey, hotkey_status)
                    })
                },
            )
            .expect("could not open Speaky");
        cx.on_app_quit(move |cx| {
            let _ = window.update(cx, |view, _, cx| {
                view.stop_recording(cx);
                if let Some((draft, audio)) = view.unsaved.take() {
                    view.persist(draft, audio);
                }
                view.stop_playback();
            });
            async {}
        })
        .detach();
        cx.on_action(move |_: &OpenSettings, cx| {
            let _ = window.update(cx, |view, _, cx| view.open_page(Page::Settings, cx));
        });
        cx.on_action(move |_: &Quit, cx| {
            let ready = window.update(cx, |view, _, cx| {
                if view.recording.is_some() {
                    view.stop_recording(cx);
                }
                if view.unsaved.is_some() {
                    return false;
                }
                if view.transcribing {
                    view.status = "正在保存转写结果，完成后即可退出。".into();
                    cx.notify();
                    return false;
                }
                view.stop_playback();
                true
            });
            if ready.unwrap_or(true) {
                cx.quit();
            }
        });
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
        ]);
        cx.set_menus(vec![Menu {
            name: "Speaky".into(),
            items: vec![
                MenuItem::action("设置…", OpenSettings),
                MenuItem::action("退出 Speaky", Quit),
            ],
        }]);
        cx.activate(true);
    });
}

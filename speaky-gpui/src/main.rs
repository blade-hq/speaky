#![cfg_attr(windows, windows_subsystem = "windows")]

mod correction_ui;
#[cfg(windows)]
mod floating_control;
#[cfg(windows)]
mod hotkey_capture;
mod icons;
#[cfg(target_os = "macos")]
mod mac_dictation;
mod model_picker;
mod onboarding;
mod overlay;
mod polish_ui;
#[cfg(any(target_os = "macos", windows))]
mod service_ui;
mod settings;
mod studio_ui;
#[cfg(any(target_os = "macos", windows))]
mod terms_ui;
#[cfg(windows)]
mod window_chrome;
#[cfg(windows)]
mod windows_dialog;
#[cfg(windows)]
mod windows_floating;

use gpui::{
    div, point, prelude::*, px, rgb, size, App, Application, Bounds, ClipboardItem, Context, Div,
    FontWeight, KeyBinding, Menu, MenuItem, MouseButton, ScrollHandle, Stateful, TitlebarOptions,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions,
};
use icons::Icon;
use onboarding::{Anchor, Step};
#[cfg(windows)]
use speaky_core::dictation_target;
use speaky_core::{
    audio, config, drafts, engines, history, hotkey, model_packs, playback, polish,
    polish_benchmark, polish_preferences,
    session::{is_silent_recording, LivePreview, SILENT_INPUT},
    terms,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const BACKGROUND: u32 = 0xf5f5f2;
const RAIL: u32 = 0xeceee9;
const SURFACE: u32 = 0xffffff;
const SIDEBAR: u32 = 0xe9eee9;
const INK: u32 = 0x202924;
const MUTED: u32 = 0x526159;
const BORDER: u32 = 0xd7dfd8;
const ACCENT: u32 = 0x285848;
const ACCENT_INK: u32 = 0xffffff;
const SELECTION: u32 = 0xdcebe1;
const ERROR: u32 = 0x973a30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    History,
    Terms,
    Settings,
}

gpui::actions!(
    speaky,
    [OpenSettings, Quit, FocusNext, FocusPrevious, DismissGuide]
);

#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../Info.plist");

struct Recording {
    recorder: audio::AudioRecorder,
    draft: PathBuf,
    destination: RecordingDestination,
    preview_enabled: bool,
    preview_pending: Option<uuid::Uuid>,
    preview: LivePreview,
    generation: uuid::Uuid,
    #[cfg(windows)]
    delivery_issue: Option<String>,
}

impl Recording {
    fn caption(&self) -> Option<&str> {
        if !self.preview_enabled {
            return None;
        }
        if !self.preview.text.is_empty() {
            return Some(&self.preview.text);
        }
        Some(self.preview.error.as_deref().unwrap_or("等待语音…"))
    }
}

enum RecordingDestination {
    Archive,
    #[cfg(windows)]
    External(Option<dictation_target::DictationTarget>),
    #[cfg(target_os = "macos")]
    MacExternal(Result<mac_dictation::Target, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordingStart {
    Button,
    Desktop,
    Resume,
}

struct PendingSave {
    draft: PathBuf,
    audio: Vec<u8>,
    destination: RecordingDestination,
    transcribe_after_save: bool,
}

struct Speaky {
    recording: Option<Recording>,
    global_hotkey: Option<hotkey::GlobalHotkey>,
    hotkey_status: String,
    status: String,
    result: Option<(history::Target, String)>,
    correction: correction_ui::CorrectionEditor,
    transcribing: bool,
    transcript_scroll: ScrollHandle,
    page: Page,
    tour: onboarding::Tour,
    model_picker: model_picker::ModelPicker,
    archive: Vec<drafts::Entry>,
    saved_recording: Option<PathBuf>,
    unsaved: Option<PendingSave>,
    // Keep the last visible words on screen while the complete recording is decoded.
    finalizing_preview: String,
    overlay: Option<WindowHandle<overlay::RecordingOverlay>>,
    #[cfg(windows)]
    floating: Option<WindowHandle<floating_control::FloatingControl>>,
    #[cfg(windows)]
    floating_retry: bool,
    playback: Option<playback::Playback>,
    invalid_terms: Option<String>,
    settings: settings::SettingsPage,
    organizer_enabled: bool,
    organizer_learning: bool,
    organizer: polish_ui::Organizer,
}

impl Speaky {
    fn studio_controls_active(&self) -> bool {
        !self.tour.is_active()
    }

    fn new(
        hotkey: Option<hotkey::GlobalHotkey>,
        hotkey_status: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = config::Config::load();
        let model_picker = model_picker::ModelPicker::new(config.as_ref().ok());
        let organizer = config.unwrap_or_default().llm;
        let archive = drafts::list();
        Self {
            recording: None,
            global_hotkey: hotkey,
            hotkey_status,
            status: String::new(),
            result: history::latest(&archive),
            correction: correction_ui::CorrectionEditor::default(),
            transcribing: false,
            transcript_scroll: ScrollHandle::new(),
            page: Page::Home,
            tour: onboarding::Tour::load(cx),
            model_picker,
            archive,
            saved_recording: None,
            unsaved: None,
            finalizing_preview: String::new(),
            overlay: None,
            #[cfg(windows)]
            floating: None,
            #[cfg(windows)]
            floating_retry: false,
            playback: None,
            invalid_terms: None,
            settings: settings::SettingsPage::default(),
            organizer_enabled: organizer.enabled,
            organizer_learning: organizer.learns_preferences(),
            organizer: polish_ui::Organizer::default(),
        }
    }

    fn toggle_recording(&mut self, source: RecordingStart, cx: &mut Context<Self>) {
        if self.recording.is_some() {
            self.stop_recording(cx);
        } else {
            self.begin_recording(drafts::new_path(), source, cx);
        }
    }

    fn recognition_model_busy(&self) -> bool {
        if self.correction.busy {
            return true;
        }
        self.local_model_switching()
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(mut recording) = self.recording.take() else {
            return;
        };
        #[cfg(windows)]
        if let RecordingDestination::External(Some(target)) = &recording.destination {
            target.stop_preview();
        }
        #[cfg(target_os = "macos")]
        if let RecordingDestination::MacExternal(target) = &mut recording.destination {
            if let Err(error) = target
                .as_ref()
                .map_or(Ok(()), mac_dictation::Target::require_current)
            {
                *target = Err(error);
            }
        }
        self.close_overlay(cx);
        let audio = recording.recorder.stop();
        if audio.is_empty() {
            self.status = "没有收到音频，请检查麦克风权限。".into();
            if self.tour.at(Step::Record) {
                self.tour.record_issue = true;
            }
            #[cfg(windows)]
            {
                self.floating_retry = true;
            }
        } else {
            let transcribe_after_save = recording.preview_enabled
                || !matches!(recording.destination, RecordingDestination::Archive);
            if recording.preview_enabled {
                self.finalizing_preview = recording.preview.text;
            }
            self.persist(
                PendingSave {
                    draft: recording.draft,
                    audio,
                    destination: recording.destination,
                    transcribe_after_save,
                },
                cx,
            );
        }
        cx.notify();
    }

    fn persist(&mut self, pending: PendingSave, cx: &mut Context<Self>) {
        let silent = is_silent_recording(&pending.audio);
        match drafts::save(&pending.draft, &pending.audio) {
            Ok(()) => {
                self.archive = drafts::list();
                self.saved_recording = Some(pending.draft.clone());
                self.status = if silent {
                    format!("录音已保存。{SILENT_INPUT}")
                } else {
                    String::new()
                };
                if !silent && pending.transcribe_after_save {
                    if self.tour.at(Step::Record)
                        && self.page == Page::Home
                        && matches!(&pending.destination, RecordingDestination::Archive)
                    {
                        self.tour.copied = false;
                        self.show_tour_step(Step::Result, cx);
                    }
                    self.transcribe(pending.draft, pending.destination, cx);
                } else if silent && self.tour.at(Step::Record) {
                    self.tour.record_issue = true;
                }
            }
            Err(error) => {
                self.status = format!("保存失败：{error}。音频仍在内存，请重试保存。");
                self.unsaved = Some(pending);
            }
        }
    }

    fn begin_recording(&mut self, draft: PathBuf, source: RecordingStart, cx: &mut Context<Self>) {
        if self.recording.is_some()
            || self.transcribing
            || self.unsaved.is_some()
            || self.recognition_model_busy()
        {
            return;
        }
        self.model_picker.close();
        // Bind the destination at the user's shortcut or floating-button press.
        // The UIA worker can resolve the editor while the microphone opens.
        #[cfg(windows)]
        let destination = if source == RecordingStart::Desktop {
            match dictation_target::DictationTarget::capture_foreground() {
                dictation_target::Foreground::Speaky => RecordingDestination::Archive,
                dictation_target::Foreground::External(target) => {
                    RecordingDestination::External(target)
                }
            }
        } else {
            RecordingDestination::Archive
        };
        #[cfg(target_os = "macos")]
        let destination = if source == RecordingStart::Desktop {
            match mac_dictation::Target::capture() {
                Ok(None) => RecordingDestination::Archive,
                Ok(Some(target)) => RecordingDestination::MacExternal(Ok(target)),
                Err(error) => RecordingDestination::MacExternal(Err(error)),
            }
        } else {
            RecordingDestination::Archive
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let destination = RecordingDestination::Archive;
        let result = (|| {
            let config = config::Config::load().map_err(|error| error.to_string())?;
            let preview_enabled = true;
            let asr = config.core.asr;
            let mut recorder = audio::AudioRecorder::new_with_name(
                asr.audio_device,
                asr.audio_device_name.as_deref(),
                asr.audio_gain,
            )?;
            let mut preview_cursor = 0;
            if source == RecordingStart::Resume {
                let wav = std::fs::read(&draft).map_err(|error| error.to_string())?;
                recorder.resume_wav(&wav)?;
                preview_cursor = (wav.len() - 44) / 2;
            } else {
                recorder.start()?;
            }
            Ok::<_, String>((recorder, preview_cursor, preview_enabled))
        })();
        match result {
            Ok((recorder, preview_cursor, preview_enabled)) => {
                self.tour.record_issue = false;
                if self.tour.at(Step::Record) {
                    self.tour.request_reveal();
                }
                self.close_overlay(cx);
                self.stop_playback();
                self.finalizing_preview.clear();
                self.transcript_scroll.set_offset(point(px(0.), px(0.)));
                self.recording = Some(Recording {
                    recorder,
                    draft,
                    destination,
                    preview_enabled,
                    preview_pending: None,
                    preview: LivePreview::new(preview_cursor),
                    generation: uuid::Uuid::new_v4(),
                    #[cfg(windows)]
                    delivery_issue: None,
                });
                self.status.clear();
                #[cfg(windows)]
                {
                    self.floating_retry = false;
                }
                self.show_recording_overlay(cx);
            }
            Err(error) => {
                self.status = error;
                if self.tour.at(Step::Record) {
                    self.tour.record_issue = true;
                }
                #[cfg(windows)]
                {
                    self.floating_retry = true;
                }
            }
        }
        cx.notify();
    }

    fn show_recording_overlay(&mut self, cx: &mut Context<Self>) {
        let Some(recording) = &self.recording else {
            return;
        };
        #[cfg(windows)]
        let show_controls = self.floating.is_none();
        #[cfg(not(windows))]
        let show_controls = true;
        if self.overlay.is_some() || (!show_controls && !recording.preview_enabled) {
            return;
        }
        let finish = cx.weak_entity();
        match overlay::RecordingOverlay::open(
            cx,
            show_controls,
            recording.preview_enabled,
            move |cx| {
                let _ = finish.update(cx, Speaky::stop_recording);
            },
        ) {
            Ok(overlay) => self.overlay = Some(overlay),
            Err(error) => self.status = format!("录音中，悬浮窗未能打开：{error}"),
        }
    }

    #[cfg(windows)]
    fn set_floating_button_enabled(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if enabled && self.floating.is_none() {
            let app = cx.weak_entity();
            let handle = floating_control::FloatingControl::open(cx, move |cx| {
                let _ = app.update(cx, |view, cx| {
                    view.toggle_recording(RecordingStart::Desktop, cx)
                });
            })?;
            self.floating = Some(handle);
            self.close_overlay(cx);
            self.show_recording_overlay(cx);
            self.sync_floating(cx);
        } else if !enabled {
            if let Some(handle) = self.floating.take() {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
            self.close_overlay(cx);
            self.show_recording_overlay(cx);
        }
        Ok(())
    }

    #[cfg(windows)]
    fn sync_floating(&self, cx: &mut Context<Self>) {
        use floating_control::State;
        let Some(handle) = self.floating else { return };
        let (state, level) = if let Some(recording) = &self.recording {
            (
                State::Recording {
                    delivery_issue: recording.delivery_issue.is_some(),
                },
                recording.recorder.get_audio_level(),
            )
        } else {
            let state = if self.transcribing {
                State::Transcribing
            } else if self.unsaved.is_some() {
                State::Busy("待保存录音")
            } else if self.correction.busy {
                State::Busy("保存修改中")
            } else if self.local_model_switching() {
                State::Busy("切换模型中")
            } else if self.floating_retry {
                State::Retry
            } else {
                State::Idle
            };
            (state, 0.)
        };
        let _ = handle.update(cx, |panel, window, cx| panel.sync(state, level, window, cx));
    }

    fn close_overlay(&mut self, cx: &mut App) {
        if let Some(handle) = self.overlay.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn save_before_exit(&mut self, cx: &mut Context<Self>) {
        if let Some(recording) = &mut self.recording {
            #[cfg(windows)]
            if let RecordingDestination::External(Some(target)) = &recording.destination {
                target.stop_preview();
            }
            recording.destination = RecordingDestination::Archive;
            recording.preview_enabled = false;
        }
        if let Some(pending) = &mut self.unsaved {
            pending.destination = RecordingDestination::Archive;
            pending.transcribe_after_save = false;
        }
        self.stop_recording(cx);
    }

    fn stop_playback(&mut self) {
        self.playback = None;
    }

    fn checkpoint(&mut self, cx: &mut Context<Self>) {
        if let Some(recording) = &mut self.recording {
            if let Err(error) = drafts::save(&recording.draft, &recording.recorder.wav_snapshot()) {
                self.status = format!("保存录音草稿失败：{error}");
                cx.notify();
            }
        }
    }

    fn poll_preview(&mut self, cx: &mut Context<Self>) {
        let Some(recording) = &mut self.recording else {
            return;
        };
        #[cfg(windows)]
        {
            let issue = match &recording.destination {
                RecordingDestination::Archive => None,
                RecordingDestination::External(None) => Some(
                    "开始听写时未找到输入框。请停止录音，点一下聊天输入框，再用快捷键或悬浮按钮开始听写。".into(),
                ),
                RecordingDestination::External(Some(target)) => target.delivery_issue(),
            };
            if recording.delivery_issue != issue {
                recording.delivery_issue = issue;
                cx.notify();
            }
        }
        if !recording.preview_enabled
            || recording.preview_pending.is_some()
            || recording
                .preview
                .retry_after
                .is_some_and(|t| Instant::now() < t)
        {
            return;
        }
        let Some(chunk) = recording.recorder.wav_preview_since(
            recording.preview.phrase_start,
            recording.preview.last_decoded,
            recording.preview.text.is_empty(),
        ) else {
            return;
        };
        if chunk.wav.is_empty() {
            recording.preview.phrase_start =
                chunk.next_start_after_quiet(recording.preview.phrase_start);
            recording.preview.last_decoded = chunk.end;
            if self.status != SILENT_INPUT
                && recording.preview.text.is_empty()
                && recording.recorder.get_audio_level() == 0.0
            {
                self.status = SILENT_INPUT.into();
                cx.notify();
            }
            return;
        }
        if self.status == SILENT_INPUT {
            self.status.clear();
            cx.notify();
        }
        let draft = recording.draft.clone();
        let generation = recording.generation;
        recording.preview_pending = Some(generation);
        self.preview(draft, generation, chunk, cx);
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
            .child(div().font_weight(FontWeight::SEMIBOLD).child("近七天"))
            .child(
                div()
                    .mt_3()
                    .flex()
                    .gap_1()
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
                                        .w(px(12.))
                                        .h(px(3. + 32. * count as f32 / highest))
                                        .rounded_full()
                                        .bg(rgb(if count == 0 { BORDER } else { ACCENT })),
                                ),
                            )
                            .tooltip(move |_, cx| {
                                cx.new(|_| ActionTooltip(tooltip.clone().into())).into()
                            })
                    })),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child("6 天前")
                    .child("近 24 小时"),
            )
    }

    fn preview(
        &mut self,
        draft: PathBuf,
        generation: uuid::Uuid,
        chunk: audio::PreviewAudio,
        cx: &mut Context<Self>,
    ) {
        let audio::PreviewAudio { end, complete, wav } = chunk;
        self.model_picker.pending_previews += 1;
        let task = cx.background_spawn(async move {
            let config = config::Config::load().map_err(|error| error.to_string())?;
            let engine = engines::recognition_engine(&config)?;
            terms::transcribe(
                engine.as_ref(),
                &wav,
                &config.core.asr.language,
                engines::TranscriptionPass::Preview,
            )
            .map_err(|error| error.to_string())
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.model_picker.pending_previews -= 1;
                if view.model_picker.pending_previews == 0 && view.recording.is_none() {
                    cx.notify();
                }
                let Some(recording) = view.recording.as_mut().filter(|recording| {
                    recording.draft == draft && recording.preview_pending == Some(generation)
                }) else {
                    return;
                };
                recording.preview_pending = None;
                if !recording.preview_enabled || recording.generation != generation {
                    return;
                }
                match recording.preview.accept(end, complete, result) {
                    Ok(()) => {
                        recording.preview.error = None;
                        recording.preview.retry_after = None;
                        #[cfg(windows)]
                        if let RecordingDestination::External(Some(target)) = &recording.destination
                        {
                            target.preview(&recording.preview.text);
                        }
                        view.transcript_scroll.scroll_to_bottom();
                        cx.notify();
                    }
                    Err(error) => {
                        log::debug!("Recording preview failed: {error}");
                        recording.preview.error = Some(format!("实时转写暂时不可用：{error}"));
                        recording.preview.retry_after =
                            Some(Instant::now() + Duration::from_secs(4));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn transcribe(
        &mut self,
        draft: PathBuf,
        destination: RecordingDestination,
        cx: &mut Context<Self>,
    ) {
        if self.recording.is_some()
            || self.transcribing
            || self.unsaved.is_some()
            || self.recognition_model_busy()
        {
            return;
        }
        self.model_picker.close();
        self.transcribing = true;
        self.stop_playback();
        if self.saved_recording.as_ref() != Some(&draft) {
            self.finalizing_preview.clear();
        }
        let completed_draft = draft.clone();
        self.status.clear();
        #[cfg(target_os = "macos")]
        let mac_target = match destination {
            RecordingDestination::Archive => None,
            RecordingDestination::MacExternal(target) => Some(target),
        };
        let task = cx.background_spawn(async move {
            let audio = std::fs::read(&draft).map_err(|error| error.to_string())?;
            if is_silent_recording(&audio) {
                return Err(SILENT_INPUT.to_owned());
            }
            let transcribe = || {
                let config = config::Config::load().map_err(|error| error.to_string())?;
                let engine = engines::recognition_engine(&config)?;
                let text = terms::transcribe(
                    engine.as_ref(),
                    &audio,
                    &config.core.asr.language,
                    engines::TranscriptionPass::Final,
                )
                .map_err(|error| error.to_string())?;
                if text.trim().is_empty() {
                    return Err("未识别到文字。".to_owned());
                }
                history::record(&draft, &text, engine.name())
                    .map_err(|error| format!("保存转写结果失败：{error}"))?;
                #[cfg(not(target_os = "macos"))]
                let status = match destination {
                    RecordingDestination::Archive => String::new(),
                    #[cfg(windows)]
                    RecordingDestination::External(Some(target)) => match target.insert(&text) {
                        Ok(()) => "已输入到原来的输入框。".into(),
                        Err(error) => format!("{error} 转写已保存，可在听写或历史中复制。"),
                    },
                    #[cfg(windows)]
                    RecordingDestination::External(None) => {
                        "没有找到可输入的文本框。转写已保存，可在听写或历史中复制。".into()
                    }
                };
                #[cfg(target_os = "macos")]
                let status = String::new();
                Ok((text, status))
            };
            transcribe().map_err(|error| format!("{error}\n录音已保存至 {}", draft.display()))
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.transcribing = false;
                view.archive = drafts::list();
                match result {
                    Ok((text, status)) => {
                        view.finalizing_preview.clear();
                        if view.saved_recording.as_ref() == Some(&completed_draft) {
                            view.saved_recording = None;
                        }
                        view.transcript_scroll.set_offset(point(px(0.), px(0.)));
                        #[cfg(target_os = "macos")]
                        let status = mac_target.map_or(status, |target| {
                            match target.and_then(|target| target.insert(&text, cx)) {
                                Ok(notice) => notice.into(),
                                Err(error) => {
                                    if error == mac_dictation::ACCESSIBILITY {
                                        mac_dictation::request_access();
                                    }
                                    format!("{error} 转写已保存，可在听写或历史中复制。")
                                }
                            }
                        });
                        view.result = Some((history::Target::Draft(completed_draft), text));
                        view.status = status;
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
    action(primary).px_3().child(label.into())
}

fn action(primary: bool) -> Div {
    div()
        .h(px(30.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .bg(rgb(if primary { ACCENT } else { SIDEBAR }))
        .text_color(rgb(if primary { ACCENT_INK } else { INK }))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|style| style.bg(rgb(if primary { 0x204b3e } else { BORDER })))
}

struct ActionTooltip(gpui::SharedString);

impl Render for ActionTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(0x27382f))
            .text_color(rgb(0xffffff))
            .text_size(px(11.))
            .child(self.0.clone())
    }
}

fn icon_button(icon: Icon, label: &'static str) -> Stateful<Div> {
    action(false)
        .id(label)
        .w(px(30.))
        .child(icon.render(INK))
        .tooltip(move |_, cx| cx.new(|_| ActionTooltip(label.into())).into())
}

fn card() -> Div {
    div()
        .p_4()
        .rounded(px(12.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(SURFACE))
}

fn page_heading(title: &'static str, description: &'static str) -> Div {
    div()
        .min_w(px(0.))
        .flex_1()
        .child(
            div()
                .text_size(px(29.))
                .line_height(px(38.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_color(rgb(MUTED)).child(description))
}

fn home_command(title: &'static str, description: &str, color: u32) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .py_2()
        .min_h(px(58.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(color))
                        .child(description.to_owned()),
                ),
        )
}

fn metric(label: &'static str, value: String) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .py_2()
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(div().text_size(px(12.)).text_color(rgb(MUTED)).child(label))
        .child(
            div()
                .text_size(px(19.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
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
        .border_color(rgb(BORDER))
        .hover(|row| row.bg(rgb(0xf3f7f3)))
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
    fn nav_item(
        &self,
        page: Page,
        label: &'static str,
        icon: Icon,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.page == page;
        let color = if selected { ACCENT } else { MUTED };
        div()
            .id(("sidebar-page", page as usize))
            .relative()
            .h(px(40.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(7.))
            .when(
                self.studio_controls_active()
                    || (page == Page::History && self.tour.at(Step::Result)),
                |row| {
                    row.focusable()
                        .tab_stop(true)
                        .focus(|row| row.border_1().border_color(rgb(ACCENT)))
                },
            )
            .text_color(rgb(color))
            .when(selected, |row| {
                row.bg(rgb(SELECTION)).font_weight(FontWeight::SEMIBOLD)
            })
            .child(icon.render(color))
            .child(label)
            .when(page == Page::History, |row| {
                row.children(self.tour.mark(Anchor::HistoryLink))
            })
            .cursor_pointer()
            .hover(|row| row.bg(rgb(if selected { 0xd3e5d9 } else { SIDEBAR })))
            .on_click(cx.listener(move |view, _, _, cx| view.open_page(page, cx)))
    }

    fn settings_control(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.page == Page::Settings;
        icon_button(Icon::Settings, "设置")
            .size(px(36.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(if selected { SELECTION } else { SURFACE }))
            .when(self.studio_controls_active(), |button| {
                button
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.border_color(rgb(ACCENT)))
            })
            .on_click(cx.listener(|view, _, _, cx| view.open_page(Page::Settings, cx)))
    }

    fn copy_recent_text(&mut self, cx: &mut Context<Self>) {
        if let Some((_, text)) = &self.result {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            if self.tour.at(Step::Result) {
                self.tour.copied = true;
                cx.notify();
            }
        }
    }

    fn home(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let result_text = self.result.as_ref().map_or("", |(_, text)| text.as_str());
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
        let preview = self.recording.as_ref().and_then(Recording::caption);
        let show_preview = self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.preview_enabled);
        let saved_recording = self
            .saved_recording
            .as_ref()
            .filter(|_| !recording && !self.transcribing && self.unsaved.is_none());

        let can_record =
            !self.transcribing && self.unsaved.is_none() && !self.recognition_model_busy();
        let (record_label, record_icon) = if recording {
            ("停止并保存", Icon::Stop)
        } else {
            ("开始录音", Icon::Microphone)
        };
        let (record_state, state_color) = if recording {
            ("录音中", ERROR)
        } else if self.transcribing {
            ("正在转写", ACCENT)
        } else if self.unsaved.is_some() {
            ("等待保存", ERROR)
        } else if !can_record {
            ("模型处理中", ACCENT)
        } else {
            ("麦克风待命", ACCENT)
        };
        let controls = card()
            .p_4()
            .flex()
            .flex_col()
            .child(
                home_command("在 Speaky 里录音", record_state, state_color)
                    .relative()
                    .children(self.tour.mark(Anchor::Record))
                    .child(
                        action(true)
                            .id("record-action")
                            .relative()
                            .h(px(39.))
                            .px_3()
                            .gap_2()
                            .text_size(px(13.))
                            .child(record_icon.render(ACCENT_INK))
                            .child(record_label)
                            .children(self.tour.mark(Anchor::RecordButton))
                            .when(
                                can_record
                                    && (self.studio_controls_active()
                                        || self.tour.at(Step::Record)),
                                |button| {
                                    button
                                        .focusable()
                                        .tab_stop(true)
                                        .focus(|style| style.border_2().border_color(rgb(INK)))
                                },
                            )
                            .when(!can_record, |button| button.opacity(0.5).cursor_default())
                            .when(can_record, |button| {
                                button.on_click(cx.listener(|view, _, _, cx| {
                                    view.toggle_recording(RecordingStart::Button, cx)
                                }))
                            }),
                    ),
            )
            .child(
                home_command(
                    "在其他应用中听写",
                    "点目标输入框，按快捷键开始，再按一次结束。",
                    MUTED,
                )
                .border_t_1()
                .border_color(rgb(BORDER))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when_some(
                            self.hotkey_status.strip_prefix("全局快捷键 "),
                            |row, shortcut| row.child(studio_ui::keycap(shortcut)),
                        )
                        .child(button("更改", false).id("home-edit-shortcut").on_click(
                            cx.listener(|view, _, window, cx| {
                                view.open_shortcut_settings(window, cx)
                            }),
                        )),
                ),
            );
        let controls = controls.child(
            div()
                .relative()
                .flex()
                .flex_col()
                .child(
                    home_command("识别模型", "使用本地模型或外部服务。", MUTED)
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .child(self.model_picker_button(cx)),
                )
                .children(self.model_picker_panel(cx))
                .children(self.tour.mark(Anchor::Model)),
        );

        let workspace = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_4()
            .child(controls);
        #[cfg(windows)]
        let workspace = workspace.when_some(
            self.recording
                .as_ref()
                .and_then(|recording| recording.delivery_issue.as_ref()),
            |column, issue| column.child(div().text_color(rgb(ERROR)).child(issue.clone())),
        );
        let workspace = workspace
            .child(
                card()
                    .relative()
                    .min_h(px(116.))
                    .children(self.tour.mark(Anchor::Transcript))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if recording && !show_preview {
                                "录音中"
                            } else if self.transcribing {
                                "转写中…"
                            } else if show_preview {
                                "实时转写"
                            } else if saved_recording.is_some()
                                && !self.finalizing_preview.is_empty()
                            {
                                "录音已保存 · 临时转写"
                            } else if saved_recording.is_some() {
                                "录音已保存"
                            } else {
                                "最近转写"
                            })
                            .when_some(
                                saved_recording.filter(|_| !self.recognition_model_busy()),
                                |row, draft| {
                                    let draft = draft.clone();
                                    row.child(
                                        button("转写", false).id("transcribe-saved").on_click(
                                            cx.listener(move |view, _, _, cx| {
                                                view.transcribe(
                                                    draft.clone(),
                                                    RecordingDestination::Archive,
                                                    cx,
                                                )
                                            }),
                                        ),
                                    )
                                },
                            )
                            .when(
                                !recording
                                    && !self.transcribing
                                    && self.saved_recording.is_none()
                                    && self.result.is_some(),
                                |row| {
                                    let mut buttons = div().flex().items_center().gap_2();
                                    if let Some((target, text)) = &self.result {
                                        buttons = buttons.children(self.correction_button(
                                            "edit-recent",
                                            target.clone(),
                                            text,
                                            cx,
                                        ));
                                    }
                                    row.child(buttons.child(
                                        icon_button(Icon::Copy, "复制文本").on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|view, _, _, cx| view.copy_recent_text(cx)),
                                        ),
                                    ))
                                },
                            ),
                    )
                    .child(
                        div()
                            .id("transcript")
                            .mt_2()
                            .text_size(px(15.))
                            .line_height(px(25.))
                            .max_h(px(174.))
                            .overflow_y_scroll()
                            .track_scroll(&self.transcript_scroll)
                            .when(
                                if recording || self.transcribing {
                                    self.recording
                                        .as_ref()
                                        .is_some_and(|r| r.preview.text.is_empty())
                                } else {
                                    result_text.is_empty()
                                },
                                |text| text.text_color(rgb(MUTED)),
                            )
                            .child(if recording {
                                preview.unwrap_or_default().to_owned()
                            } else if self.transcribing || saved_recording.is_some() {
                                self.finalizing_preview.clone()
                            } else if result_text.is_empty() {
                                "转写结果会显示在这里。".into()
                            } else {
                                result_text.to_owned()
                            }),
                    ),
            )
            .child(self.home_links(cx));
        let summary = div()
            .flex()
            .flex_col()
            .gap_3()
            .when(wide, |column| column.w(px(204.)).flex_none())
            .child(
                card()
                    .flex()
                    .flex_col()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("录音概览"))
                    .child(metric("转写字符", characters.to_string()))
                    .child(metric("录音条数", self.archive.len().to_string()))
                    .child(metric(
                        "录音总时长",
                        format!("{}:{:02}", seconds / 60, seconds % 60),
                    )),
            )
            .child(self.activity());
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_1()
                    .child(page_heading(
                        "听写工作台",
                        "录下语音、查看转写，也能把文字写入其他应用。",
                    ))
                    .children(self.tour.mark(Anchor::Workspace))
                    .when(wide, |header| {
                        header.child(
                            div()
                                .w(px(174.))
                                .flex_none()
                                .child(studio_ui::signal(40., 0.81)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap_4()
                    .when(!wide, |layout| layout.flex_col())
                    .child(workspace)
                    .child(summary),
            )
    }

    fn home_links(&self, cx: &mut Context<Self>) -> Div {
        let links = [
            (
                Page::History,
                Icon::History,
                "录音历史",
                "回听录音，整理和复制文稿。",
            ),
            (
                Page::Terms,
                Icon::Terms,
                "个人词库",
                "补充人名、项目名和常用写法。",
            ),
            (
                Page::Settings,
                Icon::Settings,
                "听写设置",
                "调整麦克风、识别服务和语音整理。",
            ),
        ];
        let mut section = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("常用入口"));
        for (index, row) in links.chunks(2).enumerate() {
            section = section.child(div().flex().gap_3().children(row.iter().enumerate().map(
                |(offset, &(page, icon, title, detail))| {
                    card()
                        .id(("home-link", index * 2 + offset))
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .cursor_pointer()
                        .hover(|tile| tile.bg(rgb(SIDEBAR)).border_color(rgb(ACCENT)))
                        .when(self.studio_controls_active(), |tile| {
                            tile.focusable()
                                .tab_stop(true)
                                .focus(|tile| tile.border_color(rgb(ACCENT)))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(icon.render(ACCENT))
                                .child(title),
                        )
                        .child(settings::detail(detail))
                        .on_click(cx.listener(move |view, _, _, cx| view.open_page(page, cx)))
                },
            )));
        }
        section
    }

    fn history_page(&self, cx: &mut Context<Self>) -> Div {
        let older_text = history::standalone(&self.archive);
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(page_heading("录音历史", "回听原音、编辑和复制转写。"))
                    .child(self.model_picker_button(cx)),
            )
            .children(self.model_picker_panel(cx))
            .when(self.transcribing, |body| {
                body.child(div().text_color(rgb(MUTED)).child("正在转写…"))
            })
            .when(self.archive.is_empty() && older_text.is_empty(), |body| {
                body.child(
                    card()
                        .relative()
                        .children(self.tour.mark(Anchor::History))
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .size(px(40.))
                                .flex_none()
                                .rounded(px(9.))
                                .bg(rgb(SIDEBAR))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::History.render(ACCENT)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child("暂无录音"))
                                .child(settings::detail(
                                    "录音结束后，可在这里回听原声和查看转写。",
                                )),
                        )
                        .child(button("前往听写", false).id("history-empty-home").on_click(
                            cx.listener(|view, _, _, cx| view.open_page(Page::Home, cx)),
                        )),
                )
            })
            .when(!self.archive.is_empty() || !older_text.is_empty(), |body| {
                body.child(
                    div()
                        .overflow_hidden()
                        .rounded(px(11.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(SURFACE))
                        .children(self.archive.iter().enumerate().map(|(index, entry)| {
                            let play = entry.path.clone();
                            let retry = entry.path.clone();
                            let resume = entry.path.clone();
                            let reveal = entry.path.clone();
                            let organize = entry.path.clone();
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
                                .when(index == 0, |row| {
                                    row.relative().children(self.tour.mark(Anchor::History))
                                })
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
                                        .child(text.clone().unwrap_or_else(|| label.into()))
                                        .when_some(entry.recognizer.clone(), |body, recognizer| {
                                            body.child(settings::detail(recognizer))
                                        })
                                        .when_some(text.as_deref(), |body, source| {
                                            body.child(self.organized_result(entry, source, cx))
                                        }),
                                )
                                .when_some(text.as_deref(), |row, source| {
                                    row.children(self.correction_button(
                                        ("edit-recording", index),
                                        history::Target::Draft(entry.path.clone()),
                                        source,
                                        cx,
                                    ))
                                })
                                .child(
                                    history_actions()
                                        .child(icon_button(Icon::Play, "播放录音").on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |view, _, _, cx| {
                                                if view.recording.is_none()
                                                    && view.unsaved.is_none()
                                                {
                                                    view.stop_playback();
                                                    match playback::Playback::start(&play) {
                                                        Ok(playback) => {
                                                            view.playback = Some(playback);
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
                                        .when(!self.recognition_model_busy(), |actions| {
                                            actions.child(
                                                if text.is_some() {
                                                    icon_button(Icon::Retry, "重新转写")
                                                } else {
                                                    button(pending_action, false)
                                                        .id(("transcribe-recording", index))
                                                }
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |view, _, _, cx| {
                                                        view.transcribe(
                                                            retry.clone(),
                                                            RecordingDestination::Archive,
                                                            cx,
                                                        );
                                                    }),
                                                ),
                                            )
                                        })
                                        .when(
                                            !self.recognition_model_busy()
                                                && !matches!(
                                                    entry.state,
                                                    drafts::State::Transcribed { .. }
                                                ),
                                            |row| {
                                                row.child(button("继续录音", false).on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |view, _, _, cx| {
                                                        view.begin_recording(
                                                            resume.clone(),
                                                            RecordingStart::Resume,
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
                                            let original = text.clone();
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
                                            .when(
                                                self.organizer_enabled,
                                                |row| {
                                                    row.child(
                                                        button("整理", false)
                                                            .id(("organize-transcript", index))
                                                            .on_click(cx.listener(
                                                                move |view, _, _, cx| {
                                                                    view.organize_transcript(
                                                                        organize.clone(),
                                                                        original.clone(),
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                    )
                                                },
                                            )
                                        }),
                                )
                        }))
                        .children(older_text.into_iter().enumerate().map(
                            |(index, (target, text))| {
                                history_row()
                                    .id(("previous-transcript", index))
                                    .when(index == 0 && self.archive.is_empty(), |row| {
                                        row.relative().children(self.tour.mark(Anchor::History))
                                    })
                                    .child(history_meta("过往转写".into()))
                                    .child(
                                        div().flex_1().min_w(px(0.)).pt(px(4.)).child(text.clone()),
                                    )
                                    .children(self.correction_button(
                                        ("edit-previous", index),
                                        target,
                                        &text,
                                        cx,
                                    ))
                                    .child(
                                        history_actions()
                                            .invisible()
                                            .group_hover("history-row", |style| style.visible())
                                            .child(
                                                icon_button(Icon::Copy, "复制文本").on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |_, _, _, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(text.clone()),
                                                        );
                                                    }),
                                                ),
                                            ),
                                    )
                            },
                        )),
                )
            })
    }

    #[cfg(any(target_os = "macos", windows))]
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

    fn terms_page(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let display = terms::load();
        let actions = div()
            .flex()
            .gap_2()
            .when(cfg!(any(target_os = "macos", windows)), |row| {
                row.child(button("编辑词表", true).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_view, _, _window, _cx| {
                        #[cfg(any(target_os = "macos", windows))]
                        _view.edit_terms(_window, _cx);
                    }),
                ))
            })
            .child(button("粘贴并保存", false).on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    if let Some(source) = cx.read_from_clipboard().and_then(|item| item.text()) {
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
            ));
        let manual = card()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("手动添加"))
            .child(match display {
                Ok(text) if text.is_empty() => {
                    div().text_color(rgb(MUTED)).child("暂无手动添加的词。")
                }
                Ok(text) => div().child(text),
                Err(error) => div()
                    .text_color(rgb(ERROR))
                    .child(format!("无法读取手动词表：{error}")),
            });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(page_heading(
                "个人词库",
                "管理专有词，以及从改错词中学到的写法。",
            ))
            .child(actions)
            .child(
                div()
                    .flex()
                    .gap_3()
                    .when(!wide, |panel| panel.flex_col())
                    .child(div().flex_1().min_w(px(0.)).child(manual))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(self.learned_terms_card(cx)),
                    ),
            )
    }
}

impl Render for Speaky {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_tour(window, cx);
        let viewport = window.viewport_size();
        let wide = viewport.width >= px(860.);
        let tour_docked = self.tour.docked(viewport);
        let (status, status_color) = match (self.page, self.settings_notice()) {
            _ if self.unsaved.is_some() => (self.status.as_str(), ERROR),
            (Page::Settings, Some(Ok(message))) => (message.as_str(), ACCENT),
            (Page::Settings, Some(Err(message))) => (message.as_str(), ERROR),
            _ if self.status.is_empty()
                && self.global_hotkey.is_none()
                && self.page == Page::Home =>
            {
                (self.hotkey_status.as_str(), MUTED)
            }
            _ => (self.status.as_str(), MUTED),
        };
        let (status, status_color) = self
            .model_picker
            .switching_notice()
            .map(|notice| (notice, ACCENT))
            .or_else(|| {
                if status.is_empty() && self.model_picker.preloading {
                    self.model_picker
                        .preload_notice()
                        .map(|notice| (notice, MUTED))
                } else {
                    None
                }
            })
            .unwrap_or((status, status_color));
        let show_playback = self.playback.is_some();
        div()
            .size_full()
            .relative()
            .flex()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(INK))
            .text_size(px(13.))
            .line_height(px(19.))
            .child(
                div()
                    .w(px(164.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .pb_3()
                    .border_r_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(RAIL))
                    .child(
                        div()
                            .h(px(74.))
                            .flex()
                            .items_center()
                            .px_1()
                            .gap_2()
                            .text_size(px(19.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(studio_ui::mark())
                            .child("Speaky")
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.start_window_move();
                            }),
                    )
                    .children(
                        [
                            (Page::Home, "工作台", Icon::Home),
                            (Page::History, "录音历史", Icon::History),
                            (Page::Terms, "词库", Icon::Terms),
                        ]
                        .map(|(page, label, icon)| self.nav_item(page, label, icon, cx)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .mt_1()
                            .pt_2()
                            .pr(px(14.))
                            .border_t_1()
                            .border_color(rgb(BORDER))
                            .child(self.tour_entry(cx).px_1())
                            .child(self.settings_control(cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .when(cfg!(windows), |column| {
                        #[cfg(windows)]
                        let column = column.child(window_chrome::header(window.is_maximized()));
                        column
                    })
                    .when(!cfg!(windows), |column| {
                        column.child(
                            div()
                                .h(px(17.))
                                .flex_none()
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.start_window_move()
                                }),
                        )
                    })
                    .when(
                        !status.is_empty() || self.unsaved.is_some() || show_playback,
                        |page| {
                            page.child(
                                div()
                                    .relative()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .mx_5()
                                    .mb_2()
                                    .px_3()
                                    .py_2()
                                    .rounded(px(7.))
                                    .bg(rgb(SURFACE))
                                    .text_size(px(12.))
                                    .text_color(rgb(status_color))
                                    .children(self.tour.mark(Anchor::Status))
                                    .child(div().flex_1().min_w(px(0.)).child(status.to_owned()))
                                    .when(show_playback, |row| {
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
                                                if let Some(pending) = view.unsaved.take() {
                                                    view.persist(pending, cx);
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
                            .when_some(self.tour.track_page(), |scroll, handle| {
                                scroll.track_scroll(handle)
                            })
                            .px_5()
                            .pt_2()
                            .pb_5()
                            .child(match self.page {
                                Page::Home => self.home(wide, cx),
                                Page::History => self.history_page(cx),
                                Page::Terms => self.terms_page(wide, cx),
                                Page::Settings => self.settings_page(cx),
                            }),
                    )
                    .when(tour_docked, |column| {
                        column.children(self.tour_dock(viewport, cx))
                    }),
            )
            .when(self.tour.is_active(), |root| {
                root.key_context("SpeakyGuide")
                    .on_action(cx.listener(|view, _: &DismissGuide, window, cx| {
                        view.finish_tour(window, cx)
                    }))
                    .children(self.tour_overlay(tour_docked, cx))
            })
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1030.), px(690.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(665.), px(435.))),
                    window_background: if cfg!(windows) {
                        WindowBackgroundAppearance::Opaque
                    } else {
                        WindowBackgroundAppearance::Blurred
                    },
                    titlebar: Some(TitlebarOptions {
                        title: Some("Speaky".into()),
                        appears_transparent: cfg!(any(windows, target_os = "macos")),
                        traffic_light_position: cfg!(target_os = "macos")
                            .then(|| point(px(18.), px(18.))),
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    #[cfg(windows)]
                    window_chrome::hide_frame_border(window);
                    window.on_window_should_close(cx, |window, cx| {
                        window.dispatch_action(Box::new(Quit), cx);
                        false
                    });
                    cx.new(|cx| {
                        let startup_config = config::Config::load();
                        let shortcut = startup_config
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
                                    view.poll_preview(cx);
                                    if let (Some(recording), Some(handle)) =
                                        (&view.recording, view.overlay)
                                    {
                                        let activity = recording.recorder.voice_activity();
                                        let _ = handle.update(cx, |panel, _, cx| {
                                            panel.push_activity(activity, recording.caption(), cx)
                                        });
                                    }
                                    #[cfg(windows)]
                                    if view.recording.is_some() && view.floating.is_some() {
                                        view.sync_floating(cx);
                                    }
                                    if let Some(playback) = &mut view.playback {
                                        match playback.is_finished() {
                                            Ok(false) => {}
                                            result => {
                                                if let Err(error) = result {
                                                    view.status = format!("播放失败：{error}");
                                                }
                                                view.stop_playback();
                                                cx.notify();
                                            }
                                        }
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        })
                        .detach();
                        let mut view = Speaky::new(hotkey, hotkey_status, cx);
                        view.show_first_tour(window, cx);
                        if let Ok(config) = startup_config {
                            view.preload_selected_model(config, cx);
                        }
                        view
                    })
                },
            )
            .expect("could not open Speaky");
        cx.on_app_quit(move |cx| {
            let _ = window.update(cx, |view, _, cx| {
                view.save_before_exit(cx);
                if let Some(pending) = view.unsaved.take() {
                    view.persist(pending, cx);
                }
                view.stop_playback();
                #[cfg(windows)]
                if let Some(handle) = view.floating.take() {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }
            });
            async {
                if let Err(error) = model_packs::shutdown() {
                    log::warn!("释放本地模型失败：{error}");
                }
            }
        })
        .detach();
        cx.on_action(move |_: &OpenSettings, cx| {
            let _ = window.update(cx, |view, _, cx| view.open_page(Page::Settings, cx));
        });
        cx.on_action(move |_: &FocusNext, cx| {
            let _ = window.update(cx, |_, window, _| window.focus_next());
        });
        cx.on_action(move |_: &FocusPrevious, cx| {
            let _ = window.update(cx, |_, window, _| window.focus_prev());
        });
        cx.on_action(move |_: &Quit, cx| {
            let ready = window.update(cx, |view, _, cx| {
                view.save_before_exit(cx);
                if view.unsaved.is_some() {
                    return false;
                }
                if view.transcribing || view.correction.busy {
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
            KeyBinding::new(if cfg!(windows) { "ctrl-q" } else { "cmd-q" }, Quit, None),
            KeyBinding::new(
                if cfg!(windows) { "ctrl-," } else { "cmd-," },
                OpenSettings,
                None,
            ),
            KeyBinding::new("tab", FocusNext, None),
            KeyBinding::new("shift-tab", FocusPrevious, None),
            KeyBinding::new("escape", DismissGuide, Some("SpeakyGuide")),
        ]);
        cx.set_menus(vec![Menu {
            name: "Speaky".into(),
            items: vec![
                MenuItem::action("设置…", OpenSettings),
                MenuItem::action("退出 Speaky", Quit),
            ],
        }]);
        cx.activate(true);
        #[cfg(windows)]
        let _ = window.update(cx, |view, _, cx| {
            cx.observe_self(|view, cx| view.sync_floating(cx)).detach();
            let enabled = config::Config::load()
                .unwrap_or_default()
                .desktop
                .windows_floating_button;
            if let Err(error) = view.set_floating_button_enabled(enabled, cx) {
                view.status = format!("桌面悬浮按钮未能打开：{error}");
                cx.notify();
            }
        });
    });
}

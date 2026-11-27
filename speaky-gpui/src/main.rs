#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod benchmark;
#[cfg(windows)]
mod benchmark_ui;
#[cfg(target_os = "macos")]
mod clipboard_notice;
mod correction_ui;
mod diagnostics;
#[cfg(windows)]
mod floating_control;
mod header_wave;
#[cfg(windows)]
mod hotkey_capture;
mod icons;
#[cfg(target_os = "macos")]
mod mac_dictation;
mod model_picker;
mod onboarding;
mod overlay;
mod polish_ui;
mod redesign_config;
mod redesign_files;
mod redesign_home;
mod redesign_library;
#[cfg(any(target_os = "macos", windows))]
mod service_ui;
mod settings;
mod studio_ui;
#[cfg(any(target_os = "macos", windows))]
mod terms_ui;
mod theme;
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
use onboarding::{Anchor, PracticeIssue, Step};
use speaky_core::{
    audio, config, drafts, engines, history, hotkey, model_packs, pipeline, playback, polish,
    polish_benchmark,
    session::{is_silent_recording, LivePreview, SILENT_INPUT},
    terms,
};
#[cfg(windows)]
use speaky_core::{dictation_target, local_models};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use theme::{
    accent as ACCENT, accent_ink as ACCENT_INK, background as BACKGROUND, border as BORDER,
    error as ERROR, ink as INK, muted as MUTED, rail as RAIL, selection as SELECTION,
    sidebar as SIDEBAR, surface as SURFACE,
};
#[cfg(target_os = "macos")]
const CLIPBOARD_FALLBACK_MESSAGE: &str = "语音识别内容已保存到剪贴板，请手动粘贴";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Models,
    Files,
    History,
    Terms,
    Settings,
    #[cfg(windows)]
    Benchmark,
}

gpui::actions!(
    speaky,
    [OpenSettings, Quit, FocusNext, FocusPrevious, DismissGuide]
);

#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../Info.plist");

struct Recording {
    recorder: speaky_core::session::DurableRecorder,
    draft: PathBuf,
    destination: RecordingDestination,
    preview_enabled: bool,
    preview_pending: Option<uuid::Uuid>,
    preview: LivePreview,
    generation: uuid::Uuid,
    escape_down: bool,
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
    External(WindowsDestination),
    #[cfg(target_os = "macos")]
    MacExternal,
}

#[cfg(windows)]
#[derive(Clone)]
struct WindowsDestination {
    target: Result<dictation_target::DictationTarget, String>,
    wechat: bool,
}

#[cfg(windows)]
impl WindowsDestination {
    fn capture_start() -> Self {
        let focus = dictation_target::StopFocus::capture();
        let target = match dictation_target::DictationTarget::capture_foreground() {
            dictation_target::Foreground::External(Some(target)) => Ok(target),
            _ => Err("开始时未找到输入框。".into()),
        };
        let wechat = focus.is_wechat() || target.as_ref().is_ok_and(|target| target.is_wechat());
        Self { target, wechat }
    }

    fn capture_stop(&self) -> Self {
        let focus = dictation_target::StopFocus::capture();
        let wechat = self.wechat || focus.is_wechat();
        let target = if self.wechat && self.target.is_err() {
            self.target.clone()
        } else {
            dictation_target::DictationTarget::capture_stopped(focus, self.target.as_ref().ok())
        };
        Self { target, wechat }
    }
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
    starting: bool,
    stop_after_start: Option<bool>,
    #[cfg(windows)]
    startup_target: Option<WindowsDestination>,
    #[cfg(target_os = "macos")]
    mac_delivery: Option<Result<Option<mac_dictation::Target>, String>>,
    #[cfg(target_os = "macos")]
    mac_delivery_generation: uuid::Uuid,
    global_hotkey: Option<hotkey::GlobalHotkey>,
    hotkey_status: String,
    #[cfg(target_os = "macos")]
    mac_dictation_access: bool,
    feedback: Option<(Page, String)>,
    transcript_feedback: Option<(PathBuf, String)>,
    result: Option<(history::Target, String)>,
    correction: correction_ui::CorrectionEditor,
    transcribing: bool,
    transcript_scroll: ScrollHandle,
    header_wave: gpui::Entity<header_wave::HeaderWave>,
    page: Page,
    tour: onboarding::Tour,
    model_picker: model_picker::ModelPicker,
    #[cfg(windows)]
    benchmark: benchmark_ui::BenchmarkPage,
    archive: Vec<drafts::Entry>,
    saved_recording: Option<PathBuf>,
    unsaved: Option<(PendingSave, String)>,
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
    organizer: polish_ui::Organizer,
    diagnostics: diagnostics::Diagnostics,
    redesign_home: redesign_home::RedesignHome,
    redesign_config: redesign_config::RedesignConfig,
    redesign_files: redesign_files::RedesignFiles,
    redesign_library: redesign_library::RedesignLibrary,
}

impl Speaky {
    fn studio_controls_active(&self) -> bool {
        !self.tour.at(Step::Welcome)
    }

    fn set_feedback(&mut self, page: Page, message: impl Into<String>) {
        let message = message.into();
        if message.is_empty() {
            self.clear_feedback(page);
        } else {
            self.feedback = Some((page, message));
        }
    }

    fn feedback_for(&self, page: Page) -> Option<&str> {
        self.feedback
            .as_ref()
            .filter(|(source, _)| *source == page)
            .map(|(_, message)| message.as_str())
    }

    fn clear_feedback(&mut self, page: Page) {
        self.feedback.take_if(|(source, _)| *source == page);
    }

    fn transcript_feedback_for(&self, path: &std::path::Path) -> Option<&str> {
        self.transcript_feedback
            .as_ref()
            .filter(|(draft, _)| draft == path)
            .map(|(_, message)| message.as_str())
    }

    fn set_transcript_feedback(&mut self, draft: PathBuf, message: String) {
        self.transcript_feedback = (!message.is_empty()).then_some((draft, message));
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
            starting: false,
            stop_after_start: None,
            #[cfg(windows)]
            startup_target: None,
            #[cfg(target_os = "macos")]
            mac_delivery: None,
            #[cfg(target_os = "macos")]
            mac_delivery_generation: uuid::Uuid::new_v4(),
            global_hotkey: hotkey,
            hotkey_status,
            #[cfg(target_os = "macos")]
            mac_dictation_access: mac_dictation::has_access(),
            feedback: None,
            transcript_feedback: None,
            result: history::latest(&archive),
            correction: correction_ui::CorrectionEditor::default(),
            transcribing: false,
            transcript_scroll: ScrollHandle::new(),
            header_wave: cx.new(|_| header_wave::HeaderWave::default()),
            page: Page::Home,
            tour: onboarding::Tour::load(cx),
            model_picker,
            #[cfg(windows)]
            benchmark: benchmark_ui::BenchmarkPage::default(),
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
            organizer: polish_ui::Organizer::default(),
            diagnostics: diagnostics::Diagnostics::default(),
            redesign_home: Default::default(),
            redesign_config: Default::default(),
            redesign_files: Default::default(),
            redesign_library: Default::default(),
        }
    }

    fn toggle_recording(&mut self, source: RecordingStart, cx: &mut Context<Self>) {
        if self.recording.is_some() || self.starting {
            self.stop_recording(cx);
        } else {
            self.begin_recording(drafts::new_path(), source, cx);
        }
    }

    fn recognition_model_busy(&self) -> bool {
        if self.starting {
            return true;
        }
        if self.correction.busy {
            return true;
        }
        #[cfg(windows)]
        if self.benchmark.is_running() {
            return true;
        }
        self.local_model_switching()
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.finish_recording(false, cx);
    }

    fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        self.finish_recording(true, cx);
    }

    fn finish_recording(&mut self, cancelled: bool, cx: &mut Context<Self>) {
        if self.starting {
            #[cfg(windows)]
            {
                if !cancelled && self.stop_after_start != Some(false) {
                    self.startup_target = self
                        .startup_target
                        .as_ref()
                        .map(WindowsDestination::capture_stop);
                }
            }
            self.stop_after_start = Some(cancelled);
            #[cfg(target_os = "macos")]
            {
                self.mac_delivery = Some(mac_dictation::Target::capture());
            }
            self.set_feedback(Page::Home, "麦克风准备后将停止并保存。");
            self.close_overlay(cx);
            cx.notify();
            return;
        }
        let Some(mut recording) = self.recording.take() else {
            return;
        };
        #[cfg(windows)]
        if let RecordingDestination::External(previous) = &mut recording.destination {
            if cancelled {
                self.startup_target = None;
                if let Ok(target) = previous.target.clone() {
                    let task = cx.background_spawn(async move { target.cancel_preview() });
                    cx.spawn(async move |entity, cx| {
                        if let Err(error) = task.await {
                            let _ = entity.update(cx, |view, cx| {
                                view.set_feedback(Page::Home, format!("录音已保留。{error}"));
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                }
            } else {
                *previous = self
                    .startup_target
                    .take()
                    .unwrap_or_else(|| previous.capture_stop());
            }
        }
        #[cfg(target_os = "macos")]
        if !cancelled
            && self.mac_delivery.is_none()
            && matches!(recording.destination, RecordingDestination::MacExternal)
        {
            self.mac_delivery = Some(mac_dictation::Target::capture());
        }
        if cancelled {
            recording.destination = RecordingDestination::Archive;
            self.finalizing_preview.clear();
        }
        self.close_overlay(cx);
        let audio = recording.recorder.stop();
        if audio.is_empty() {
            self.set_feedback(Page::Home, "没有收到音频，请检查麦克风权限。");
            if !cancelled && self.tour.is_practice_draft(&recording.draft) {
                self.tour.issue = Some(PracticeIssue::Microphone);
                self.tour.request_reveal();
            }
            #[cfg(windows)]
            {
                self.floating_retry = true;
            }
        } else {
            let transcribe_after_save = !cancelled
                && (recording.preview_enabled
                    || !matches!(recording.destination, RecordingDestination::Archive));
            if recording.preview_enabled && !cancelled {
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
                if silent {
                    self.set_feedback(Page::Home, format!("录音已保存。{SILENT_INPUT}"));
                } else {
                    self.clear_feedback(Page::Home);
                }
                if !silent && pending.transcribe_after_save {
                    self.transcribe(pending.draft, pending.destination, cx);
                } else if silent
                    && pending.transcribe_after_save
                    && self.tour.is_practice_draft(&pending.draft)
                {
                    self.tour.issue = Some(PracticeIssue::Microphone);
                    self.tour.request_reveal();
                }
            }
            Err(error) => {
                self.unsaved = Some((pending, error.to_string()));
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
        #[cfg(target_os = "macos")]
        {
            self.mac_delivery_generation = uuid::Uuid::new_v4();
        }
        self.model_picker.close();
        self.model_picker.unload_feedback = None;
        // Capture the editor for live previews while the microphone opens.
        #[cfg(windows)]
        let destination = if source == RecordingStart::Desktop {
            RecordingDestination::External(WindowsDestination::capture_start())
        } else {
            RecordingDestination::Archive
        };
        #[cfg(target_os = "macos")]
        let destination = if source == RecordingStart::Desktop {
            RecordingDestination::MacExternal
        } else {
            RecordingDestination::Archive
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let destination = RecordingDestination::Archive;
        self.starting = true;
        self.stop_after_start = None;
        #[cfg(windows)]
        {
            self.startup_target = match &destination {
                RecordingDestination::External(target) => Some(target.clone()),
                _ => None,
            };
        }
        #[cfg(target_os = "macos")]
        {
            self.mac_delivery = None;
        }
        self.clear_feedback(Page::Home);
        #[cfg(windows)]
        if source == RecordingStart::Desktop
            && self.floating.is_none()
            && !self.tour.is_practice_draft(&draft)
        {
            self.show_recording_overlay(cx);
        }
        cx.notify();
        let opening_draft = draft.clone();
        let task = cx.background_spawn(async move {
            let config = config::Config::load().map_err(|error| error.to_string())?;
            speaky_core::session::open_recording(
                &config,
                &opening_draft,
                source == RecordingStart::Resume,
            )
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, move |view, cx| {
                view.starting = false;
                match result {
                    Ok((recorder, preview)) => {
                        let practice = view.tour.is_practice_draft(&draft);
                        if practice {
                            view.tour.issue = None;
                            view.tour.request_reveal();
                        }
                        view.stop_playback();
                        view.finalizing_preview.clear();
                        view.transcript_scroll.set_offset(point(px(0.), px(0.)));
                        view.recording = Some(Recording {
                            recorder,
                            draft,
                            destination,
                            preview_enabled: true,
                            preview_pending: None,
                            preview,
                            generation: uuid::Uuid::new_v4(),
                            escape_down: hotkey::escape_pressed(),
                            #[cfg(windows)]
                            delivery_issue: None,
                        });
                        view.clear_feedback(Page::Home);
                        #[cfg(windows)]
                        {
                            view.floating_retry = false;
                        }
                        if let Some(cancelled) = view.stop_after_start.take() {
                            view.finish_recording(cancelled, cx);
                        } else {
                            #[cfg(windows)]
                            {
                                view.startup_target = None;
                            }
                            if !practice {
                                if let Some(handle) = view.overlay {
                                    let _ = handle.update(cx, |panel, _, cx| panel.recording_started(cx));
                                } else {
                                    view.show_recording_overlay(cx);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        view.close_overlay(cx);
                        let page = if source == RecordingStart::Resume {
                            Page::History
                        } else {
                            Page::Home
                        };
                        view.set_feedback(page, error);
                        view.stop_after_start = None;
                        if view.tour.is_practice_draft(&draft) {
                            view.tour.issue = Some(PracticeIssue::Microphone);
                            view.tour.request_reveal();
                        }
                        #[cfg(windows)]
                        {
                            view.startup_target = None;
                            view.floating_retry = true;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_recording_overlay(&mut self, cx: &mut Context<Self>) {
        let show_caption = match &self.recording {
            Some(recording) => recording.preview_enabled,
            None if self.starting => true,
            None => return,
        };
        #[cfg(windows)]
        let show_controls = self.floating.is_none();
        #[cfg(not(windows))]
        let show_controls = true;
        if self.overlay.is_some() || (!show_controls && !show_caption) {
            return;
        }
        let finish = cx.weak_entity();
        let cancel = cx.weak_entity();
        match overlay::RecordingOverlay::open(
            cx,
            show_controls,
            show_caption,
            self.starting,
            move |cx| {
                let _ = finish.update(cx, Speaky::stop_recording);
            },
            move |cx| {
                let _ = cancel.update(cx, Speaky::cancel_recording);
            },
        ) {
            Ok(overlay) => self.overlay = Some(overlay),
            Err(error) => self.set_feedback(Page::Home, format!("录音浮条未能打开：{error}")),
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
            let cancel = cx.weak_entity();
            let handle = floating_control::FloatingControl::open(
                cx,
                move |cx| {
                    let _ = app.update(cx, |view, cx| {
                        if !view.handle_tour_floating_click(cx) {
                            view.toggle_recording(RecordingStart::Desktop, cx);
                        }
                    });
                },
                move |cx| {
                    let _ = cancel.update(cx, Speaky::cancel_recording);
                },
            )?;
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
            let state = if self.starting {
                State::Busy("正在打开麦克风…")
            } else if self.tour.preparing {
                State::Busy("正在准备录音…")
            } else if self.transcribing {
                State::Transcribing
            } else if self.unsaved.is_some() {
                State::Busy("待保存录音")
            } else if self.correction.busy {
                State::Busy("保存修改中")
            } else if self.benchmark.is_running() {
                State::Busy("模型评测中")
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
        if self.starting {
            self.stop_after_start = Some(true);
            self.set_feedback(self.page, "麦克风准备后将停止并保存。");
            cx.notify();
            return;
        }
        if let Some(recording) = &mut self.recording {
            #[cfg(windows)]
            if let RecordingDestination::External(WindowsDestination {
                target: Ok(target), ..
            }) = &recording.destination
            {
                target.stop_preview();
            }
            recording.destination = RecordingDestination::Archive;
            recording.preview_enabled = false;
        }
        if let Some((pending, _)) = &mut self.unsaved {
            pending.destination = RecordingDestination::Archive;
            pending.transcribe_after_save = false;
        }
        self.stop_recording(cx);
    }

    fn stop_playback(&mut self) {
        self.playback = None;
        #[cfg(windows)]
        {
            self.benchmark.playing = false;
        }
    }

    fn checkpoint(&mut self, cx: &mut Context<Self>) {
        if let Some(recording) = &mut self.recording {
            if let Err(error) = drafts::save(&recording.draft, &recording.recorder.wav_snapshot()) {
                self.set_feedback(Page::Home, format!("保存录音草稿失败：{error}"));
                cx.notify();
            }
        }
    }

    fn poll_preview(&mut self, cx: &mut Context<Self>) {
        if self.starting && hotkey::escape_pressed() {
            self.cancel_recording(cx);
            return;
        }
        if let Some(recording) = &mut self.recording {
            let down = hotkey::escape_pressed();
            let cancel = down && !recording.escape_down;
            recording.escape_down = down;
            if cancel {
                self.cancel_recording(cx);
                return;
            }
        }
        let Some(recording) = &mut self.recording else {
            return;
        };
        #[cfg(windows)]
        {
            let issue = match &recording.destination {
                RecordingDestination::Archive => None,
                RecordingDestination::External(destination) => match &destination.target {
                    Ok(target) => target.delivery_issue(),
                    Err(error) if destination.wechat => Some(error.clone()),
                    Err(_) => Some("开始时未找到输入框。结束后会尝试写入当前输入框。".into()),
                },
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
            if recording.preview.error.as_deref() != Some(SILENT_INPUT)
                && recording.preview.text.is_empty()
                && recording.recorder.get_audio_level() == 0.0
            {
                recording.preview.error = Some(SILENT_INPUT.into());
                cx.notify();
            }
            return;
        }
        if recording.preview.error.as_deref() == Some(SILENT_INPUT) {
            recording.preview.error = None;
            cx.notify();
        }
        let draft = recording.draft.clone();
        let generation = recording.generation;
        recording.preview_pending = Some(generation);
        self.preview(draft, generation, chunk, cx);
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
            let Some(engine) = engines::preview_recognition_engine(&config)? else {
                return Ok(String::new());
            };
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
                        if let RecordingDestination::External(WindowsDestination {
                            target: Ok(target),
                            ..
                        }) = &recording.destination
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
        #[cfg(target_os = "macos")]
        let mac_delivery_generation = {
            self.mac_delivery_generation = uuid::Uuid::new_v4();
            self.mac_delivery_generation
        };
        self.model_picker.close();
        self.transcribing = true;
        self.stop_playback();
        if self.saved_recording.as_ref() != Some(&draft) {
            self.finalizing_preview.clear();
        }
        let completed_draft = draft.clone();
        if self.tour.is_practice_draft(&draft) {
            self.tour.issue = None;
        }
        self.clear_feedback(Page::Home);
        self.transcript_feedback.take_if(|(path, _)| path == &draft);
        #[cfg(target_os = "macos")]
        let mac_external = matches!(destination, RecordingDestination::MacExternal);
        let task = cx.background_spawn(async move {
            let config = config::Config::load().map_err(|error| error.to_string())?;
            #[cfg(windows)]
            if let RecordingDestination::External(destination) = destination {
                let original = pipeline::transcribe_original(&config, &draft)?;
                let delivery = destination.target.and_then(|target| {
                    target.deliver(&original)?;
                    Ok(target)
                });
                let transcript = pipeline::rewrite_saved(&config, &draft, original.clone());
                let delivery_notice = match delivery {
                    Ok(target) if transcript.text != original => {
                        target.revise(&transcript.text).err().map(|error| {
                            log::debug!(
                                "Could not update the external field with the rewrite: {error}"
                            );
                            "润色稿未写入，已保存。"
                        })
                    }
                    Ok(_) => None,
                    Err(error) => {
                        log::debug!(
                            "Could not write the transcript to the external field: {error}"
                        );
                        Some("未写入输入框，转写已保存。")
                    }
                };
                let status = delivery_notice.map_or(transcript.notice, str::to_owned);
                return Ok::<_, String>((transcript.text, status, false));
            }
            let transcript = pipeline::transcribe_saved(&config, &draft)?;
            let rewrite_completed = config.llm.enabled && transcript.notice.is_empty();
            Ok::<_, String>((transcript.text, transcript.notice, rewrite_completed))
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.transcribing = false;
                view.archive = drafts::list();
                match result {
                    Ok((text, status, rewrite_completed)) => {
                        #[cfg(target_os = "macos")]
                        let mut status = status;
                        view.finalizing_preview.clear();
                        if view.saved_recording.as_ref() == Some(&completed_draft) {
                            view.saved_recording = None;
                        }
                        view.transcript_scroll.set_offset(point(px(0.), px(0.)));
                        #[cfg(target_os = "macos")]
                        if mac_external {
                            let needs_clipboard = match view
                                .mac_delivery
                                .take()
                                .unwrap_or_else(|| Err("未找到停止时的输入框。".into()))
                            {
                                Ok(Some(target)) => {
                                    match target.insert(&text, mac_delivery_generation, cx) {
                                        Ok(notice) => {
                                            status.push_str(notice);
                                            false
                                        }
                                        Err(error) => {
                                            status.push_str(&error);
                                            true
                                        }
                                    }
                                }
                                Ok(None) => true,
                                Err(error) => {
                                    status.push_str(&error);
                                    true
                                }
                            };
                            if needs_clipboard {
                                status.push_str(&view.clipboard_fallback(&text, cx));
                            }
                        }
                        let practice = view.tour.is_practice_draft(&completed_draft);
                        if practice {
                            view.show_practice_result(
                                &completed_draft,
                                &text,
                                rewrite_completed,
                                cx,
                            );
                        }
                        view.set_transcript_feedback(completed_draft.clone(), status);
                        view.result = Some((history::Target::Draft(completed_draft), text));
                    }
                    Err(error) => {
                        let no_speech = error.lines().next() == Some(pipeline::NO_RECOGNIZED_TEXT);
                        if view.tour.is_practice_draft(&completed_draft) {
                            view.finalizing_preview.clear();
                            view.tour.issue = Some(if no_speech {
                                PracticeIssue::Microphone
                            } else {
                                PracticeIssue::Transcription
                            });
                            view.tour.request_reveal();
                        }
                        view.set_transcript_feedback(completed_draft, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    fn clipboard_fallback(&self, text: &str, cx: &mut Context<Self>) -> String {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        let mut message = CLIPBOARD_FALLBACK_MESSAGE.to_owned();
        if let Err(error) = clipboard_notice::show(cx) {
            message.push_str(&format!("（{error}）"));
        }
        message
    }

    fn compare_recording(&mut self, draft: PathBuf, cx: &mut Context<Self>) {
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
        self.set_transcript_feedback(draft.clone(), "正在用所选模型测试这段录音…".into());
        let tested_draft = draft.clone();
        let task = cx.background_spawn(async move {
            let audio = std::fs::read(&draft).map_err(|error| error.to_string())?;
            if is_silent_recording(&audio) {
                return Err(SILENT_INPUT.to_owned());
            }
            let config = config::Config::load().map_err(|error| error.to_string())?;
            let engine = engines::recognition_engine(&config)?;
            let model = engine.name().to_owned();
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
            drafts::save_comparison(&draft, &model, &text)
                .map_err(|error| format!("保存模型测试结果失败：{error}"))?;
            Ok::<_, String>(())
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.transcribing = false;
                view.archive = drafts::list();
                let message = match result {
                    Ok(()) => String::new(),
                    Err(error) => format!("模型测试失败：{error}"),
                };
                view.set_transcript_feedback(tested_draft, message);
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
        .bg(rgb(if primary { ACCENT() } else { SIDEBAR() }))
        .text_color(rgb(if primary { ACCENT_INK() } else { INK() }))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|style| {
            style.bg(rgb(if primary {
                theme::choose(0xa7b8ff, 0x204b3e)
            } else {
                BORDER()
            }))
        })
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
        .child(icon.render(INK()))
        .tooltip(move |_, cx| cx.new(|_| ActionTooltip(label.into())).into())
}

fn card() -> Div {
    div()
        .p_4()
        .rounded(px(12.))
        .border_1()
        .border_color(rgb(BORDER()))
        .bg(rgb(SURFACE()))
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
        .child(div().text_color(rgb(MUTED())).child(description))
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
        let color = if selected { ACCENT() } else { MUTED() };
        div()
            .id(("sidebar-page", page as usize))
            .relative()
            .h(px(40.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(7.))
            .when(self.studio_controls_active(), |row| {
                row.focusable()
                    .tab_stop(true)
                    .focus(|row| row.border_1().border_color(rgb(ACCENT())))
            })
            .text_color(rgb(color))
            .when(selected, |row| {
                row.bg(rgb(SELECTION())).font_weight(FontWeight::SEMIBOLD)
            })
            .child(icon.render(color))
            .child(label)
            .when(page == Page::History, |row| {
                row.children(self.tour.mark(Anchor::HistoryLink))
            })
            .when(page == Page::Terms, |row| {
                row.children(self.tour.mark(Anchor::TermsLink))
            })
            .when(page == Page::Files, |row| {
                row.children(self.tour.mark(Anchor::FilesLink))
            })
            .cursor_pointer()
            .hover(|row| {
                row.bg(rgb(if selected {
                    theme::choose(0x3a486f, 0xcfe4d7)
                } else {
                    SIDEBAR()
                }))
            })
            .on_click(cx.listener(move |view, _, window, cx| {
                if view.tour.at(Step::Finish)
                    && matches!(page, Page::History | Page::Terms | Page::Files)
                {
                    view.finish_tour(window, cx);
                }
                view.open_page(page, cx);
            }))
    }

    fn settings_control(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.page == Page::Settings;
        icon_button(Icon::Settings, "设置")
            .size(px(36.))
            .w_full()
            .justify_start()
            .px_2()
            .gap_2()
            .child("设置")
            .border_1()
            .border_color(rgb(BORDER()))
            .bg(rgb(if selected { SELECTION() } else { SURFACE() }))
            .when(self.studio_controls_active(), |button| {
                button
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.border_color(rgb(ACCENT())))
            })
            .on_click(cx.listener(|view, _, _, cx| view.open_page(Page::Settings, cx)))
    }

    fn copy_practice_result(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.tour.practice_result() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            self.tour.copied = true;
            cx.notify();
        }
    }

    fn edit_terms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.correction.busy {
            return;
        }
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
                        let message = match result {
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
                        view.set_feedback(Page::Terms, message);
                        cx.notify();
                    });
                })
                .detach(),
            Err(error) => {
                self.set_feedback(Page::Terms, format!("无法打开常用词编辑器：{error}"));
                cx.notify();
            }
        }
    }

    fn page_footer(&self, cx: &mut Context<Self>) -> Option<Div> {
        let show_playback = self.playback.is_some();
        #[cfg(windows)]
        let show_playback = show_playback && !self.benchmark.playing;
        let (message, color) = if let Some((_, error)) = &self.unsaved {
            (
                Some(format!("录音尚未保存，音频仍在内存。{error}")),
                ERROR(),
            )
        } else if matches!(self.page, Page::Settings | Page::Models) {
            match self.settings_notice() {
                Some(Ok(message)) => (Some(message.clone()), MUTED()),
                Some(Err(message)) => (Some(message.clone()), ERROR()),
                None => (self.feedback_for(self.page).map(str::to_owned), MUTED()),
            }
        } else {
            (self.feedback_for(self.page).map(str::to_owned), MUTED())
        };
        if message.is_none() && !show_playback {
            return None;
        }
        let correction_action = self
            .feedback_for(self.page)
            .filter(|text| message.as_deref() == Some(*text))
            .and_then(|text| self.correction_notice_action(text, cx));
        Some(
            div()
                .relative()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .mx_5()
                .py_2()
                .border_t_1()
                .border_color(rgb(BORDER()))
                .text_size(px(12.))
                .text_color(rgb(color))
                .children(self.tour.mark(Anchor::Status))
                .child(div().flex_1().min_w(px(0.)).children(message))
                .children(correction_action)
                .when(show_playback, |row| {
                    row.child(
                        button("停止播放", false)
                            .id("stop-playback")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.stop_playback();
                                cx.notify();
                            })),
                    )
                })
                .when(self.unsaved.is_some(), |row| {
                    row.child(button("重试保存", true).id("retry-save-audio").on_click(
                        cx.listener(|view, _, _, cx| {
                            if let Some((pending, _)) = view.unsaved.take() {
                                view.persist(pending, cx);
                                cx.notify();
                            }
                        }),
                    ))
                }),
        )
    }
}

impl Render for Speaky {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tour_docked = self.prepare_tour(window);
        let viewport = window.viewport_size();
        let wide = viewport.width >= px(860.);
        let footer = self.page_footer(cx);
        div()
            .size_full()
            .relative()
            .flex()
            .bg(rgb(BACKGROUND()))
            .text_color(rgb(INK()))
            .text_size(px(13.))
            .line_height(px(19.))
            .child(
                div()
                    .w(if viewport.width >= px(970.) {
                        px(214.)
                    } else {
                        px(156.)
                    })
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .pb_3()
                    .ml_3()
                    .mt_3()
                    .mb_3()
                    .rounded(px(13.))
                    .border_1()
                    .border_color(rgb(BORDER()))
                    .bg(rgb(RAIL()))
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
                            .child(
                                div().flex().flex_col().child("Speaky").child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(MUTED()))
                                        .child(concat!("领铄 · ", env!("CARGO_PKG_VERSION"))),
                                ),
                            )
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.start_window_move();
                            }),
                    )
                    .children(
                        [
                            (Page::Home, "首页", Icon::Home),
                            (Page::Models, "模型选择", Icon::Model),
                            (Page::Files, "转录文件", Icon::Folder),
                            (Page::History, "历史记录", Icon::History),
                            (Page::Terms, "词典", Icon::Terms),
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
                            .flex_col()
                            .border_t_1()
                            .border_color(rgb(BORDER()))
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
                        let column = column.child(window_chrome::header(
                            window.is_maximized(),
                            self.theme_switch(true, cx),
                        ));
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
                    .child(
                        div()
                            .id(self.page as usize)
                            .flex_1()
                            .min_h(px(0.))
                            .min_w(px(0.))
                            .overflow_y_scroll()
                            .when(
                                self.page == Page::History && !self.tour.is_active(),
                                |scroll| scroll.track_scroll(&self.redesign_library.scroll),
                            )
                            .when_some(self.tour.track_page(), |scroll, handle| {
                                scroll.track_scroll(handle)
                            })
                            .px_5()
                            .pt_2()
                            .pb_5()
                            .child(match self.page {
                                Page::Home => self.v30_home(viewport.width >= px(970.), cx),
                                Page::Models => self.v30_models(window, cx),
                                Page::Files => self.v30_files(cx),
                                Page::History => self.v30_history(cx),
                                #[cfg(windows)]
                                Page::Benchmark => self.benchmark_page(cx),
                                Page::Terms => self.v30_terms(wide, cx),
                                Page::Settings => self.v30_settings(cx),
                            }),
                    )
                    .children(footer)
                    .when(tour_docked, |column| {
                        column.children(self.tour_dock(viewport, cx))
                    }),
            )
            .when(self.tour.is_active(), |root| {
                root.key_context("SpeakyGuide")
                    .on_action(cx.listener(|view, _: &DismissGuide, window, cx| {
                        if view.recording.is_some() || view.starting {
                            view.cancel_recording(cx);
                        } else {
                            view.finish_tour(window, cx);
                        }
                    }))
                    .children(self.tour_overlay(viewport, tour_docked, cx))
            })
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let configured_theme = config::Config::load()
            .map(|config| config.appearance.theme)
            .unwrap_or_else(|_| "auto".into());
        theme::set(theme::resolve(&configured_theme, cx.window_appearance()));
        #[cfg(windows)]
        let display_id = std::env::var_os("SPEAKY_START_ON_SECONDARY")
            .filter(|value| value == "1")
            .and_then(|_| cx.primary_display())
            .and_then(|primary| {
                cx.displays()
                    .into_iter()
                    .find(|display| display.id() != primary.id())
                    .map(|display| display.id())
            });
        #[cfg(not(windows))]
        let display_id = None;
        let bounds = Bounds::centered(display_id, size(px(1120.), px(740.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(665.), px(435.))),
                    display_id,
                    focus: display_id.is_none(),
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
                        #[cfg(windows)]
                        let release_gpu = startup_config.as_ref().ok().is_some_and(|c| {
                            !engines::uses_gemini_asr(c)
                                && (c.engine.current == "remote"
                                    || config::Framework::selected(c).is_some())
                        }) && local_models::has_owned_manager();
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
                            if entity
                                .update(cx, |view, cx| {
                                    #[cfg(target_os = "macos")]
                                    {
                                        let access = mac_dictation::has_access();
                                        if view.mac_dictation_access != access {
                                            view.mac_dictation_access = access;
                                            cx.notify();
                                        }
                                    }
                                    view.checkpoint(cx);
                                    if matches!(view.page, Page::Home | Page::Settings) {
                                        view.refresh_input_devices(cx);
                                    }
                                })
                                .is_err()
                            {
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
                                    view.v30_tick(cx);
                                    if view.diagnostics.checking {
                                        cx.notify();
                                    }
                                    if view.page == Page::Home {
                                        let level = view
                                            .recording
                                            .as_ref()
                                            .map(|recording| recording.recorder.get_audio_level());
                                        view.header_wave
                                            .update(cx, |wave, _| wave.set_level(level));
                                    }
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
                                                    view.set_feedback(
                                                        view.page,
                                                        format!("播放失败：{error}"),
                                                    );
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
                        view.refresh_input_devices(cx);
                        view.show_first_tour(window, cx);
                        #[cfg(any(target_os = "macos", windows))]
                        view.resolve_framework_name(cx);
                        #[cfg(windows)]
                        if release_gpu {
                            view.release_local_model(None, cx);
                        }
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
                if let Some((pending, _)) = view.unsaved.take() {
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
                if view.starting || view.transcribing || view.correction.busy {
                    view.set_feedback(view.page, "正在保存转写结果，完成后即可退出。");
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
        if display_id.is_none() {
            cx.activate(true);
        }
        #[cfg(target_os = "macos")]
        cx.defer(|_| mac_dictation::request_access());
        #[cfg(windows)]
        let _ = window.update(cx, |view, _, cx| {
            cx.observe_self(|view, cx| view.sync_floating(cx)).detach();
            let enabled = config::Config::load()
                .unwrap_or_default()
                .desktop
                .windows_floating_button;
            if let Err(error) = view.set_floating_button_enabled(enabled, cx) {
                view.set_feedback(Page::Settings, format!("桌面悬浮按钮未能打开：{error}"));
                cx.notify();
            }
        });
    });
}

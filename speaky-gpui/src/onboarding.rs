//! First-run setup and a practice recording in the actual Speaky interface.
mod card;
mod effects;
mod layout;
mod motion;
mod platform;

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use gpui::{
    div, prelude::*, px, rgb, Canvas, Context, Div, FocusHandle, Pixels, ScrollHandle, Size,
    Stateful, Window,
};

use crate::{
    action, config::Config, drafts, icons::Icon, Page, RecordingDestination, RecordingStart,
    Speaky, ACCENT, BORDER, MUTED, SIDEBAR,
};

pub(crate) use layout::Anchor;
use layout::{Layout, CARD_WIDTH, WELCOME_WIDTH};
use motion::{Clock, Frame, Scene};
pub(crate) use platform::motion_allowed;

// Preserve the launch preference of existing profiles.
const MARKER: &str = "onboarding-v1.done";

pub(crate) async fn microphone_permission() -> bool {
    platform::microphone_permission().await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Welcome,
    Shortcut,
    Models,
    Floating,
    Record,
    Finish,
}

impl Step {
    const ALL: [Self; 6] = [
        Self::Welcome,
        Self::Shortcut,
        Self::Models,
        Self::Floating,
        Self::Record,
        Self::Finish,
    ];

    fn page(self) -> Page {
        match self {
            Self::Shortcut | Self::Floating => Page::Settings,
            Self::Models => Page::Models,
            Self::Welcome | Self::Record | Self::Finish => Page::Home,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Welcome => "欢迎",
            Self::Shortcut => "听写快捷键",
            Self::Models => "识别与改写",
            Self::Floating => "桌面悬浮按钮",
            Self::Record => "试说一句",
            Self::Finish => "完成设置",
        }
    }
}

pub(crate) struct PracticeResult {
    original: String,
    rewrite: Option<String>,
    show_rewrite: bool,
}

impl PracticeResult {
    fn new(original: String, rewrite: Option<String>) -> Self {
        Self {
            original,
            show_rewrite: rewrite.is_some(),
            rewrite,
        }
    }

    fn text(&self) -> &str {
        if let Some(rewrite) = self.rewrite.as_deref().filter(|_| self.show_rewrite) {
            rewrite
        } else {
            &self.original
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PracticeIssue {
    Microphone,
    Model,
    Transcription,
}

pub(crate) struct Tour {
    step: Option<Step>,
    layout: Layout,
    clock: Clock,
    frame: Frame,
    focus: FocusHandle,
    replay: FocusHandle,
    pub(crate) preparing: bool,
    pub(crate) issue: Option<PracticeIssue>,
    draft: Option<PathBuf>,
    result: Option<PracticeResult>,
    pub(crate) copied: bool,
    shortcut_waiting: bool,
    shortcut_verified: bool,
    transition: u64,
    #[cfg(windows)]
    window: Option<isize>,
}

impl Tour {
    pub(crate) fn load(cx: &mut Context<Speaky>) -> Self {
        let mut layout = Layout::default();
        layout.reset();
        Self {
            step: first_step(&Config::config_dir().join(MARKER)),
            layout,
            clock: Clock::default(),
            frame: Frame::default(),
            focus: cx.focus_handle(),
            replay: cx.focus_handle(),
            preparing: false,
            issue: None,
            draft: None,
            result: None,
            copied: false,
            shortcut_waiting: false,
            shortcut_verified: false,
            transition: 0,
            #[cfg(windows)]
            window: None,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.step.is_some()
    }

    pub(crate) fn at(&self, step: Step) -> bool {
        self.step == Some(step)
    }

    pub(crate) fn is_practice_draft(&self, draft: &Path) -> bool {
        self.is_active() && self.draft.as_deref() == Some(draft)
    }

    pub(crate) fn mark(&self, anchor: Anchor) -> Option<Canvas<()>> {
        self.is_active().then(|| self.layout.mark(anchor))
    }

    pub(crate) fn track_page(&self) -> Option<&ScrollHandle> {
        self.is_active().then_some(&self.layout.scroll)
    }

    pub(crate) fn request_reveal(&self) {
        self.layout.reveal();
    }

    pub(crate) fn result_appearance(&self) -> Option<(f32, Pixels)> {
        if self.practice_result().is_none() || self.frame.scene != Scene::Transcript {
            return None;
        }
        let reveal = self.frame.content();
        (reveal < 1.).then(|| (0.45 + 0.55 * reveal, px(5. * (1. - reveal))))
    }

    pub(crate) fn practice_result(&self) -> Option<&str> {
        self.result
            .as_ref()
            .filter(|_| self.at(Step::Record))
            .map(PracticeResult::text)
    }

    pub(crate) fn has_rewrite(&self) -> bool {
        self.result
            .as_ref()
            .is_some_and(|result| result.rewrite.is_some())
    }

    pub(crate) fn showing_rewrite(&self) -> bool {
        self.result
            .as_ref()
            .is_some_and(|result| result.show_rewrite)
    }
}

fn first_step(marker: &Path) -> Option<Step> {
    (!marker.is_file()).then_some(Step::Welcome)
}

#[derive(Clone, Copy)]
enum GuideAction {
    Next,
    Back,
    Finish,
    StopTour,
    Return,
    KeepShortcut,
    CaptureShortcut,
    ModelTab(bool),
    #[cfg(windows)]
    Floating(bool),
    Privacy,
    #[cfg(target_os = "macos")]
    Accessibility,
    Devices,
    Models,
    Record,
    RetryTranscription,
    OpenFeature(Page),
}

type GuideButton = (&'static str, GuideAction);

impl Speaky {
    pub(crate) fn show_first_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            self.tour.window = match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get()),
                _ => None,
            };
        }
        if self.tour.is_active() {
            self.restart_tour(window, cx);
        }
    }

    pub(crate) fn tour_window_active(&self, _cx: &Context<Self>) -> bool {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
            self.tour.window == Some(unsafe { GetForegroundWindow() } as isize)
        }
        #[cfg(not(windows))]
        _cx.active_window()
            .and_then(|window| window.downcast::<Self>())
            .is_some()
    }

    fn restart_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if (self.tour.is_active() && !self.tour.at(Step::Welcome))
            || self.recording.is_some()
            || self.starting
        {
            return;
        }
        self.tour.layout.clear_camera();
        self.show_tour_step(Step::Welcome, cx);
        window.focus(&self.tour.focus);
    }

    pub(crate) fn finish_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tour.step.take().is_none() {
            return;
        }
        self.tour.preparing = false;
        self.tour.issue = None;
        self.tour.draft = None;
        self.tour.result = None;
        self.tour.copied = false;
        self.tour.shortcut_waiting = false;
        self.tour.shortcut_verified = false;
        self.tour.transition = self.tour.transition.wrapping_add(1);
        if let Err(error) = drafts::write_atomically(&Config::config_dir().join(MARKER), b"done\n")
        {
            self.set_feedback(Page::Home, format!("无法保存引导进度：{error}"));
        }
        self.open_page(Page::Home, cx);
        window.focus(&self.tour.replay);
    }

    pub(crate) fn show_tour_step(&mut self, step: Step, cx: &mut Context<Self>) {
        if step == Step::Shortcut && !self.tour.at(Step::Shortcut) {
            self.tour.shortcut_waiting = false;
            self.tour.shortcut_verified = false;
            self.clear_guide_shortcut_error();
        }
        if step == Step::Models && !self.tour.at(Step::Models) {
            self.guide_show_model_tab(false, cx);
        }
        self.tour.step = Some(step);
        self.tour.transition = self.tour.transition.wrapping_add(1);
        self.tour.layout.reset();
        if self.page != step.page() {
            self.open_page(step.page(), cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn tour_shortcut_changed(&mut self) {
        if self.tour.at(Step::Shortcut) {
            self.tour.shortcut_waiting = true;
            self.tour.shortcut_verified = false;
        }
    }

    pub(crate) fn handle_tour_hotkey(&mut self, cx: &mut Context<Self>) -> bool {
        if self.tour.at(Step::Record) && self.guide_recording_in_progress() {
            self.stop_recording(cx);
            return true;
        }
        if !self.tour_window_active(cx) {
            return false;
        }
        match self.tour.step {
            Some(Step::Shortcut) if self.page == Page::Settings => {
                if !self.guide_shortcut_capturing() && self.v30_registered_shortcut().is_some() {
                    self.tour.shortcut_verified = true;
                    self.tour.shortcut_waiting = false;
                    cx.notify();
                }
                true
            }
            Some(Step::Record) if self.page == Page::Home => {
                self.toggle_practice_recording(cx);
                true
            }
            Some(Step::Welcome) => true,
            _ => false,
        }
    }

    fn guide_recording_in_progress(&self) -> bool {
        self.recording
            .as_ref()
            .is_some_and(|recording| self.tour.is_practice_draft(&recording.draft))
            || (self.starting && self.tour.draft.is_some())
    }

    #[cfg(windows)]
    pub(crate) fn handle_tour_floating_click(&mut self, cx: &mut Context<Self>) -> bool {
        if self.tour.at(Step::Record) && self.guide_recording_in_progress() {
            self.stop_recording(cx);
            return true;
        }
        if self.tour.at(Step::Record) && self.page == Page::Home && self.tour_window_active(cx) {
            self.toggle_practice_recording(cx);
            return true;
        }
        false
    }

    pub(crate) fn prepare_tour(&mut self, window: &mut Window) -> bool {
        let Some(step) = self.tour.step else {
            return false;
        };
        let scene = match step {
            Step::Welcome => Scene::Welcome,
            Step::Shortcut if self.tour.shortcut_verified => Scene::ShortcutReady,
            Step::Shortcut => Scene::Shortcut,
            Step::Models if self.guide_model_ready() => Scene::ModelReady,
            Step::Models => Scene::Model,
            #[cfg(windows)]
            Step::Floating
                if self.tour_floating_status()
                    == crate::floating_control::FloatingStatus::Visible =>
            {
                Scene::FloatingReady
            }
            Step::Floating => Scene::Floating,
            Step::Record if self.page == Page::Settings => Scene::Microphone,
            Step::Record if self.page == Page::Models => Scene::Model,
            Step::Record if self.practice_result_ready() => Scene::Transcript,
            Step::Record if self.transcribing => Scene::Processing,
            Step::Record if self.recording.is_some() => Scene::Live,
            Step::Record => Scene::Record,
            Step::Finish => Scene::Finish,
        };
        if self.tour.frame.scene != scene {
            self.tour.request_reveal();
        }
        self.tour.frame = self.tour.clock.frame(
            scene,
            step as usize,
            self.tour.transition,
            Instant::now(),
            motion_allowed(),
        );
        if self.tour.frame.animating() {
            window.request_animation_frame();
        }
        step != Step::Welcome
            && (self.tour.layout.docked(window.viewport_size())
                || self.page == Page::Models
                || (self.page != step.page() && self.page != Page::Settings))
    }

    fn run_guide_action(
        &mut self,
        epoch: u64,
        action: GuideAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(step) = self.tour.step else {
            return;
        };
        if epoch != self.tour.transition {
            return;
        }
        match action {
            GuideAction::Next | GuideAction::Back => {
                let index = step as usize;
                let next = if matches!(action, GuideAction::Back) {
                    index.saturating_sub(1)
                } else {
                    index + 1
                };
                if let Some(next) = Step::ALL.get(next) {
                    self.show_tour_step(*next, cx);
                    window.focus(&self.tour.focus);
                }
            }
            GuideAction::Finish | GuideAction::StopTour => {
                if self.guide_recording_in_progress() {
                    self.stop_recording(cx);
                }
                self.finish_tour(window, cx);
            }
            GuideAction::Return => {
                self.open_page(step.page(), cx);
                window.focus(&self.tour.focus);
            }
            GuideAction::KeepShortcut => {
                self.clear_guide_shortcut_error();
                self.tour.shortcut_waiting = true;
                cx.notify();
            }
            GuideAction::CaptureShortcut => self.begin_shortcut_capture(window, cx),
            GuideAction::ModelTab(rewrite) => self.guide_show_model_tab(rewrite, cx),
            #[cfg(windows)]
            GuideAction::Floating(enabled) => self.set_tour_floating_button(enabled, cx),
            GuideAction::Privacy => cx.open_url(platform::MICROPHONE_SETTINGS),
            #[cfg(target_os = "macos")]
            GuideAction::Accessibility => crate::mac_dictation::request_access(),
            GuideAction::Devices => self.open_page(Page::Settings, cx),
            GuideAction::Models => {
                self.redesign_config.rewrite = false;
                self.open_page(Page::Models, cx);
            }
            GuideAction::Record => self.toggle_practice_recording(cx),
            GuideAction::RetryTranscription => self.retry_practice_transcription(cx),
            GuideAction::OpenFeature(page) => {
                self.finish_tour(window, cx);
                self.open_page(page, cx);
            }
        }
    }

    pub(crate) fn practice_result_ready(&self) -> bool {
        self.tour.practice_result().is_some()
    }

    pub(crate) fn show_practice_result(
        &mut self,
        path: &Path,
        fallback: &str,
        rewrite_completed: bool,
        cx: &mut Context<Self>,
    ) {
        let entry = self.archive.iter().find(|entry| entry.path == path);
        let original: &str = match entry.map(|entry| &entry.state) {
            Some(drafts::State::Transcribed { text }) => text,
            _ => fallback,
        };
        let rewrite = entry
            .and_then(|entry| entry.organized.as_ref())
            .filter(|value| rewrite_completed && value.source == original && value.text == fallback)
            .map(|value| value.text.clone());
        self.tour.result = Some(PracticeResult::new(original.to_owned(), rewrite));
        self.tour.copied = false;
        self.tour.request_reveal();
        cx.notify();
    }

    pub(crate) fn select_practice_version(&mut self, rewrite: bool, cx: &mut Context<Self>) {
        if let Some(result) = &mut self.tour.result {
            result.show_rewrite = rewrite && result.rewrite.is_some();
            self.tour.copied = false;
            cx.notify();
        }
    }

    pub(crate) fn toggle_practice_recording(&mut self, cx: &mut Context<Self>) {
        if !self.tour.at(Step::Record) || self.page != Page::Home {
            return;
        }
        if self.recording.is_some() || self.starting {
            self.stop_recording(cx);
            return;
        }
        if self.tour.preparing
            || self.transcribing
            || self.unsaved.is_some()
            || self.diagnostics.checking
            || self.recognition_model_busy()
        {
            return;
        }
        self.tour.preparing = true;
        self.tour.issue = None;
        self.tour.result = None;
        self.tour.copied = false;
        // A prior attempt's saved recording stays in history.
        self.tour.draft = None;
        self.clear_feedback(Page::Home);
        let transition = self.tour.transition;
        cx.spawn(async move |entity, cx| {
            let readiness = if microphone_permission().await {
                cx.background_spawn(async {
                    let config = Config::load().map_err(|error| error.to_string())?;
                    crate::engines::recognition_engine(&config)
                        .map(|_| crate::engines::uses_local_asr(&config))
                })
                .await
                .map_err(|error| (PracticeIssue::Model, error))
            } else {
                Err((
                    PracticeIssue::Microphone,
                    "请在系统设置中允许 Speaky 访问麦克风。".into(),
                ))
            };
            let _ = entity.update(cx, |view, cx| {
                if view.tour.transition != transition {
                    return;
                }
                view.tour.preparing = false;
                match readiness {
                    Ok(local)
                        if view.page == Page::Home
                            && view.recording.is_none()
                            && !view.starting
                            && !view.transcribing
                            && view.unsaved.is_none()
                            && !view.recognition_model_busy() =>
                    {
                        if local {
                            view.model_picker.note_model_loaded();
                        }
                        let draft = drafts::new_path();
                        view.tour.draft = Some(draft.clone());
                        view.begin_recording(draft, RecordingStart::Button, cx);
                    }
                    Ok(_) => {}
                    Err((issue, error)) => {
                        view.set_feedback(Page::Home, error);
                        view.tour.issue = Some(issue);
                        view.tour.request_reveal();
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn retry_practice_transcription(&mut self, cx: &mut Context<Self>) {
        if !self.tour.at(Step::Record) || self.tour.issue != Some(PracticeIssue::Transcription) {
            return;
        }
        let Some(draft) = self.tour.draft.clone() else {
            return;
        };
        self.transcribe(draft, RecordingDestination::Archive, cx);
    }

    pub(crate) fn tour_entry(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        action(false)
            .id("replay-tour")
            .h(px(36.))
            .w_full()
            .justify_start()
            .px_2()
            .gap_1p5()
            .bg(rgb(SIDEBAR()))
            .text_color(rgb(MUTED()))
            .child(Icon::Guide.render(ACCENT()))
            .child("进入引导")
            .when(!self.tour.is_active(), |row| {
                row.track_focus(&self.tour.replay)
                    .tab_stop(true)
                    .focus(|row| row.border_1().border_color(rgb(ACCENT())))
            })
            .on_click(cx.listener(|view, _, window, cx| view.restart_tour(window, cx)))
    }

    pub(crate) fn tour_dock(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> Option<Div> {
        let step = self.tour.step?;
        Some(
            div()
                .relative()
                .flex_none()
                .w_full()
                .px_4()
                .py_2()
                .bg(rgb(crate::RAIL()))
                .border_t_1()
                .border_color(rgb(BORDER()))
                .child(self.story_card(
                    step,
                    (viewport.width - px(270.)).clamp(px(270.), px(550.)),
                    Some((viewport.height * 0.46).max(px(160.))),
                    cx,
                ))
                .children(self.tour.mark(Anchor::Dock)),
        )
    }

    pub(crate) fn tour_overlay(
        &self,
        viewport: Size<Pixels>,
        docked: bool,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let step = self.tour.step?;
        let frame = self.tour.frame;
        let (primary, companion, extra) = match frame.scene {
            Scene::Welcome => (Anchor::Workspace, None, None),
            Scene::Shortcut | Scene::ShortcutReady => (Anchor::Shortcut, None, None),
            Scene::Microphone => (Anchor::Input, None, None),
            Scene::Model | Scene::ModelReady if self.redesign_config.rewrite => {
                (Anchor::Rewrite, Some(Anchor::Model), None)
            }
            Scene::Model | Scene::ModelReady => (
                Anchor::Model,
                Some(Anchor::ModelChoice),
                Some(Anchor::Rewrite),
            ),
            Scene::Floating | Scene::FloatingReady => (Anchor::Floating, None, None),
            Scene::Record if self.tour.issue == Some(PracticeIssue::Transcription) => {
                (Anchor::Transcript, Some(Anchor::Record), None)
            }
            Scene::Record => (Anchor::RecordButton, Some(Anchor::Shortcut), None),
            Scene::Live => (
                Anchor::RecordButton,
                Some(Anchor::Transcript),
                Some(Anchor::Record),
            ),
            Scene::Processing | Scene::Transcript => (Anchor::Transcript, None, None),
            Scene::Finish => (
                Anchor::HistoryLink,
                Some(Anchor::TermsLink),
                Some(Anchor::FilesLink),
            ),
        };
        let width = if step == Step::Welcome {
            WELCOME_WIDTH.min(viewport.width - px(32.))
        } else {
            CARD_WIDTH
        };
        Some(self.tour.layout.spotlight(
            frame,
            primary,
            companion,
            extra,
            self.unsaved.is_some() || self.tour.issue.is_some(),
            (!docked).then(|| self.story_card(step, width, None, cx)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_profiles_keep_their_first_launch_preference() {
        let root = std::env::temp_dir().join(format!("speaky-tour-{}", uuid::Uuid::new_v4()));
        let marker = root.join(MARKER);
        assert_eq!(first_step(&marker), Some(Step::Welcome));
        drafts::write_atomically(&marker, b"done\n").unwrap();
        assert_eq!(first_step(&marker), None);
        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}

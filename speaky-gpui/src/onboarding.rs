//! A first recording, guided through the actual Speaky interface.
mod card;
mod effects;
mod layout;
mod motion;
mod platform;

use std::{
    path::Path,
    time::{Duration, Instant},
};

use gpui::{
    div, prelude::*, px, rgb, Canvas, Context, Div, FocusHandle, Pixels, ScrollHandle, Size,
    Stateful, Window,
};

use crate::{
    action, audio::AudioRecorder, config::Config, drafts, icons::Icon, is_silent_recording, Page,
    RecordingStart, Speaky, ACCENT, BORDER, MUTED, SIDEBAR,
};

pub(crate) use layout::Anchor;
use layout::{Layout, CARD_WIDTH, WELCOME_WIDTH};
use motion::{Clock, Frame, Scene};

// Preserve the previous launch preference. Everyone can replay from the sidebar.
const MARKER: &str = "onboarding-v1.done";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Welcome,
    Microphone,
    Model,
    Record,
    Result,
    Finish,
}

const STEPS: [Step; 6] = [
    Step::Welcome,
    Step::Microphone,
    Step::Model,
    Step::Record,
    Step::Result,
    Step::Finish,
];

impl Step {
    fn page(self) -> Page {
        match self {
            Self::Microphone => Page::Settings,
            Self::Model if !cfg!(windows) => Page::Settings,
            _ => Page::Home,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Microphone {
    #[default]
    Untested,
    Checking,
    Ready,
    Quiet,
    Unavailable,
    Denied,
}

pub(crate) struct Tour {
    step: Option<Step>,
    layout: Layout,
    clock: Clock,
    frame: Frame,
    focus: FocusHandle,
    replay: FocusHandle,
    microphone: Microphone,
    microphone_check: u64,
    pub(crate) record_issue: bool,
    pub(crate) copied: bool,
    transition: u64,
    leaving: bool,
    #[cfg(windows)]
    model_rows: Option<Instant>,
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
            microphone: Microphone::Untested,
            microphone_check: 0,
            record_issue: false,
            copied: false,
            transition: 0,
            leaving: false,
            #[cfg(windows)]
            model_rows: None,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.step.is_some()
    }

    pub(crate) fn at(&self, step: Step) -> bool {
        self.step == Some(step)
    }

    pub(crate) fn mark(&self, anchor: Anchor) -> Option<Canvas<()>> {
        self.is_active().then(|| self.layout.mark(anchor))
    }

    pub(crate) fn track_page(&self) -> Option<&ScrollHandle> {
        self.is_active().then_some(&self.layout.scroll)
    }

    pub(crate) fn docked(&mut self, viewport: Size<Pixels>) -> bool {
        self.is_active() && !self.at(Step::Welcome) && self.layout.docked(viewport)
    }

    pub(crate) fn request_reveal(&self) {
        self.layout.reveal();
    }

    #[cfg(windows)]
    pub(crate) fn model_rows_arrived(&mut self) {
        if self.at(Step::Model) {
            self.model_rows = Some(Instant::now());
            self.request_reveal();
        }
    }

    #[cfg(windows)]
    pub(crate) fn model_row_motion(&self, index: usize) -> Option<(f32, Pixels)> {
        if !self.at(Step::Model) || !self.frame.moving {
            return None;
        }
        let now = self.model_rows?.elapsed().as_secs_f32() * 1000.;
        let delay = index.min(6) as f32 * 50.;
        let t = motion::ease_out(motion::phase(now, delay, delay + 225.));
        (t < 1.).then(|| (0.06 + 0.94 * t, px(7. * (1. - t))))
    }
}

fn first_step(marker: &Path) -> Option<Step> {
    (!marker.is_file()).then_some(Step::Welcome)
}

#[derive(Clone, Copy)]
enum GuideAction {
    Back,
    Next,
    StopTour,
    Locate,
    CheckMicrophone,
    Privacy,
    #[cfg(windows)]
    Models,
    #[cfg(any(windows, target_os = "macos"))]
    Service,
    Record,
    Copy,
    History,
}

type GuideButton = (&'static str, GuideAction);

impl Speaky {
    pub(crate) fn show_first_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tour.is_active() {
            self.show_tour_step(Step::Welcome, cx);
            window.focus(&self.tour.focus);
        }
    }

    fn restart_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tour.microphone_check = self.tour.microphone_check.wrapping_add(1);
        self.tour.microphone = Microphone::Untested;
        self.tour.record_issue = false;
        self.tour.copied = false;
        self.tour.leaving = false;
        self.tour.layout.clear_camera();
        self.show_tour_step(Step::Welcome, cx);
        window.focus(&self.tour.focus);
    }

    pub(crate) fn finish_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tour.step.take().is_none() {
            return;
        }
        self.tour.leaving = false;
        self.tour.microphone_check = self.tour.microphone_check.wrapping_add(1);
        if let Err(error) = drafts::write_atomically(&Config::config_dir().join(MARKER), b"done\n")
        {
            self.status = format!("无法保存引导进度：{error}");
        }
        window.focus(&self.tour.replay);
        cx.notify();
    }

    pub(crate) fn show_tour_step(&mut self, step: Step, cx: &mut Context<Self>) {
        self.tour.step = Some(step);
        self.tour.transition = self.tour.transition.wrapping_add(1);
        self.tour.layout.reset();
        #[cfg(windows)]
        {
            self.model_picker.close();
            self.tour.model_rows = None;
        }
        if self.page != step.page() || step.page() == Page::Settings {
            self.open_page(step.page(), cx);
        } else {
            cx.notify();
        }
        #[cfg(windows)]
        if step == Step::Model {
            self.toggle_model_picker(cx);
        }
    }

    pub(crate) fn prepare_tour(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.tour.step else {
            return;
        };
        let now = Instant::now();
        let moving = platform::motion_allowed();
        let scene = if self.tour.leaving {
            Scene::Exit
        } else {
            match step {
                Step::Welcome => Scene::Welcome,
                Step::Microphone => Scene::Microphone,
                Step::Model => Scene::Model,
                Step::Record if self.recording.is_some() => Scene::Live,
                Step::Record => Scene::Record,
                Step::Result if self.page == Page::History => Scene::Archive,
                Step::Result
                    if self.page == Page::Home
                        && moving
                        && !self.transcribing
                        && (self.tour.clock.scene() == Scene::HistoryLink
                            || (self.tour.clock.scene() == Scene::Transcript
                                && self.tour.clock.elapsed(now) >= 1450.)) =>
                {
                    Scene::HistoryLink
                }
                Step::Result => Scene::Transcript,
                Step::Finish => Scene::Finish,
            }
        };
        let old_scene = self.tour.frame.scene;
        let frame = self
            .tour
            .clock
            .frame(scene, step as usize, self.tour.transition, now, moving);
        self.tour.frame = frame;
        if old_scene != scene && matches!(scene, Scene::HistoryLink | Scene::Archive) {
            self.tour.request_reveal();
        }
        if self.tour.leaving && !frame.animating() {
            self.finish_tour(window, cx);
            return;
        }
        #[cfg(windows)]
        let model_options_moving = self.tour.model_row_motion(6).is_some();
        #[cfg(not(windows))]
        let model_options_moving = false;
        if frame.animating() || model_options_moving {
            window.request_animation_frame();
        }
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
        // The previous frame can still receive a second click before it is replaced.
        if epoch != self.tour.transition || self.tour.leaving {
            return;
        }
        match action {
            GuideAction::Next if step == Step::Record && self.recording.is_some() => {
                self.stop_recording(cx)
            }
            GuideAction::Back | GuideAction::Next => {
                let index = step as usize;
                let next = if matches!(action, GuideAction::Back) {
                    STEPS[index.saturating_sub(1)]
                } else if let Some(next) = STEPS.get(index + 1) {
                    *next
                } else {
                    if self.tour.frame.moving {
                        self.tour.leaving = true;
                        self.tour.transition = self.tour.transition.wrapping_add(1);
                        cx.notify();
                    } else {
                        self.finish_tour(window, cx);
                    }
                    return;
                };
                self.show_tour_step(next, cx);
                window.focus(&self.tour.focus);
            }
            GuideAction::StopTour => self.finish_tour(window, cx),
            GuideAction::Locate => self.show_tour_step(step, cx),
            GuideAction::CheckMicrophone => self.check_tour_microphone(cx),
            GuideAction::Privacy => cx.open_url(platform::MICROPHONE_SETTINGS),
            #[cfg(windows)]
            GuideAction::Models => self.toggle_model_picker(cx),
            #[cfg(any(windows, target_os = "macos"))]
            GuideAction::Service => self.open_service_settings(window, cx),
            GuideAction::Record => self.toggle_recording(RecordingStart::Button, cx),
            GuideAction::Copy => self.copy_recent_text(cx),
            GuideAction::History => self.open_page(Page::History, cx),
        }
    }

    fn check_tour_microphone(&mut self, cx: &mut Context<Self>) {
        if self.recording.is_some() || self.tour.microphone == Microphone::Checking {
            return;
        }
        self.tour.microphone = Microphone::Checking;
        self.tour.microphone_check = self.tour.microphone_check.wrapping_add(1);
        let check = self.tour.microphone_check;
        cx.spawn(async move |entity, cx| {
            let state = if platform::microphone_permission().await {
                cx.background_spawn(async {
                    match probe_microphone() {
                        Ok(state) => state,
                        Err(error) => {
                            log::debug!("Guide microphone check: {error}");
                            Microphone::Unavailable
                        }
                    }
                })
                .await
            } else {
                Microphone::Denied
            };
            let _ = entity.update(cx, |view, cx| {
                if view.tour.microphone_check == check {
                    view.tour.microphone = state;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn tour_entry(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        action(false)
            .id("replay-tour")
            .h(px(36.))
            .flex_1()
            .justify_start()
            .px_2()
            .gap_1p5()
            .bg(rgb(SIDEBAR))
            .text_color(rgb(MUTED))
            .child(Icon::Guide.render(ACCENT))
            .child("使用引导")
            .when(!self.tour.is_active(), |row| {
                row.track_focus(&self.tour.replay)
                    .tab_stop(true)
                    .focus(|row| row.border_1().border_color(rgb(ACCENT)))
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
                .px_5()
                .pt_2()
                .pb_2()
                .bg(rgb(0xe2ebe3))
                .when(self.tour.leaving, |dock| {
                    dock.opacity(1. - self.tour.frame.card())
                })
                .border_t_1()
                .border_color(rgb(BORDER))
                .child(self.story_card(
                    step,
                    (viewport.width - px(204.)).min(px(553.)),
                    Some(viewport.height * 0.50),
                    cx,
                ))
                .children(self.tour.mark(Anchor::Dock)),
        )
    }

    pub(crate) fn tour_overlay(
        &self,
        docked: bool,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let step = self.tour.step?;
        let frame = self.tour.frame;
        let (primary, companion) = match frame.scene {
            Scene::Welcome | Scene::Finish | Scene::Exit => (Anchor::Workspace, None),
            Scene::Microphone => (Anchor::Input, None),
            Scene::Model if self.page == Page::Settings => (Anchor::Service, None),
            Scene::Model => (Anchor::Model, None),
            Scene::Record => (Anchor::Record, None),
            Scene::Live if docked => (Anchor::Transcript, Some(Anchor::Record)),
            Scene::Live => (Anchor::Record, Some(Anchor::Transcript)),
            Scene::Transcript => (Anchor::Transcript, None),
            Scene::HistoryLink => (Anchor::HistoryLink, Some(Anchor::Transcript)),
            Scene::Archive => (Anchor::History, None),
        };
        let width = if step == Step::Welcome {
            WELCOME_WIDTH
        } else {
            CARD_WIDTH
        };
        let allow_history = step == Step::Result && self.page == Page::Home && !frame.moving;
        Some(self.tour.layout.spotlight(
            frame,
            primary,
            companion,
            allow_history,
            self.unsaved.is_some()
                || (step == Step::Record && self.tour.record_issue)
                || (step == Step::Result && self.playback.is_some()),
            (!docked).then(|| self.story_card(step, width, None, cx)),
        ))
    }
}

fn probe_microphone() -> Result<Microphone, String> {
    let input = Config::load().map_err(|error| error.to_string())?.core.asr;
    let mut recorder = AudioRecorder::new_with_name(
        input.audio_device,
        input.audio_device_name.as_deref(),
        input.audio_gain,
    )?;
    recorder.start()?;
    std::thread::sleep(Duration::from_millis(1600));
    let wav = recorder.stop();
    Ok(if wav.is_empty() {
        Microphone::Unavailable
    } else if is_silent_recording(&wav) {
        Microphone::Quiet
    } else {
        Microphone::Ready
    })
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

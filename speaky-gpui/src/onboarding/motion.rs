//! One clock drives the spotlight, card, progress and scene details.
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Scene {
    #[default]
    Welcome,
    Shortcut,
    ShortcutReady,
    Microphone,
    Model,
    ModelReady,
    Floating,
    FloatingReady,
    Record,
    Live,
    Transcript,
    Processing,
    Finish,
}

struct Cue {
    focus: (f32, f32),
    card: (f32, f32),
    content: (f32, f32),
    end: f32,
}

impl Scene {
    fn cue(self) -> Cue {
        let (focus, card, content, end) = match self {
            Self::Welcome => ((25., 410.), (0., 280.), (95., 405.), 550.),
            Self::Shortcut | Self::Microphone | Self::Model | Self::Floating | Self::Finish => {
                ((0., 330.), (0., 330.), (35., 340.), 465.)
            }
            Self::ShortcutReady | Self::ModelReady | Self::FloatingReady => {
                ((0., 220.), (0., 260.), (0., 320.), 560.)
            }
            Self::Record => ((0., 340.), (0., 325.), (40., 350.), 635.),
            Self::Live | Self::Processing => ((0., 310.), (0., 310.), (35., 325.), 430.),
            Self::Transcript => ((0., 370.), (0., 360.), (80., 430.), 600.),
        };
        Cue {
            focus,
            card,
            content,
            end,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub scene: Scene,
    pub previous: Scene,
    pub serial: u64,
    pub elapsed: f32,
    pub moving: bool,
    pub progress: f32,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            scene: Scene::Welcome,
            previous: Scene::Welcome,
            serial: 0,
            elapsed: 0.,
            moving: false,
            progress: 0.,
        }
    }
}

impl Frame {
    pub fn animating(self) -> bool {
        self.moving && self.elapsed < self.scene.cue().end
    }

    pub fn focus(self) -> f32 {
        smooth(self.part(self.scene.cue().focus))
    }

    pub fn card(self) -> f32 {
        ease_out(self.part(self.scene.cue().card))
    }

    pub fn content(self) -> f32 {
        smooth(self.part(self.scene.cue().content))
    }

    pub fn part(self, (start, end): (f32, f32)) -> f32 {
        if self.moving {
            phase(self.elapsed, start, end)
        } else {
            1.
        }
    }
}

pub(super) struct Clock {
    scene: Scene,
    previous: Scene,
    start: Instant,
    serial: u64,
    from_progress: f32,
    target_progress: f32,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            scene: Scene::Welcome,
            previous: Scene::Welcome,
            start: Instant::now(),
            serial: 0,
            from_progress: 0.,
            target_progress: 0.,
        }
    }
}

impl Clock {
    pub fn elapsed(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.start).as_secs_f32() * 1000.
    }

    pub fn frame(
        &mut self,
        scene: Scene,
        index: usize,
        epoch: u64,
        now: Instant,
        moving: bool,
    ) -> Frame {
        if self.serial != epoch || self.scene != scene {
            self.from_progress = self.progress(self.elapsed(now), moving);
            self.target_progress = index as f32;
            self.previous = self.scene;
            self.scene = scene;
            self.serial = epoch;
            self.start = now;
        }
        let elapsed = self.elapsed(now);
        Frame {
            scene,
            previous: self.previous,
            serial: epoch,
            elapsed,
            moving,
            progress: self.progress(elapsed, moving),
        }
    }

    fn progress(&self, elapsed: f32, moving: bool) -> f32 {
        if moving {
            lerp(
                self.from_progress,
                self.target_progress,
                smooth(phase(elapsed, 25., 315.)),
            )
        } else {
            self.target_progress
        }
    }
}

pub(super) fn phase(value: f32, start: f32, end: f32) -> f32 {
    ((value - start) / (end - start)).clamp(0., 1.)
}

pub(super) fn smooth(t: f32) -> f32 {
    t * t * (3. - 2. * t)
}

pub(super) fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

pub(super) fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn changing_direction_uses_the_progress_already_displayed() {
        let mut clock = Clock::default();
        let time = Instant::now();
        clock.frame(Scene::Record, 3, 1, time, true);
        let interruption = time + Duration::from_millis(235);
        let first = clock.frame(Scene::Record, 3, 1, interruption, true);
        let second = clock.frame(Scene::Model, 2, 2, interruption, true);
        assert_eq!(first.progress, second.progress);
        assert_eq!(second.previous, Scene::Record);
        let reduced = clock.frame(Scene::Model, 2, 2, interruption, false);
        assert_eq!(reduced.progress, 2.);
        assert_eq!(reduced.focus(), 1.);
        assert!(!reduced.animating());
    }
}

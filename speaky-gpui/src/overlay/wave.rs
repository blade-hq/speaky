use crate::audio::VoiceActivity;
use gpui::{
    canvas, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, Bounds, PathBuilder,
    Pixels, Window,
};
use std::f32::consts::{PI, TAU};

const COLORS: [u32; 5] = [0x59caff, 0x8198ff, 0xbe89ff, 0xff82bc, 0xffb47c];

#[derive(Clone, Copy)]
pub(super) struct VoiceWave {
    amplitude: f32,
    pitch: f32,
    phase: f32,
}

impl Default for VoiceWave {
    fn default() -> Self {
        Self {
            amplitude: 0.,
            pitch: 0.5,
            phase: 0.,
        }
    }
}

impl VoiceWave {
    pub(super) fn update(&mut self, activity: VoiceActivity) -> bool {
        let level = activity.level;
        // Expand quieter syllables without setting an idle wave in motion.
        let target = if level.is_finite() && level > 0.0008 {
            ((20. * level.log10() + 62.) / 50.).clamp(0., 1.).sqrt()
        } else {
            0.
        };
        let previous = self.amplitude;
        let smoothing = if target > previous { 0.80 } else { 0.34 };
        self.amplitude += (target - previous) * smoothing;
        if self.amplitude < 0.005 {
            self.amplitude = 0.;
        }
        if let Some(hz) = activity.pitch_hz {
            let target_pitch = ((hz / 80.).log2() / (440_f32 / 80.).log2()).clamp(0., 1.);
            self.pitch += (target_pitch - self.pitch) * 0.54;
        } else {
            self.pitch += (0.5 - self.pitch) * 0.20;
        }
        if self.amplitude > 0. {
            self.phase = (self.phase + 0.21 + 0.44 * self.pitch) % TAU;
        }
        self.amplitude > 0. || previous > 0.
    }

    fn cycles(self) -> f32 {
        2.4 + self.pitch * 6.4
    }

    pub(super) fn element(self) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                if self.amplitude > 0.08 {
                    self.paint_curve(bounds, window, true);
                }
                self.paint_curve(bounds, window, false);
            },
        )
        .w_full()
        .h(px(28.))
        .flex_none()
    }

    fn paint_curve(self, bounds: Bounds<Pixels>, window: &mut Window, echo: bool) {
        let width = f32::from(bounds.size.width) - 2.;
        if width < 4. {
            return;
        }
        let height = f32::from(bounds.size.height);
        // Pitch changes both the crest spacing and the vertical centre.
        // Loudness still determines how far the curve moves from that centre.
        let center = height * (0.5 + (0.5 - self.pitch) * 0.40 * self.amplitude);
        let amplitude = (center.min(height - center) - 1.8).max(0.) * self.amplitude;
        let cycles = self.cycles();
        let (phase, scale, opacity, thickness) = if echo {
            (self.phase + 1.65, 0.65, self.amplitude * 0.4, 1.2)
        } else {
            (
                self.phase,
                1.,
                0.47 + 0.53 * self.amplitude,
                1.4 + self.amplitude,
            )
        };
        let position = |t: f32| {
            // Modulate the carrier to create crests of different heights and
            // always meet the centre line at the two ends.
            let carrier = t * cycles * TAU;
            let contour = (t * PI).sin()
                * (0.74 * (carrier - phase).sin() + 0.26 * (2. * carrier + phase).sin());
            bounds.origin + point(px(1. + t * width), px(center - amplitude * scale * contour))
        };

        // GPUI's linear gradient has two stops. Join adjacent colour spans at
        // the exact same point on the curve for a continuous multi-stop line.
        let spans = (COLORS.len() - 1) as f32;
        let steps = (width / spans / 2.8).ceil() as usize;
        for (index, colors) in COLORS.windows(2).enumerate() {
            let mut path = PathBuilder::stroke(px(thickness));
            path.move_to(position(index as f32 / spans));
            for step in 1..=steps {
                path.line_to(position(
                    (index as f32 + step as f32 / steps as f32) / spans,
                ));
            }
            if let Ok(path) = path.build() {
                let gradient = linear_gradient(
                    90.,
                    linear_color_stop(rgb(colors[0]), 0.),
                    linear_color_stop(rgb(colors[1]), 1.),
                )
                .opacity(opacity);
                window.paint_path(path, gradient);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_rises_with_loudness_and_settles_to_a_still_line() {
        let mut wave = VoiceWave::default();
        let volume = |level| VoiceActivity {
            level,
            pitch_hz: None,
        };
        assert!(!wave.update(volume(0.)));
        assert!(!wave.update(volume(0.0004)));
        assert!(wave.update(volume(0.003)));
        let quiet = wave.amplitude;
        assert!(quiet > 0.1);
        assert!(wave.update(volume(0.16)));
        assert!(wave.amplitude > quiet * 1.8);
        for _ in 0..24 {
            wave.update(volume(0.));
        }
        assert!(!wave.update(volume(0.)));
        assert_eq!(wave.amplitude, 0.);
    }

    #[test]
    fn a_higher_pitch_draws_more_crests_at_the_same_volume() {
        let mut low = VoiceWave::default();
        let mut high = VoiceWave::default();
        for _ in 0..6 {
            low.update(VoiceActivity {
                level: 0.015,
                pitch_hz: Some(110.),
            });
            high.update(VoiceActivity {
                level: 0.015,
                pitch_hz: Some(330.),
            });
        }
        assert_eq!(low.amplitude, high.amplitude);
        assert!(high.cycles() > low.cycles() * 1.75);
    }
}

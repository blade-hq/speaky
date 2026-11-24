use crate::audio::VoiceActivity;
use std::f32::consts::{PI, TAU};

pub const COLORS: [u32; 5] = [0x59caff, 0x8198ff, 0xbe89ff, 0xff82bc, 0xffb47c];

#[derive(Clone, Copy)]
pub struct VoiceWave {
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

pub struct WaveCurve {
    pub spans: [Vec<(f32, f32)>; COLORS.len() - 1],
    pub opacity: f32,
    pub thickness: f32,
}

impl VoiceWave {
    pub fn update(&mut self, activity: VoiceActivity) -> bool {
        self.update_dt(activity, 0.065)
    }

    pub fn update_dt(&mut self, activity: VoiceActivity, dt_secs: f32) -> bool {
        if !dt_secs.is_finite() || dt_secs <= 0. {
            return false;
        }
        let steps = dt_secs / 0.065;
        let smooth = |weight: f32| 1. - (1. - weight).powf(steps);
        let level = activity.level;
        // Expand quieter syllables without setting an idle wave in motion.
        let target = if level.is_finite() && level > 0.0008 {
            ((20. * level.log10() + 62.) / 50.).clamp(0., 1.).sqrt()
        } else {
            0.
        };
        let previous = self.amplitude;
        let smoothing = if target > previous { 0.80 } else { 0.34 };
        self.amplitude += (target - previous) * smooth(smoothing);
        if self.amplitude < 0.005 {
            self.amplitude = 0.;
        }
        if let Some(hz) = activity.pitch_hz.filter(|hz| hz.is_finite() && *hz > 0.) {
            let target_pitch = ((hz / 80.).log2() / (440_f32 / 80.).log2()).clamp(0., 1.);
            self.pitch += (target_pitch - self.pitch) * smooth(0.54);
        } else {
            self.pitch += (0.5 - self.pitch) * smooth(0.20);
        }
        if self.amplitude > 0. {
            self.phase = (self.phase + (0.21 + 0.44 * self.pitch) * steps.min(2.)) % TAU;
        }
        self.amplitude > 0. || previous > 0.
    }

    fn cycles(self) -> f32 {
        2.4 + self.pitch * 6.4
    }

    pub fn sample(self, width: f32, height: f32, echo: bool) -> Option<WaveCurve> {
        let width = width - 2.;
        if !width.is_finite()
            || width < 4.
            || !height.is_finite()
            || height <= 0.
            || (echo && self.amplitude <= 0.08)
        {
            return None;
        }
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
            (1. + t * width, center - amplitude * scale * contour)
        };

        let spans = (COLORS.len() - 1) as f32;
        let steps = (width / spans / 2.8).ceil() as usize;
        Some(WaveCurve {
            spans: std::array::from_fn(|index| {
                (0..=steps)
                    .map(|step| position((index as f32 + step as f32 / steps as f32) / spans))
                    .collect()
            }),
            opacity,
            thickness,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_tracks_elapsed_time_at_different_frame_rates() {
        let speak = VoiceActivity {
            level: 0.05,
            pitch_hz: Some(220.),
        };
        let mut slow = VoiceWave::default();
        let mut fast = slow;
        for _ in 0..15 {
            slow.update_dt(speak, 1. / 15.);
        }
        for _ in 0..60 {
            fast.update_dt(speak, 1. / 60.);
        }
        assert!((slow.amplitude - fast.amplitude).abs() < 1e-5);
        assert!((slow.pitch - fast.pitch).abs() < 1e-5);
        assert!((slow.phase - fast.phase).abs() < 0.1);

        let curve = slow.sample(112., 28., false).unwrap();
        assert!(slow.sample(112., 28., true).unwrap().opacity < curve.opacity);
        for spans in curve.spans.windows(2) {
            assert_eq!(spans[0].last(), spans[1].first());
        }
        assert_eq!(curve.spans[0][0].0, 1.);
        assert_eq!(curve.spans.last().unwrap().last().unwrap().0, 111.);
    }

    #[test]
    fn long_pauses_limit_phase_and_silence_drops_the_echo() {
        let speak = VoiceActivity {
            level: 0.1,
            pitch_hz: None,
        };
        let mut wave = VoiceWave::default();
        wave.update(speak);
        let phase = wave.phase;
        wave.update_dt(speak, 30.);
        assert!((wave.phase - phase - 0.86).abs() < 1e-5);
        let quiet = VoiceActivity {
            level: 0.,
            pitch_hz: None,
        };
        wave.update_dt(quiet, 30.);
        assert!(wave.sample(112., 28., true).is_none());
        let curve = wave.sample(112., 28., false).unwrap();
        assert!(curve.spans.iter().flatten().all(|&(_, y)| y == 14.));
        assert!(!wave.update(quiet));
    }

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

use super::recorder::SAMPLE_RATE;

pub(super) const WINDOW_SAMPLES: usize = 1024;
const DOWNSAMPLE: usize = 4;
const ANALYSIS_RATE: usize = SAMPLE_RATE as usize / DOWNSAMPLE;
const MIN_LAG: usize = ANALYSIS_RATE / 550;
const MAX_LAG: usize = ANALYSIS_RATE / 65;
const WINDOW: usize = WINDOW_SAMPLES / DOWNSAMPLE;
const COMPARISON_SAMPLES: usize = WINDOW - MAX_LAG - 1;

pub struct VoiceActivity {
    pub level: f32,
    pub pitch_hz: Option<f32>,
}

impl VoiceActivity {
    pub(super) fn from_pcm(samples: &[i16]) -> Self {
        let level = level(samples);
        let pitch_hz = (level > 0.0008 && samples.len() == WINDOW_SAMPLES)
            .then(|| estimate_pitch(samples))
            .flatten();
        Self { level, pitch_hz }
    }
}

pub(super) fn level(samples: &[i16]) -> f32 {
    let sum: i64 = samples.iter().map(|&sample| i64::from(sample).abs()).sum();
    (sum as f32 / samples.len().max(1) as f32 / 32768.).min(1.)
}

fn estimate_pitch(samples: &[i16]) -> Option<f32> {
    let mut reduced = [0.; WINDOW];
    for (target, block) in reduced.iter_mut().zip(samples.chunks_exact(DOWNSAMPLE)) {
        *target = block.iter().map(|&sample| f32::from(sample)).sum::<f32>() / DOWNSAMPLE as f32;
    }
    // Use the cumulative normalized difference and first qualifying minimum
    // from YIN (de Cheveigné and Kawahara, doi:10.1121/1.1458024).
    let mut normalized = [1.; MAX_LAG + 2];
    let mut sum = 0.;
    for (lag, value) in normalized.iter_mut().enumerate().skip(1) {
        let difference: f32 = reduced[..COMPARISON_SAMPLES]
            .iter()
            .zip(&reduced[lag..])
            .map(|(&a, &b)| (a - b).powi(2))
            .sum();
        sum += difference;
        if sum > 0. {
            *value = difference * lag as f32 / sum;
        }
    }
    let mut lag = (MIN_LAG..=MAX_LAG).find(|&lag| normalized[lag] < 0.20)?;
    while lag < MAX_LAG && normalized[lag + 1] < normalized[lag] {
        lag += 1;
    }
    let (left, middle, right) = (normalized[lag - 1], normalized[lag], normalized[lag + 1]);
    let curvature = left - 2. * middle + right;
    let shift = if curvature > 0. {
        (0.5 * (left - right) / curvature).clamp(-0.5, 0.5)
    } else {
        0.
    };
    let pitch = ANALYSIS_RATE as f32 / (lag as f32 + shift);
    (65. ..=550.).contains(&pitch).then_some(pitch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn distinguishes_pitch_at_equal_loudness() {
        for hz in [82., 110., 220., 330., 510.] {
            let samples: [i16; WINDOW_SAMPLES] = std::array::from_fn(|i| {
                let phase = TAU * hz * i as f32 / SAMPLE_RATE as f32;
                (350. * phase.sin() + 950. * (2. * phase + 0.3).sin()) as i16
            });
            let activity = VoiceActivity::from_pcm(&samples);
            assert!(activity.level > 0.01);
            let estimate = activity.pitch_hz.unwrap();
            assert!(
                (estimate - hz).abs() < hz * 0.04,
                "{hz} Hz measured {estimate}"
            );
        }
    }

    #[test]
    fn does_not_assign_pitch_to_silence_or_noise() {
        assert!(VoiceActivity::from_pcm(&[0; WINDOW_SAMPLES])
            .pitch_hz
            .is_none());
        assert!(VoiceActivity::from_pcm(&[320; WINDOW_SAMPLES])
            .pitch_hz
            .is_none());
        let mut state = 0x1788_f582_u32;
        let noise: [i16; WINDOW_SAMPLES] = std::array::from_fn(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 16) as i16) / 8
        });
        let activity = VoiceActivity::from_pcm(&noise);
        assert!(activity.level > 0.03);
        assert!(activity.pitch_hz.is_none());
    }
}

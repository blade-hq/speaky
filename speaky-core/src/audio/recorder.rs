use super::meter::{self, VoiceActivity, WINDOW_SAMPLES};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, StreamConfig};
use log::{debug, error, info, warn};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Audio format constants
pub(super) const SAMPLE_RATE: u32 = 16000;
const CHANNELS: u16 = 1;
const SAMPLE_WIDTH: u16 = 2; // 16-bit
const PREVIEW_LIMIT: usize = 12 * SAMPLE_RATE as usize;
const FIRST_PREVIEW_STEP: usize = SAMPLE_RATE as usize * 55 / 100;
// Allow enough new speech for a useful hypothesis before decoding the phrase again.
const PREVIEW_STEP: usize = SAMPLE_RATE as usize * 4 / 5;

pub struct PreviewAudio {
    pub end: usize,
    pub complete: bool,
    /// Empty for a quiet span; the live cursor may advance without an ASR call.
    pub wav: Vec<u8>,
}

impl PreviewAudio {
    pub fn next_start_after_quiet(&self, start: usize) -> usize {
        // Retain 100 ms of pre-roll for a soft onset at the end of the quiet span.
        self.end
            .saturating_sub(SAMPLE_RATE as usize / 10)
            .max(start)
    }
}

// Short clips of microphone room tone can produce invented words from a
// batch recognizer. This very low floor applies only to live hypotheses.
fn preview_has_signal(samples: &[i16]) -> bool {
    let frame_size = SAMPLE_RATE as usize / 50;
    samples.chunks(frame_size).any(|frame| {
        let energy = frame
            .iter()
            .map(|&sample| i32::from(sample).abs())
            .sum::<i32>();
        energy >= 12 * frame.len() as i32
    })
}

// A partial is always decoded from the start of its phrase. After speech,
// 400 ms of low energy ends the phrase; keep that pause in the submitted WAV.
fn preview_boundary(samples: &[i16]) -> (usize, bool) {
    const RATE: usize = SAMPLE_RATE as usize;
    const FRAME: usize = RATE / 50; // 20 ms
    let count = samples.len().min(PREVIEW_LIMIT);
    let mut loudest = 0;
    let mut quiet_frames = 0;
    let mut quietest_tail = i32::MAX;
    let mut tail_cut = PREVIEW_LIMIT;
    for (index, frame) in samples[..count].chunks_exact(FRAME).enumerate() {
        let energy = frame.iter().map(|&s| i32::from(s).abs()).sum::<i32>() / FRAME as i32;
        let end = (index + 1) * FRAME;
        if loudest > 0 && energy <= (loudest / 8).min(200) {
            quiet_frames += 1;
            if quiet_frames == 20 {
                return (end, true);
            }
        } else {
            quiet_frames = 0;
        }
        loudest = loudest.max(energy);
        if end >= PREVIEW_LIMIT - 2 * RATE && energy <= quietest_tail {
            quietest_tail = energy;
            tail_cut = end - FRAME / 2;
        }
    }
    if count == PREVIEW_LIMIT {
        (tail_cut, true)
    } else {
        (count, false)
    }
}

/// Thread-safe wrapper for cpal Stream
/// cpal::Stream doesn't implement Send/Sync, so we need to protect it with a Mutex
/// and only allow access through our controlled methods
struct StreamHandle(Option<Stream>);

/// Streaming area resampler. Each 16 kHz output sample is the weighted mean
/// of the source interval it represents. This provides a small anti-aliasing
/// low-pass filter and, unlike selecting every Nth source sample, keeps its
/// phase continuous across CPAL callback boundaries.
struct AreaResampler {
    output_interval: u64,
    remaining: u64,
    weighted_sum: f64,
    accumulated_weight: u64,
}

impl AreaResampler {
    fn new(input_rate: u32) -> Self {
        // Integer phase units avoid long-term drift for rates such as
        // 44.1 kHz that cannot be represented exactly as a floating ratio.
        let output_interval = input_rate as u64;
        Self {
            output_interval,
            remaining: output_interval,
            weighted_sum: 0.0,
            accumulated_weight: 0,
        }
    }

    fn push(&mut self, sample: f64, output: &mut Vec<i16>, gain: f64) {
        let mut source_weight = SAMPLE_RATE as u64;
        while source_weight > 0 {
            let weight = source_weight.min(self.remaining);
            self.weighted_sum += sample * weight as f64;
            self.accumulated_weight += weight;
            self.remaining -= weight;
            source_weight -= weight;

            if self.remaining == 0 {
                let averaged = self.weighted_sum / self.accumulated_weight.max(1) as f64;
                output.push((averaged * gain).clamp(-32768.0, 32767.0) as i16);
                self.remaining = self.output_interval;
                self.weighted_sum = 0.0;
                self.accumulated_weight = 0;
            }
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InputDevices {
    available: Vec<(u32, String)>,
    default_name: Option<String>,
}

impl InputDevices {
    pub fn discover() -> Self {
        let host = cpal::default_host();
        Self::from_devices(&input_devices(&host), host.default_input_device().as_ref())
    }

    fn from_devices(devices: &[Device], default: Option<&Device>) -> Self {
        let available = devices
            .iter()
            .enumerate()
            .map(|(index, device)| {
                let name = device.name().unwrap_or_else(|_| format!("Device {index}"));
                (index as u32, name)
            })
            .collect();
        let default_name =
            default.map(|device| device.name().unwrap_or_else(|_| "系统默认麦克风".into()));
        Self {
            available,
            default_name,
        }
    }

    pub fn available(&self) -> &[(u32, String)] {
        &self.available
    }

    fn configured(&self, index: Option<u32>, name: Option<&str>) -> Option<&(u32, String)> {
        // Only use the index for older profiles with no saved device name.
        // Unplugging a named device can assign its old index to another device.
        self.available.iter().find(|(id, label)| match name {
            Some(name) => name == label,
            None => index == Some(*id),
        })
    }

    pub fn configured_name(&self, index: Option<u32>, name: Option<&str>) -> Option<&str> {
        self.configured(index, name)
            .map(|(_, label)| label.as_str())
    }

    pub fn preferred_name(&self, index: Option<u32>, name: Option<&str>) -> Option<&str> {
        self.configured_name(index, name)
            .or(self.default_name.as_deref())
            .or_else(|| self.available.last().map(|(_, label)| label.as_str()))
    }
}

fn input_devices(host: &Host) -> Vec<Device> {
    host.input_devices()
        .map(|devices| devices.collect())
        .unwrap_or_default()
}

/// Audio recorder using cpal for cross-platform support
pub struct AudioRecorder {
    device: Option<Device>,
    opened_input_name: Option<String>,
    stream: Mutex<StreamHandle>,
    frames: Arc<Mutex<Vec<i16>>>,
    is_recording: Arc<AtomicBool>,
    gain: f64,
    audio_level_callback: Arc<Mutex<Option<Box<dyn Fn(f32) + Send + Sync>>>>,
    audio_data_callback: Arc<Mutex<Option<Box<dyn Fn(&[u8]) + Send + Sync>>>>,
}

impl AudioRecorder {
    /// Create a new audio recorder
    ///
    /// # Arguments
    /// * `device_index` - Optional index of audio device to use
    /// * `gain` - Audio gain factor (clamped between 0.1 and 5.0)
    ///
    /// # Returns
    /// A new AudioRecorder instance
    pub fn new(device_index: Option<u32>, gain: f64) -> Result<Self, String> {
        Self::new_with_name(device_index, None, gain)
    }

    /// Create a recorder, preferring a stable device name over its volatile
    /// enumeration index. USB and PipeWire device indices can change between
    /// application launches or while an audio service is restarting.
    pub fn new_with_name(
        device_index: Option<u32>,
        device_name: Option<&str>,
        gain: f64,
    ) -> Result<Self, String> {
        let host = cpal::default_host();

        let mut enumerated = input_devices(&host);
        let default = host.default_input_device();
        let inputs = InputDevices::from_devices(&enumerated, default.as_ref());
        let configured = inputs.configured(device_index, device_name);
        if configured.is_none() && (device_name.is_some() || device_index.is_some()) {
            warn!("Configured audio input is unavailable; falling back to the default input");
        }
        let device = configured
            .and_then(|(index, _)| enumerated.get(*index as usize).cloned())
            .or(default)
            .or_else(|| enumerated.pop());

        let clamped_gain = gain.clamp(0.1, 5.0);
        if device.is_none() {
            warn!("No audio input device found, recording may not work");
        } else if let Ok(device_name) = device.as_ref().unwrap().name() {
            info!(
                "Using audio device: {} with gain {}",
                device_name, clamped_gain
            );
        }

        Ok(Self {
            device,
            opened_input_name: None,
            stream: Mutex::new(StreamHandle(None)),
            // Pre-allocate with capacity to reduce reallocations during recording
            frames: Arc::new(Mutex::new(Vec::with_capacity(SAMPLE_RATE as usize * 60))), // 1 minute max
            is_recording: Arc::new(AtomicBool::new(false)),
            gain: clamped_gain,
            audio_level_callback: Arc::new(Mutex::new(None)),
            audio_data_callback: Arc::new(Mutex::new(None)),
        })
    }

    /// Get list of available input devices
    ///
    /// # Returns
    /// Vector of tuples containing (device_index, device_name)
    pub fn get_devices() -> Vec<(u32, String)> {
        InputDevices::discover().available
    }

    /// Name of the input that supplied audio when the last stream was opened.
    pub fn opened_input_name(&self) -> Option<&str> {
        self.opened_input_name.as_deref()
    }

    /// Set the audio level callback
    pub fn set_audio_level_callback<F>(&mut self, callback: F)
    where
        F: Fn(f32) + Send + Sync + 'static,
    {
        *self.audio_level_callback.lock() = Some(Box::new(callback));
    }

    /// Set the audio data callback for streaming ASR
    pub fn set_audio_data_callback<F>(&mut self, callback: F)
    where
        F: Fn(&[u8]) + Send + Sync + 'static,
    {
        *self.audio_data_callback.lock() = Some(Box::new(callback));
    }

    pub fn clear_audio_data_callback(&mut self) {
        *self.audio_data_callback.lock() = None;
    }

    /// Start recording
    ///
    /// # Returns
    /// Result indicating success or error message
    ///
    /// # Notes
    /// If already recording, this is a no-op and returns Ok(())
    pub fn start(&mut self) -> Result<(), String> {
        self.start_with_frames(Vec::new())
    }

    #[allow(dead_code)]
    pub fn resume_wav(&mut self, wav: &[u8]) -> Result<(), String> {
        self.start_with_frames(Self::recorded_frames(wav)?)
    }

    pub fn recorded_frames(wav: &[u8]) -> Result<Vec<i16>, String> {
        let payload = wav.get(44..).filter(|payload| {
            wav.starts_with(b"RIFF")
                && wav.get(8..16) == Some(b"WAVEfmt ")
                && wav.get(20..24) == Some(&[1, 0, 1, 0])
                && wav.get(24..28) == Some(&SAMPLE_RATE.to_le_bytes())
                && wav.get(34..40) == Some(b"\x10\x00data")
                && wav.get(40..44) == Some(&(payload.len() as u32).to_le_bytes())
                && payload.len() % 2 == 0
        });
        Ok(payload
            .ok_or("录音格式无法续录。")?
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
            .collect())
    }

    fn start_with_frames(&mut self, initial_frames: Vec<i16>) -> Result<(), String> {
        if self.is_recording.load(Ordering::SeqCst) {
            debug!("Already recording, ignoring start request");
            return Ok(());
        }

        let saved_samples = initial_frames.len();
        self.opened_input_name = None;
        *self.frames.lock() = initial_frames;
        self.is_recording.store(true, Ordering::SeqCst);

        let frames = Arc::clone(&self.frames);
        let is_recording = Arc::clone(&self.is_recording);
        let audio_level_callback = Arc::clone(&self.audio_level_callback);
        let audio_data_callback = Arc::clone(&self.audio_data_callback);
        let gain = self.gain;

        // Prefer the configured/default device, but always keep every current
        // input device as a fallback. Device indices are not stable across
        // PipeWire/ALSA restarts or USB reconnects, so restricting an
        // explicitly configured index to one candidate can permanently make
        // recording fail after the device list changes.
        let mut candidates = Vec::new();
        if let Some(device) = self.device.take() {
            candidates.push(device);
        }
        let host = cpal::default_host();
        candidates.extend(host.default_input_device());
        if let Ok(devices) = host.input_devices() {
            candidates.extend(devices);
        }

        // On Linux, PipeWire/ALSA may expose a virtual "default" endpoint
        // before the physical USB microphone. Try named devices first so a
        // stale default cannot block the streaming session.
        let preferred_name = candidates
            .first()
            .and_then(|device| device.name().ok())
            .unwrap_or_default();
        candidates.sort_by_key(|device| {
            let name = device.name().unwrap_or_default();
            (name != preferred_name, name.eq_ignore_ascii_case("default"))
        });
        // The same ALSA/PipeWire endpoint can be returned twice (once as the
        // configured handle and once during enumeration). Remove duplicates
        // before trying fallbacks so a short candidate list cannot crowd out
        // the physical microphone.
        let mut seen_names = HashSet::new();
        candidates.retain(|device| {
            let name = device.name().unwrap_or_default();
            seen_names.insert(name)
        });

        if candidates.is_empty() {
            self.is_recording.store(false, Ordering::SeqCst);
            return Err("No audio input device available".to_string());
        }

        let mut last_error = None;
        for device in candidates {
            let name = device.name().ok();
            let device_name = name.as_deref().unwrap_or("unknown device");
            let supported = match device.default_input_config() {
                Ok(config) => config,
                Err(error) => {
                    warn!("Failed to query audio device '{}': {}", device_name, error);
                    last_error = Some(error.to_string());
                    continue;
                }
            };
            let input_rate = supported.sample_rate().0;
            let input_channels = supported.channels() as usize;
            let stream_config = supported.config();

            let stream = match supported.sample_format() {
                SampleFormat::I16 => build_stream::<i16>(
                    &device,
                    &stream_config,
                    input_rate,
                    input_channels,
                    gain,
                    Arc::clone(&frames),
                    Arc::clone(&is_recording),
                    Arc::clone(&audio_level_callback),
                    Arc::clone(&audio_data_callback),
                    |sample| sample,
                ),
                SampleFormat::F32 => build_stream::<f32>(
                    &device,
                    &stream_config,
                    input_rate,
                    input_channels,
                    gain,
                    Arc::clone(&frames),
                    Arc::clone(&is_recording),
                    Arc::clone(&audio_level_callback),
                    Arc::clone(&audio_data_callback),
                    |sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16,
                ),
                SampleFormat::U16 => build_stream::<u16>(
                    &device,
                    &stream_config,
                    input_rate,
                    input_channels,
                    gain,
                    Arc::clone(&frames),
                    Arc::clone(&is_recording),
                    Arc::clone(&audio_level_callback),
                    Arc::clone(&audio_data_callback),
                    |sample| (sample as i32 - 32768) as i16,
                ),
                format => Err(format!("unsupported sample format {:?}", format)),
            };

            let stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    warn!("Failed to open audio device '{}': {}", device_name, error);
                    last_error = Some(error);
                    continue;
                }
            };

            if let Err(error) = stream.play() {
                warn!("Failed to start audio device '{}': {}", device_name, error);
                last_error = Some(error.to_string());
                continue;
            }

            // Some ALSA devices report a successful stream but immediately
            // fail in their callback (for example a stale PCH timestamp).
            // Give the stream a short moment to produce frames before
            // accepting it, then continue with the next input device.
            std::thread::sleep(Duration::from_millis(150));
            // On resume, the saved samples must not count as microphone input.
            if frames.lock().len() == saved_samples {
                warn!(
                    "Audio device '{}' produced no frames; trying fallback",
                    device_name
                );
                self.is_recording.store(false, Ordering::SeqCst);
                drop(stream);
                self.frames.lock().truncate(saved_samples);
                self.is_recording.store(true, Ordering::SeqCst);
                last_error = Some(format!("device '{}' produced no frames", device_name));
                continue;
            }

            info!("Recording started successfully on '{}'", device_name);
            self.opened_input_name = name;
            self.device = Some(device);
            self.stream.lock().0 = Some(stream);
            return Ok(());
        }

        self.is_recording.store(false, Ordering::SeqCst);
        Err(format!(
            "Failed to start any audio input device{}",
            last_error
                .map(|error| format!(": {}", error))
                .unwrap_or_default()
        ))
    }

    /// Stop recording and return WAV data
    ///
    /// # Returns
    /// WAV-encoded audio data as bytes
    ///
    /// # Notes
    /// Returns empty Vec if no audio was captured
    pub fn stop(&mut self) -> Vec<u8> {
        if !self.is_recording.load(Ordering::SeqCst) {
            debug!("Not recording, stop is no-op");
            return Vec::new();
        }

        self.is_recording.store(false, Ordering::SeqCst);

        if let Some(stream) = self.stream.lock().0.take() {
            drop(stream);
        }

        let wav_data = self.wav_snapshot();
        if !wav_data.is_empty() {
            info!("Recording stopped: {} bytes WAV", wav_data.len());
        }
        wav_data
    }

    pub fn wav_snapshot(&self) -> Vec<u8> {
        let frames = self.frames.lock().clone();
        if frames.is_empty() {
            return Vec::new();
        }
        self.create_wav(&frames)
    }

    pub fn sample_count(&self) -> usize {
        self.frames.lock().len()
    }

    pub fn wav_preview_since(
        &self,
        start: usize,
        last_decoded: usize,
        first_preview: bool,
    ) -> Option<PreviewAudio> {
        let frames = self.frames.lock();
        let available = frames.get(start..)?;
        let step = if first_preview {
            FIRST_PREVIEW_STEP
        } else {
            PREVIEW_STEP
        };
        if frames.len().saturating_sub(last_decoded.max(start)) < step {
            return None;
        }
        // Release the capture buffer before scanning for speech boundaries.
        let samples = available[..available.len().min(PREVIEW_LIMIT)].to_vec();
        drop(frames);
        let (count, complete) = preview_boundary(&samples);
        Some(PreviewAudio {
            end: start + count,
            complete,
            wav: if preview_has_signal(&samples[..count]) {
                self.create_wav(&samples[..count])
            } else {
                Vec::new()
            },
        })
    }

    /// Check if currently recording
    ///
    /// # Returns
    /// true if recording, false otherwise
    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::SeqCst)
    }

    /// Get current audio level (0.0 - 1.0)
    pub fn get_audio_level(&self) -> f32 {
        let frames = self.frames.lock();
        let start = frames.len().saturating_sub(WINDOW_SAMPLES);
        meter::level(&frames[start..])
    }

    pub fn voice_activity(&self) -> VoiceActivity {
        let mut window = [0; WINDOW_SAMPLES];
        let length = {
            let frames = self.frames.lock();
            let tail = &frames[frames.len().saturating_sub(WINDOW_SAMPLES)..];
            window[..tail.len()].copy_from_slice(tail);
            tail.len()
        };
        // Release the audio capture lock before running pitch estimation.
        VoiceActivity::from_pcm(&window[..length])
    }

    /// Get raw PCM data (for streaming)
    pub fn get_pcm_data(&self) -> Vec<u8> {
        let frames = self.frames.lock();
        let mut data = Vec::with_capacity(frames.len() * 2);
        for sample in frames.iter() {
            data.extend_from_slice(&sample.to_le_bytes());
        }
        data
    }

    /// Create WAV file from samples
    fn create_wav(&self, samples: &[i16]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let file_len = 36 + data_len;

        let mut buffer = Vec::with_capacity(44 + data_len);

        // RIFF header
        buffer.extend_from_slice(b"RIFF");
        buffer.extend_from_slice(&(file_len as u32).to_le_bytes());
        buffer.extend_from_slice(b"WAVE");

        // fmt subchunk
        buffer.extend_from_slice(b"fmt ");
        buffer.extend_from_slice(&16u32.to_le_bytes()); // Subchunk1Size
        buffer.extend_from_slice(&1u16.to_le_bytes()); // AudioFormat (PCM)
        buffer.extend_from_slice(&CHANNELS.to_le_bytes()); // NumChannels
        buffer.extend_from_slice(&SAMPLE_RATE.to_le_bytes()); // SampleRate
        let byte_rate = SAMPLE_RATE * CHANNELS as u32 * SAMPLE_WIDTH as u32;
        buffer.extend_from_slice(&byte_rate.to_le_bytes()); // ByteRate
        let block_align = CHANNELS * SAMPLE_WIDTH;
        buffer.extend_from_slice(&block_align.to_le_bytes()); // BlockAlign
        let bits_per_sample = SAMPLE_WIDTH * 8;
        buffer.extend_from_slice(&bits_per_sample.to_le_bytes()); // BitsPerSample

        // data subchunk
        buffer.extend_from_slice(b"data");
        buffer.extend_from_slice(&(data_len as u32).to_le_bytes());

        for sample in samples {
            buffer.extend_from_slice(&sample.to_le_bytes());
        }

        buffer
    }
}

/// Build a capture stream for one of cpal's supported sample formats. Audio
/// devices commonly expose 48 kHz (and sometimes float samples), while ASR
/// engines expect 16 kHz mono PCM. The callback converts, mixes down and
/// performs a lightweight resampling before storing samples.
fn build_stream<T>(
    device: &Device,
    config: &StreamConfig,
    input_rate: u32,
    input_channels: usize,
    gain: f64,
    frames: Arc<Mutex<Vec<i16>>>,
    is_recording: Arc<AtomicBool>,
    audio_level_callback: Arc<Mutex<Option<Box<dyn Fn(f32) + Send + Sync>>>>,
    audio_data_callback: Arc<Mutex<Option<Box<dyn Fn(&[u8]) + Send + Sync>>>>,
    converter: fn(T) -> i16,
) -> Result<Stream, String>
where
    T: cpal::SizedSample + 'static,
{
    if input_channels == 0 || input_rate == 0 {
        return Err("invalid audio device format".to_string());
    }

    let resampler = Arc::new(Mutex::new(AreaResampler::new(input_rate)));
    let stream = device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                if !is_recording.load(Ordering::SeqCst) {
                    return;
                }

                let mut resampler = resampler.lock();
                let mut processed = Vec::new();

                for frame in data.chunks(input_channels) {
                    if frame.is_empty() {
                        continue;
                    }

                    let sum: i64 = frame.iter().map(|&sample| converter(sample) as i64).sum();
                    let mono = sum as f64 / frame.len() as f64;
                    resampler.push(mono, &mut processed, gain);
                }
                if processed.is_empty() {
                    return;
                }

                let sum: i64 = processed.iter().map(|&sample| (sample as i64).abs()).sum();
                let average = sum as f32 / processed.len() as f32;
                let level = (average / 32768.0).min(1.0);
                if let Some(ref callback) = *audio_level_callback.lock() {
                    callback(level);
                }
                if let Some(ref callback) = *audio_data_callback.lock() {
                    let mut pcm = Vec::with_capacity(processed.len() * 2);
                    for sample in &processed {
                        pcm.extend_from_slice(&sample.to_le_bytes());
                    }
                    callback(&pcm);
                }
                frames.lock().extend_from_slice(&processed);
            },
            move |error| {
                error!("Audio stream error: {:?}", error);
            },
            None,
        )
        .map_err(|error| error.to_string())?;

    Ok(stream)
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        self.is_recording.store(false, Ordering::SeqCst);
        self.stream.lock().0.take();
    }
}

#[cfg(test)]
mod tests {
    use super::{AreaResampler, AudioRecorder, InputDevices, SAMPLE_RATE};

    #[test]
    fn disconnected_named_input_falls_back_to_default_even_if_its_old_index_exists() {
        let mut inputs = InputDevices {
            available: vec![(0, "USB mic".into()), (1, "Built-in mic".into())],
            default_name: Some("Built-in mic".into()),
        };
        assert_eq!(inputs.configured_name(Some(0), Some("Headset")), None);
        assert_eq!(
            inputs.preferred_name(Some(0), Some("Headset")),
            Some("Built-in mic")
        );
        assert_eq!(inputs.preferred_name(Some(0), None), Some("USB mic"));

        inputs.available.push((2, "Headset".into()));
        assert_eq!(
            inputs.preferred_name(Some(0), Some("Headset")),
            Some("Headset")
        );
        assert_eq!(
            InputDevices::default().preferred_name(None, Some("Headset")),
            None
        );
    }

    #[test]
    fn resumes_saved_pcm_without_losing_samples() {
        let mut wav = [0_u8; 48];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[8..16].copy_from_slice(b"WAVEfmt ");
        wav[20..24].copy_from_slice(&[1, 0, 1, 0]);
        wav[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
        wav[34..40].copy_from_slice(b"\x10\x00data");
        wav[40..44].copy_from_slice(&4_u32.to_le_bytes());
        wav[44..46].copy_from_slice(&(-194_i16).to_le_bytes());
        wav[46..48].copy_from_slice(&364_i16.to_le_bytes());
        assert_eq!(AudioRecorder::recorded_frames(&wav).unwrap(), [-194, 364]);
        assert!(AudioRecorder::recorded_frames(&wav[..47]).is_err());
    }

    #[test]
    fn preview_redecodes_the_whole_phrase_and_finishes_at_a_pause() {
        let recorder = AudioRecorder::new_with_name(None, None, 1.0).unwrap();
        let second = SAMPLE_RATE as usize;
        recorder
            .frames
            .lock()
            .resize(super::FIRST_PREVIEW_STEP - 1, 100);
        assert!(recorder.wav_preview_since(0, 0, true).is_none());
        recorder
            .frames
            .lock()
            .resize(super::FIRST_PREVIEW_STEP, 100);
        assert!(recorder.wav_preview_since(0, 0, false).is_none());
        let partial = recorder.wav_preview_since(0, 0, true).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.end, super::FIRST_PREVIEW_STEP);
        assert!(recorder.wav_preview_since(0, partial.end, false).is_none());
        recorder.frames.lock().resize(second, 100);
        assert!(recorder.wav_preview_since(0, partial.end, false).is_none());
        recorder.frames.lock().resize(2 * second, 200);
        recorder.frames.lock().resize(3 * second, 0);
        recorder.frames.lock().resize(5 * second, 500);
        let first = recorder.wav_preview_since(0, partial.end, false).unwrap();
        let next = recorder
            .wav_preview_since(first.end, first.end, false)
            .unwrap();
        let full = recorder.wav_snapshot();
        assert!(first.complete);
        assert_eq!(first.end, 2 * second + second * 2 / 5);
        assert_eq!(&first.wav[44..], &full[44..44 + 2 * first.end]);
        assert!(!next.complete);
        assert_eq!(&next.wav[44..], &full[44 + 2 * first.end..]);
    }

    #[test]
    fn room_tone_is_skipped_only_in_preview() {
        let recorder = AudioRecorder::new_with_name(None, None, 1.0).unwrap();
        recorder.frames.lock().resize(super::PREVIEW_STEP, 3);
        let quiet = recorder.wav_preview_since(0, 0, true).unwrap();
        assert!(quiet.wav.is_empty());
        recorder.frames.lock().resize(super::PREVIEW_STEP * 2, 20);
        let start = quiet.next_start_after_quiet(0);
        let voice = recorder.wav_preview_since(start, quiet.end, true).unwrap();
        assert!(!voice.wav.is_empty());
        assert_eq!(&voice.wav[44..46], &3i16.to_le_bytes());
        let full = recorder.wav_snapshot();
        assert_eq!(full.len(), 44 + 4 * super::PREVIEW_STEP);
        assert_eq!(&full[44..46], &3i16.to_le_bytes());
    }

    #[test]
    fn long_preview_uses_a_quiet_boundary_within_the_time_limit() {
        let second = SAMPLE_RATE as usize;
        let mut samples = vec![4000; 13 * second];
        let frame = second / 50;
        samples[11 * second..11 * second + frame].fill(5);
        assert_eq!(
            super::preview_boundary(&samples),
            (11 * second + frame / 2, true)
        );
    }

    #[test]
    fn resamples_48khz_to_16khz_without_changing_a_constant_signal() {
        let mut resampler = AreaResampler::new(48_000);
        let mut output = Vec::new();
        for _ in 0..48_000 {
            resampler.push(1_000.0, &mut output, 1.0);
        }

        assert_eq!(output.len(), 16_000);
        assert!(output.iter().all(|sample| *sample == 1_000));
    }

    #[test]
    fn keeps_resampling_state_continuous_across_input_chunks() {
        let input = (0..44_100)
            .map(|index| ((index % 997) as i16) - 498)
            .collect::<Vec<_>>();
        let mut whole = AreaResampler::new(44_100);
        let mut chunked = AreaResampler::new(44_100);
        let mut whole_output = Vec::new();
        let mut chunked_output = Vec::new();

        for sample in &input {
            whole.push(*sample as f64, &mut whole_output, 1.0);
        }
        for chunk in input.chunks(317) {
            for sample in chunk {
                chunked.push(*sample as f64, &mut chunked_output, 1.0);
            }
        }

        assert_eq!(whole_output.len(), 16_000);
        assert_eq!(chunked_output, whole_output);
    }
}

// Make AudioRecorder thread-safe by using Arc<Mutex<>> internally
unsafe impl Send for AudioRecorder {}
unsafe impl Sync for AudioRecorder {}

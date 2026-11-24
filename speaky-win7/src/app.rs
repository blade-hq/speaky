use speaky_core::{
    audio::{AudioRecorder, PreviewAudio, VoiceActivity},
    config::Config,
    dictation_target::{DictationTarget, Foreground},
    drafts, engines, history,
    session::{is_silent_recording, LivePreview, SILENT_INPUT},
    terms,
};
use std::{
    fs,
    path::PathBuf,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const UPDATE: u32 = WM_APP + 1;
pub type Rows = Vec<(history::Target, String, String)>;

pub enum Event {
    Started(Sender<()>),
    History(Rows),
    EditFailed(String, String),
    Aux(String),
    Meter(VoiceActivity, u64, String),
    Saved(Rows),
    Done(String, Rows),
    TranscriptionFailed(PathBuf, String, Rows),
    Unsaved(Pending, String),
    Settings(Result<(Config, String), String>),
    Devices(Vec<(u32, String)>),
    Dictionary(Result<(String, Vec<String>), String>),
}

#[derive(Clone)]
pub struct Updates {
    pub window: usize,
    pub events: Sender<Event>,
}

impl Updates {
    pub fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            unsafe { PostMessageW(self.window as _, UPDATE, 0, 0) };
        }
    }
}

pub struct Pending {
    path: PathBuf,
    wav: Vec<u8>,
    target: Option<DictationTarget>,
    config: Config,
}

pub fn rows() -> Rows {
    let archive = drafts::list();
    let mut rows = archive
        .iter()
        .map(|entry| {
            let text = match &entry.state {
                drafts::State::Transcribed { text } => text.clone(),
                _ => String::new(),
            };
            let label: String = text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(36)
                .collect();
            let label = if label.is_empty() {
                "未转写录音".into()
            } else {
                label
            };
            (
                history::Target::Draft(entry.path.clone()),
                format!(
                    "{:02}:{:02}  {label}",
                    entry.duration_seconds / 60,
                    entry.duration_seconds % 60
                ),
                text,
            )
        })
        .collect::<Rows>();
    rows.extend(
        history::standalone(&archive)
            .into_iter()
            .map(|(target, text)| (target, text.chars().take(36).collect(), text)),
    );
    rows
}

pub fn start(config: Config, old: Option<PathBuf>, external: bool, updates: Updates) {
    let target = if external {
        match DictationTarget::capture_foreground() {
            Foreground::External(target) => target,
            Foreground::Speaky => None,
        }
    } else {
        None
    };
    thread::spawn(move || {
        if let Err(error) = record(config, old, target, &updates) {
            updates.send(Event::Done(error, rows()));
        }
    });
}

fn record(
    config: Config,
    old: Option<PathBuf>,
    target: Option<DictationTarget>,
    updates: &Updates,
) -> Result<(), String> {
    let asr = &config.core.asr;
    let mut recorder = AudioRecorder::new_with_name(
        asr.audio_device,
        asr.audio_device_name.as_deref(),
        asr.audio_gain,
    )?;
    let path = if let Some(path) = old {
        let wav = fs::read(&path).map_err(|error| error.to_string())?;
        recorder.resume_wav(&wav)?;
        path
    } else {
        recorder.start()?;
        drafts::new_path()
    };
    let (stop, stopped) = mpsc::channel();
    let mut preview = LivePreview::new(recorder.sample_count());
    let mut decoding: Option<Receiver<_>> = None;
    let mut checkpoint = Instant::now();
    let mut checkpoint_error = None;
    updates.send(Event::Started(stop));
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(Duration::from_millis(65)) {
        if checkpoint.elapsed() >= Duration::from_secs(2) {
            let wav = recorder.wav_snapshot();
            if !wav.is_empty() {
                checkpoint_error = drafts::save(&path, &wav)
                    .err()
                    .map(|error| format!("录音暂存失败：{error}"));
            }
            checkpoint = Instant::now();
        }
        if let Some(result) = decoding.as_ref().and_then(|rx| rx.try_recv().ok()) {
            let (end, complete, result) = result;
            decoding = None;
            match preview.accept(end, complete, result) {
                Ok(()) => {
                    preview.error = None;
                    preview.retry_after = None;
                    if let Some(target) = &target {
                        target.preview(&preview.text);
                    }
                }
                Err(error) => {
                    preview.error = Some(format!("实时转写暂时不可用：{error}"));
                    preview.retry_after = Some(Instant::now() + Duration::from_secs(4));
                }
            }
        }
        if decoding.is_none()
            && preview
                .retry_after
                .is_none_or(|time| Instant::now() >= time)
        {
            if let Some(chunk) = recorder.wav_preview_since(
                preview.phrase_start,
                preview.last_decoded,
                preview.text.is_empty(),
            ) {
                if chunk.wav.is_empty() {
                    preview.phrase_start = chunk.next_start_after_quiet(preview.phrase_start);
                    preview.last_decoded = chunk.end;
                } else {
                    let (tx, rx) = mpsc::channel();
                    decoding = Some(rx);
                    let config = config.clone();
                    thread::spawn(move || {
                        let PreviewAudio { end, complete, wav } = chunk;
                        let result = transcribe(&config, &wav, engines::TranscriptionPass::Preview);
                        let _ = tx.send((end, complete, result.map(|(text, _)| text)));
                    });
                }
            }
        }
        let caption = checkpoint_error
            .as_ref()
            .or(preview.error.as_ref())
            .map(String::as_str)
            .unwrap_or(&preview.text);
        updates.send(Event::Meter(
            recorder.voice_activity(),
            (recorder.sample_count() / 16000) as u64,
            caption.to_owned(),
        ));
    }
    if let Some(target) = &target {
        target.stop_preview();
    }
    let wav = recorder.stop();
    if wav.is_empty() {
        return Err("没有收到音频，请检查麦克风。".into());
    }
    finish(
        Pending {
            path,
            wav,
            target,
            config,
        },
        updates,
    );
    Ok(())
}

pub fn retry(pending: Pending, updates: Updates) {
    thread::spawn(move || finish(pending, &updates));
}

fn finish(pending: Pending, updates: &Updates) {
    if let Err(error) = drafts::save(&pending.path, &pending.wav) {
        updates.send(Event::Unsaved(
            pending,
            format!("保存失败：{error}。请重试保存。"),
        ));
        return;
    }
    updates.send(Event::Saved(rows()));
    if is_silent_recording(&pending.wav) {
        updates.send(Event::Done(format!("录音已保存。{SILENT_INPUT}"), rows()));
    } else {
        publish_transcript(
            &pending.config,
            pending.path,
            &pending.wav,
            pending.target,
            updates,
        );
    }
}

pub fn retranscribe(config: Config, path: PathBuf, updates: Updates) {
    thread::spawn(move || match fs::read(&path) {
        Ok(wav) => publish_transcript(&config, path, &wav, None, &updates),
        Err(error) => updates.send(Event::TranscriptionFailed(path, error.to_string(), rows())),
    });
}

fn publish_transcript(
    config: &Config,
    path: PathBuf,
    wav: &[u8],
    target: Option<DictationTarget>,
    updates: &Updates,
) {
    let result =
        transcribe(config, wav, engines::TranscriptionPass::Final).and_then(|(text, engine)| {
            history::record(&path, &text, &engine).map_err(|error| error.to_string())?;
            Ok(text)
        });
    match result {
        Ok(text) => {
            let status = match target.and_then(|target| target.insert(&text).err()) {
                Some(error) => format!("转写已保存，可在历史中复制。{error}"),
                None => "转写已保存。".into(),
            };
            updates.send(Event::Done(status, rows()));
        }
        Err(error) => updates.send(Event::TranscriptionFailed(
            path,
            format!("录音已保存。转写失败：{error}"),
            rows(),
        )),
    }
}

fn transcribe(
    config: &Config,
    wav: &[u8],
    pass: engines::TranscriptionPass,
) -> Result<(String, String), String> {
    let engine = engines::recognition_engine(config)?;
    let text = terms::transcribe(engine.as_ref(), wav, &config.core.asr.language, pass)
        .map_err(|error| error.to_string())?;
    Ok((text, engine.name().to_owned()))
}

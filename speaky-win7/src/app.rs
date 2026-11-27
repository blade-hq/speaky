use speaky_core::{
    audio::{PreviewAudio, VoiceActivity},
    config::Config,
    dictation_target::{self, DictationTarget, Foreground, StopFocus},
    drafts, engines, history, pipeline,
    session::{is_silent_recording, SILENT_INPUT},
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
    Imported(Result<PathBuf, String>),
    OrganizerTest(Result<String, String>),
    MicrophoneTest(Result<String, String>),
    RecognitionTest(Result<String, String>),
    OrganizedHistory(PathBuf, Result<(), String>),
    Started(Sender<StopRequest>),
    ClipboardNotice,
    History(Rows),
    EditFailed(String, String),
    Aux(String),
    Meter(VoiceActivity, u64, String),
    Saved(Rows),
    Done(String, Rows),
    TranscriptionFailed(PathBuf, String, Rows),
    Unsaved(Pending, String),
    Settings(Result<(Config, String), String>),
    ModelCatalog(u64, Result<engines::ModelCatalog, String>),
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
    external: bool,
    config: Config,
    cancelled: bool,
}

pub struct StopRequest {
    pub cancelled: bool,
    focus: StopFocus,
    startup_target: Option<Result<DictationTarget, String>>,
}
impl StopRequest {
    pub fn new(cancelled: bool) -> Self {
        Self {
            cancelled,
            focus: StopFocus::capture(),
            startup_target: None,
        }
    }

    pub fn during_startup() -> Self {
        Self {
            startup_target: Some(DictationTarget::capture_at_stop(None)),
            ..Self::new(false)
        }
    }
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
        if let Err(error) = record(config, old, target, external, &updates) {
            updates.send(Event::Done(error, rows()));
        }
    });
}

fn record(
    config: Config,
    old: Option<PathBuf>,
    target: Option<DictationTarget>,
    external: bool,
    updates: &Updates,
) -> Result<(), String> {
    let resume = old.is_some();
    let path = old.unwrap_or_else(drafts::new_path);
    let (mut recorder, mut preview) = speaky_core::session::open_recording(&config, &path, resume)?;
    let (stop, stopped) = mpsc::channel();
    let mut decoding: Option<Receiver<_>> = None;
    let mut checkpoint = Instant::now();
    let mut checkpoint_error = None;
    updates.send(Event::Started(stop));
    let request = loop {
        match stopped.recv_timeout(Duration::from_millis(65)) {
            Ok(request) => break request,
            Err(RecvTimeoutError::Disconnected) => break StopRequest::new(true),
            Err(RecvTimeoutError::Timeout) => {}
        }
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
        if !engines::uses_gemini_asr(&config)
            && decoding.is_none()
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
    };
    if let Some(target) = &target {
        target.stop_preview();
    }
    let wav = recorder.stop();
    let target = if request.cancelled {
        if let Some(target) = &target {
            if let Err(error) = target.cancel_preview() {
                updates.send(Event::Aux(error));
            }
        }
        None
    } else if external {
        request
            .startup_target
            .unwrap_or_else(|| DictationTarget::capture_stopped(request.focus, target.as_ref()))
            .ok()
    } else {
        None
    };
    if wav.is_empty() {
        return Err("没有收到音频，请检查麦克风。".into());
    }
    finish(
        Pending {
            path,
            wav,
            target,
            external,
            config,
            cancelled: request.cancelled,
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
    if pending.cancelled {
        updates.send(Event::Done(
            "已取消转写，原始录音已保存在历史中。".into(),
            rows(),
        ));
    } else if is_silent_recording(&pending.wav) {
        updates.send(Event::Done(format!("录音已保存。{SILENT_INPUT}"), rows()));
    } else {
        publish_transcript(
            &pending.config,
            pending.path,
            &pending.wav,
            pending.target,
            pending.external,
            updates,
        );
    }
}

pub fn retranscribe(config: Config, path: PathBuf, updates: Updates) {
    thread::spawn(move || match fs::read(&path) {
        Ok(wav) => publish_transcript(&config, path, &wav, None, false, &updates),
        Err(error) => updates.send(Event::TranscriptionFailed(path, error.to_string(), rows())),
    });
}

fn publish_transcript(
    config: &Config,
    path: PathBuf,
    _wav: &[u8],
    target: Option<DictationTarget>,
    external: bool,
    updates: &Updates,
) {
    let result = pipeline::transcribe_saved(config, &path);
    match result {
        Ok(transcript) => {
            let text = transcript.text;
            let delivery = if external {
                target
                    .ok_or_else(|| "停止时未找到可写输入框。".into())
                    .and_then(|target| target.deliver(&text))
                    .map(|_| true)
            } else {
                Ok(false)
            };
            let status = match delivery {
                Ok(true) => "已输入到当前输入框。".into(),
                Ok(false) => "转写已保存。".into(),
                Err(error) => match dictation_target::copy_fallback(&text) {
                    Ok(()) => {
                        updates.send(Event::ClipboardNotice);
                        format!("{error} 语音识别内容已保存到剪贴板，请手动粘贴")
                    }
                    Err(clipboard) => format!("{error} {clipboard}"),
                },
            };
            updates.send(Event::Done(
                format!("{} {status}", transcript.notice),
                rows(),
            ));
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

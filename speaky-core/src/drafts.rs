use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub duration_seconds: u64,
    pub state: State,
    pub recognized_at: Option<SystemTime>,
    pub recognizer: Option<String>,
    pub organized: Option<Organized>,
    pub comparisons: Vec<Comparison>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Comparison {
    pub model: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Organized {
    pub source: String,
    pub text: String,
    pub base_url: String,
    pub model: String,
    pub profile: Option<crate::polish::Profile>,
}

impl Organized {
    pub fn model_settings(&self) -> crate::config::LlmOpenAIConfig {
        crate::config::LlmOpenAIConfig {
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum State {
    #[default]
    Draft,
    Cancelled,
    Transcribed {
        text: String,
    },
}

#[derive(Default, Deserialize, Serialize)]
struct Transcript {
    #[serde(flatten)]
    state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recognized_at: Option<SystemTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recognizer: Option<String>,
}

fn recorded_model_name(value: String) -> String {
    if let Some((label, model)) = value.split_once(" / ") {
        if let Some(f) = crate::config::Framework::ALL
            .iter()
            .find(|f| f.label == label)
        {
            // Older recordings saved only a framework ID, without the serving endpoint
            // or weight name. Today's local preset cannot identify those past weights.
            if model == f.id {
                return format!("{}（旧记录未保存模型名）", f.label);
            }
            return model.to_owned();
        }
    }
    value
}

pub fn directory() -> PathBuf {
    crate::config::Config::config_dir().join("drafts")
}

pub fn new_path() -> PathBuf {
    directory().join(format!("{}.wav", uuid::Uuid::new_v4()))
}

pub fn save(path: &Path, audio: &[u8]) -> io::Result<()> {
    if audio.is_empty() {
        return Ok(());
    }
    write_atomically(path, audio)
}

pub fn set_state(path: &Path, state: State) -> io::Result<()> {
    let metadata = serde_json::to_vec(&state).map_err(io::Error::other)?;
    write_atomically(&path.with_extension("json"), &metadata)
}

fn read_transcript(path: &Path) -> io::Result<Transcript> {
    let sidecar = path.with_extension("json");
    let mut transcript: Transcript =
        serde_json::from_slice(&fs::read(&sidecar)?).map_err(io::Error::other)?;
    if transcript.recognized_at.is_none() {
        transcript.recognized_at = fs::metadata(sidecar).and_then(|m| m.modified()).ok();
    }
    Ok(transcript)
}

fn save_transcript(path: &Path, transcript: &Transcript) -> io::Result<()> {
    let bytes = serde_json::to_vec(transcript).map_err(io::Error::other)?;
    write_atomically(&path.with_extension("json"), &bytes)
}

pub fn record(
    path: &Path,
    text: &str,
    when: SystemTime,
    recognizer: Option<&str>,
) -> io::Result<()> {
    let previous = match read_transcript(path) {
        Ok(old) => Some(old),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let original_text = previous.and_then(|old| match old.state {
        State::Transcribed { text } => Some(old.original_text.unwrap_or(text)),
        _ => None,
    });
    let transcript = Transcript {
        state: State::Transcribed { text: text.into() },
        recognized_at: Some(when),
        recognizer: recognizer.map(str::to_owned),
        original_text,
    };
    save_transcript(path, &transcript)
}

pub fn edit(path: &Path, expected: &str, edited: &str) -> io::Result<()> {
    let mut transcript = read_transcript(path)?;
    if !matches!(&transcript.state, State::Transcribed { text } if text == expected) {
        return Err(io::Error::other("转写内容已变化，请重新打开后修改。"));
    }
    transcript
        .original_text
        .get_or_insert_with(|| expected.into());
    transcript.state = State::Transcribed {
        text: edited.into(),
    };
    save_transcript(path, &transcript)
}

pub fn save_organized(path: &Path, result: &Organized) -> io::Result<()> {
    let metadata = serde_json::to_vec(result).map_err(io::Error::other)?;
    write_atomically(&path.with_extension("polish.json"), &metadata)
}

pub fn save_comparison(path: &Path, model: &str, text: &str) -> io::Result<()> {
    let comparison_path = path.with_extension("comparisons.json");
    let lock_path = path.with_extension("comparisons.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(lock_path)?;
    lock.lock()?;
    let mut comparisons = read_comparisons(path)?;
    if let Some(existing) = comparisons.iter_mut().find(|entry| entry.model == model) {
        existing.text = text.to_owned();
    } else {
        comparisons.push(Comparison {
            model: model.to_owned(),
            text: text.to_owned(),
        });
    }
    let bytes = serde_json::to_vec(&comparisons).map_err(io::Error::other)?;
    write_atomically(&comparison_path, &bytes)
}

fn read_comparisons(path: &Path) -> io::Result<Vec<Comparison>> {
    match fs::read(path.with_extension("comparisons.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn list() -> Vec<Entry> {
    list_in(&directory())
}

pub(super) fn list_in(directory: &Path) -> Vec<Entry> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut drafts = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()?.to_str()? != "wav" {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            (metadata.is_file() && metadata.len() > 44)
                .then(|| (metadata.modified().ok(), path, metadata.len()))
        })
        .collect::<Vec<_>>();
    drafts.sort_unstable_by(|left, right| right.cmp(left));
    drafts
        .into_iter()
        .map(|(modified, path, bytes)| {
            let transcript = read_transcript(&path).unwrap_or_default();
            let organized = fs::read(path.with_extension("polish.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            let comparisons = read_comparisons(&path).unwrap_or_default();
            Entry {
                path,
                modified,
                duration_seconds: (bytes - 44) / (16_000 * 2),
                state: transcript.state,
                recognized_at: transcript.recognized_at,
                recognizer: transcript.recognizer.map(recorded_model_name),
                organized,
                comparisons,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn older_history_shows_only_model_names_that_were_actually_recorded() {
        assert_eq!(
            super::recorded_model_name("FunASR / funasr".into()),
            "FunASR（旧记录未保存模型名）"
        );
        assert_eq!(
            super::recorded_model_name("FunASR / custom/Paraformer".into()),
            "custom/Paraformer"
        );
        assert_eq!(
            super::recorded_model_name("SenseVoiceSmall".into()),
            "SenseVoiceSmall"
        );
    }
    use super::*;

    #[test]
    fn model_comparisons_keep_each_model_and_replace_only_its_own_result() {
        let folder = std::env::temp_dir().join(format!("speaky-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&folder).unwrap();
        let recording = folder.join("recording.wav");
        save_comparison(&recording, "Qwen3-ASR-0.6B", "第一版").unwrap();
        save_comparison(&recording, "Qwen3-ASR-1.7B", "另一模型").unwrap();
        save_comparison(&recording, "Qwen3-ASR-0.6B", "更新版").unwrap();
        let saved = read_comparisons(&recording).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].model, "Qwen3-ASR-0.6B");
        assert_eq!(saved[0].text, "更新版");
        assert_eq!(saved[1].text, "另一模型");
        assert!(!recording.with_extension("json").exists());
        fs::remove_file(recording.with_extension("comparisons.json")).unwrap();
        fs::remove_file(recording.with_extension("comparisons.lock")).unwrap();
        fs::remove_dir(folder).unwrap();
    }

    #[test]
    fn organized_text_survives_reload_and_preserves_source() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("recording.wav");
        let audio = [1; 48];
        let state = State::Transcribed {
            text: "嗯，周四开会".into(),
        };
        save(&path, &audio).unwrap();
        set_state(&path, state.clone()).unwrap();
        let result = Organized {
            source: "嗯，周四开会".into(),
            text: "周四开会。".into(),
            base_url: "http://127.0.0.1:8000/v1".into(),
            model: "test-model".into(),
            profile: None,
        };
        save_organized(&path, &result).unwrap();
        let archive = list_in(&directory);
        assert_eq!(archive[0].state, state);
        assert_eq!(archive[0].organized.as_ref().unwrap().text, result.text);
        assert_eq!(archive[0].organized.as_ref().unwrap().profile, None);
        assert_eq!(fs::read(&path).unwrap(), audio);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reads_a_saved_builtin_profile() {
        let result: Organized = serde_json::from_str(
            r#"{"source":"原文","text":"整理稿","base_url":"http://localhost/v1","model":"test","profile":"Balanced"}"#,
        )
        .unwrap();
        assert_eq!(result.profile, Some(crate::polish::Profile::Balanced));
    }

    #[test]
    fn a_completed_snapshot_replaces_the_previous_one() {
        use std::io::Read;
        let directory = std::env::temp_dir().join(format!("speaky-{}", uuid::Uuid::new_v4()));
        let path = directory.join("test.wav");
        save(&path, b"first snapshot").unwrap();
        let mut old_reader = fs::File::open(&path).unwrap();
        save(&path, b"second").unwrap();
        save(&path, b"").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let mut old_bytes = Vec::new();
        old_reader.read_to_end(&mut old_bytes).unwrap();
        assert_eq!(old_bytes, b"first snapshot");
        drop(old_reader);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn archive_keeps_cancelled_transcribed_and_legacy_recordings() {
        let directory = std::env::temp_dir().join(format!("speaky-{}", uuid::Uuid::new_v4()));
        for index in 0..7 {
            save(&directory.join(format!("{index}.wav")), &[0; 46]).unwrap();
        }
        let cancelled = directory.join("0.wav");
        let transcribed = directory.join("1.wav");
        save(&transcribed, &[0; 64_044]).unwrap();
        set_state(&cancelled, State::Cancelled).unwrap();
        let completed = State::Transcribed {
            text: "重新转写成功".into(),
        };
        set_state(&transcribed, State::Cancelled).unwrap();
        set_state(&transcribed, completed.clone()).unwrap();
        fs::write(directory.join("2.json"), "{").unwrap();
        fs::write(directory.join("interrupted.wav.tmp"), [0; 46]).unwrap();

        let archive = list_in(&directory);
        assert_eq!(archive.len(), 7);
        for (path, state) in [
            (cancelled.clone(), State::Cancelled),
            (transcribed, completed),
            (directory.join("2.wav"), State::Draft),
            (directory.join("3.wav"), State::Draft),
        ] {
            assert!(archive
                .iter()
                .any(|entry| entry.path == path && entry.state == state));
        }
        assert!(archive
            .iter()
            .any(|entry| entry.path == directory.join("1.wav") && entry.duration_seconds == 2));
        assert_eq!(fs::read(cancelled).unwrap(), [0; 46]);
        fs::remove_dir_all(directory).unwrap();
    }
}

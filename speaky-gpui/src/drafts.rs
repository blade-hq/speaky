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
    pub organized: Option<Organized>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Organized {
    pub source: String,
    pub text: String,
    pub base_url: String,
    pub model: String,
    pub profile: Option<crate::polish::Profile>,
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

fn directory() -> PathBuf {
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

pub fn save_organized(path: &Path, result: &Organized) -> io::Result<()> {
    let metadata = serde_json::to_vec(result).map_err(io::Error::other)?;
    write_atomically(&path.with_extension("polish.json"), &metadata)
}

pub(crate) fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
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

fn list_in(directory: &Path) -> Vec<Entry> {
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
            let state = fs::read(path.with_extension("json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            let organized = fs::read(path.with_extension("polish.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            Entry {
                path,
                modified,
                duration_seconds: (bytes - 44) / (16_000 * 2),
                state,
                organized,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let directory = std::env::temp_dir().join(format!("speaky-{}", uuid::Uuid::new_v4()));
        let path = directory.join("test.wav");
        save(&path, b"first snapshot").unwrap();
        save(&path, b"second").unwrap();
        save(&path, b"").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
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

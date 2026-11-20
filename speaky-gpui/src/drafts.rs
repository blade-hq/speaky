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
            Entry {
                path,
                modified,
                duration_seconds: (bytes - 44) / (16_000 * 2),
                state,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

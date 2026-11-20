use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing draft directory"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("wav.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(audio)?;
    file.sync_all()?;
    fs::rename(temporary, path)
}

pub fn recent() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory()) else {
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
            (metadata.is_file() && metadata.len() > 44).then(|| (metadata.modified().ok(), path))
        })
        .collect::<Vec<_>>();
    drafts.sort_unstable_by(|left, right| right.cmp(left));
    drafts.into_iter().take(5).map(|(_, path)| path).collect()
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
        assert!(!path.with_extension("wav.tmp").exists());
        fs::remove_dir_all(directory).unwrap();
    }
}

use crate::{config::Config, drafts};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_HISTORY_SIZE: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Draft(PathBuf),
    History(String),
}

#[derive(Clone, Serialize, Deserialize)]
struct HistoryItem {
    #[serde(default)]
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    draft: Option<String>,
    text: String,
    timestamp: u64,
    engine: String,
    polished: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_text: Option<String>,
}

fn path() -> PathBuf {
    Config::config_dir().join("history.json")
}

fn read_items(path: &Path) -> io::Result<Vec<HistoryItem>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut items: Vec<HistoryItem> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    // Old files have no IDs. A slot remains stable until the first write, which
    // persists every assigned ID before inserting, moving or dropping rows.
    for (index, item) in items.iter_mut().enumerate() {
        if item.id.is_empty() {
            item.id = format!("legacy-{index}");
        }
    }
    Ok(items)
}

static ITEMS: Lazy<Mutex<Vec<HistoryItem>>> = Lazy::new(|| {
    Mutex::new(read_items(&path()).unwrap_or_else(|e| {
        log::warn!("Could not read Speaky history: {e}");
        Vec::new()
    }))
});

fn lock_records(path: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("无效的历史目录。"))?,
    )?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    file.lock()?;
    Ok(file)
}

fn save_items(path: &Path, items: &[HistoryItem]) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(items).map_err(io::Error::other)?;
    drafts::write_atomically(path, &bytes)
}

fn filename(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::other("录音文件名无效。"))
}

fn archived<'a>(item: &HistoryItem, archive: &'a [drafts::Entry]) -> Option<&'a drafts::Entry> {
    let name = item.draft.as_deref()?;
    archive.iter().find(|entry| {
        entry.path.file_name().is_some_and(|file| file == name)
            && matches!(entry.state, drafts::State::Transcribed { .. })
    })
}

fn draft_result(entry: &drafts::Entry) -> Option<(Target, String)> {
    match &entry.state {
        drafts::State::Transcribed { text } => {
            Some((Target::Draft(entry.path.clone()), text.clone()))
        }
        _ => None,
    }
}

pub fn standalone(archive: &[drafts::Entry]) -> Vec<(Target, String)> {
    ITEMS
        .lock()
        .iter()
        .filter(|item| archived(item, archive).is_none())
        .map(|item| (Target::History(item.id.clone()), item.text.clone()))
        .collect()
}

pub fn latest(archive: &[drafts::Entry]) -> Option<(Target, String)> {
    if !legacy_matches(&ITEMS.lock(), archive).is_empty() {
        let path = path();
        let migrate = || -> io::Result<()> {
            let _guard = lock_records(&path)?;
            let mut items = read_items(&path)?;
            let pairs = legacy_matches(&items, archive);
            if !pairs.is_empty() {
                for (i, name) in pairs {
                    items[i].draft = Some(name);
                }
                save_items(&path, &items)?;
            }
            *ITEMS.lock() = items;
            Ok(())
        };
        if let Err(e) = migrate() {
            log::warn!("Could not link legacy recording history: {e}");
        }
    }
    let items = ITEMS.lock();
    latest_from(&items, archive)
}

// Previous versions wrote the WAV sidecar immediately before appending the
// same transcript to history, where timestamps only had second precision.
fn legacy_matches(items: &[HistoryItem], archive: &[drafts::Entry]) -> Vec<(usize, String)> {
    let mut matches = Vec::new();
    let mut hits = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if item.draft.is_some()
            || !item.id.starts_with("legacy-")
            || item.polished
            || item.original_text.is_some()
        {
            continue;
        }
        for entry in archive {
            let drafts::State::Transcribed { text } = &entry.state else {
                continue;
            };
            if &item.text != text {
                continue;
            }
            let Some(seconds) = entry
                .recognized_at
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
            else {
                continue;
            };
            if item.timestamp != seconds && item.timestamp.checked_sub(1) != Some(seconds) {
                continue;
            }
            let Ok(name) = filename(&entry.path) else {
                continue;
            };
            if items
                .iter()
                .any(|other| other.draft.as_deref() == Some(name))
            {
                continue;
            }
            matches.push((i, name));
            *hits.entry(name).or_insert(0) += 1;
        }
    }
    let mut eligible = Vec::new();
    for chunk in matches.chunk_by(|a, b| a.0 == b.0) {
        if let [(i, name)] = chunk {
            if hits.get(name) == Some(&1) {
                eligible.push((*i, (*name).into()));
            }
        }
    }
    eligible
}

fn latest_from(items: &[HistoryItem], archive: &[drafts::Entry]) -> Option<(Target, String)> {
    let newest = archive
        .iter()
        .filter(|entry| matches!(entry.state, drafts::State::Transcribed { .. }))
        .max_by_key(|entry| entry.recognized_at);
    let Some(item) = items.first() else {
        return newest.and_then(draft_result);
    };
    let linked = archived(item, archive);
    let indexed_at = linked
        .and_then(|entry| entry.recognized_at)
        .or_else(|| UNIX_EPOCH.checked_add(Duration::from_secs(item.timestamp)));
    // The WAV sidecar is authoritative even when writing the secondary history
    // index was interrupted after recognition.
    if let Some(newer) = newest.filter(|entry| entry.recognized_at > indexed_at) {
        return draft_result(newer);
    }
    linked
        .and_then(draft_result)
        .or_else(|| Some((Target::History(item.id.clone()), item.text.clone())))
}

pub fn record(draft: &Path, text: &str, engine: &str) -> io::Result<()> {
    let path = path();
    record_in(&path, &ITEMS, draft, text, engine)
}

fn record_in(
    path: &Path,
    cache: &Mutex<Vec<HistoryItem>>,
    draft: &Path,
    text: &str,
    engine: &str,
) -> io::Result<()> {
    let _guard = lock_records(path)?;
    let name = filename(draft)?;
    let now = SystemTime::now();
    drafts::record(draft, text, now, Some(engine))?;
    let update_index = || -> io::Result<()> {
        let mut items = read_items(path)?;
        let previous = items
            .iter()
            .position(|item| item.draft.as_deref() == Some(name))
            .map(|i| items.remove(i));
        items.insert(
            0,
            HistoryItem {
                id: previous
                    .as_ref()
                    .map(|i| i.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                draft: Some(name.into()),
                text: text.into(),
                engine: engine.into(),
                polished: false,
                timestamp: now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
                original_text: previous.map(|i| i.original_text.unwrap_or(i.text)),
            },
        );
        items.truncate(MAX_HISTORY_SIZE);
        save_items(path, &items)?;
        *cache.lock() = items;
        Ok(())
    };
    if let Err(e) = update_index() {
        log::warn!("Recording transcript was saved; could not update its history index: {e}");
    }
    Ok(())
}

pub fn edit(target: &Target, expected: &str, edited: &str) -> io::Result<()> {
    let path = path();
    edit_in(&path, &ITEMS, target, expected, edited)
}

fn edit_in(
    path: &Path,
    cache: &Mutex<Vec<HistoryItem>>,
    target: &Target,
    expected: &str,
    edited: &str,
) -> io::Result<()> {
    if edited.trim().is_empty() {
        return Err(io::Error::other("转写文本不能为空。"));
    }
    let _guard = lock_records(path)?;
    match target {
        Target::Draft(draft) => {
            let name = filename(draft)?;
            drafts::edit(draft, expected, edited)?;
            let update_index = || -> io::Result<()> {
                let mut items = read_items(path)?;
                if let Some(item) = items.iter_mut().find(|i| i.draft.as_deref() == Some(name)) {
                    item.text = edited.into();
                    item.original_text.get_or_insert_with(|| expected.into());
                    save_items(path, &items)?;
                    *cache.lock() = items;
                }
                Ok(())
            };
            if let Err(e) = update_index() {
                log::warn!("Edited recording was saved; could not update its history index: {e}");
            }
        }
        Target::History(id) => {
            let mut items = read_items(path)?;
            let item = items
                .iter_mut()
                .find(|item| &item.id == id)
                .ok_or_else(|| io::Error::other("找不到这条历史转写。"))?;
            if item.text != expected {
                return Err(io::Error::other("转写内容已变化，请重新打开后修改。"));
            }
            // An already-open editor can outlive migration of its standalone
            // history row in another process. Follow the persisted association.
            let mut saved_recording = false;
            if let Some(name) = item.draft.as_deref() {
                let file = Path::new(name);
                if file.file_name() != Some(file.as_os_str())
                    || file.extension().and_then(|ext| ext.to_str()) != Some("wav")
                {
                    return Err(io::Error::other("录音文件名无效，请刷新历史后重试。"));
                }
                let directory = path
                    .parent()
                    .ok_or_else(|| io::Error::other("无效的历史目录。"))?;
                let draft = directory.join("drafts").join(file);
                if draft.try_exists()? {
                    drafts::edit(&draft, expected, edited)?;
                    saved_recording = true;
                }
            }
            item.original_text.get_or_insert_with(|| expected.into());
            item.text = edited.into();
            if let Err(e) = save_items(path, &items) {
                if !saved_recording {
                    return Err(e);
                }
                log::warn!("Edited recording was saved; could not update its history index: {e}");
            }
            *cache.lock() = items;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("speaky-correction-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(dir.join("drafts")).unwrap();
            Self(dir)
        }
        fn audio(&self, name: &str) -> PathBuf {
            let path = self.0.join("drafts").join(name);
            drafts::save(&path, &[42; 64]).unwrap();
            path
        }
        fn archive(&self) -> Vec<drafts::Entry> {
            drafts::list_in(&self.0.join("drafts"))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn corrections_follow_the_recording_across_reload_and_keep_the_original() {
        let fixture = Fixture::new();
        let path = fixture.0.join("history.json");
        let a = fixture.audio("a.wav");
        let b = fixture.audio("b.wav");
        let cache = Mutex::default();
        record_in(&path, &cache, &a, "red 开发", "model").unwrap();
        record_in(&path, &cache, &b, "red 开发", "model").unwrap();
        let key = Target::Draft(b.clone());
        edit_in(&path, &cache, &key, "red 开发", "Rag 开发").unwrap();
        edit_in(&path, &cache, &key, "Rag 开发", "RAG 开发").unwrap();
        assert!(edit_in(&path, &cache, &key, "red 开发", "stale").is_err());
        let saved = read_items(&path).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].text, "RAG 开发");
        assert_eq!(saved[1].text, "red 开发");
        let meta: serde_json::Value =
            serde_json::from_slice(&fs::read(b.with_extension("json")).unwrap()).unwrap();
        assert_eq!(meta["original_text"], "red 开发");
        assert_eq!(fs::read(&b).unwrap(), [42; 64]);
        assert_eq!(
            latest_from(&saved, &fixture.archive()),
            Some((key, "RAG 开发".into()))
        );
    }

    #[test]
    fn links_legacy_rows_only_when_both_sides_match_uniquely_in_time() {
        let fixture = Fixture::new();
        let path = fixture.0.join("history.json");
        let row =
            serde_json::json!({"text":"red", "timestamp":501, "engine":"model", "polished":false});
        fs::write(&path, serde_json::to_vec(&[&row]).unwrap()).unwrap();
        let one = read_items(&path).unwrap();
        let a = fixture.audio("a.wav");
        let b = fixture.audio("b.wav");
        drafts::record(&a, "red", UNIX_EPOCH + Duration::from_secs(500), None).unwrap();
        drafts::record(&b, "red", UNIX_EPOCH + Duration::from_secs(10), None).unwrap();
        assert_eq!(
            legacy_matches(&one, &fixture.archive()),
            vec![(0, "a.wav".into())]
        );
        fs::write(&path, serde_json::to_vec(&[&row, &row]).unwrap()).unwrap();
        let two = read_items(&path).unwrap();
        assert!(legacy_matches(&two, &fixture.archive()).is_empty());
        drafts::record(&b, "red", UNIX_EPOCH + Duration::from_secs(501), None).unwrap();
        assert!(legacy_matches(&one, &fixture.archive()).is_empty());
        let cache = Mutex::default();
        edit_in(
            &path,
            &cache,
            &Target::History(two[1].id.clone()),
            "red",
            "RAG",
        )
        .unwrap();
        let saved = read_items(&path).unwrap();
        assert_eq!(saved[0].text, "red");
        assert_eq!(saved[1].text, "RAG");
        assert_eq!(saved[1].id, two[1].id);
    }

    #[test]
    fn follows_new_recording_link_and_keeps_damaged_sidecars_on_retry() {
        let fixture = Fixture::new();
        let path = fixture.0.join("history.json");
        let draft = fixture.audio("linked.wav");
        let cache = Mutex::default();
        record_in(&path, &cache, &draft, "red", "model").unwrap();
        let old_editor = Target::History(read_items(&path).unwrap()[0].id.clone());
        edit_in(&path, &cache, &old_editor, "red", "RAG").unwrap();
        assert_eq!(
            draft_result(&fixture.archive()[0]),
            Some((Target::Draft(draft.clone()), "RAG".into()))
        );
        let sidecar = draft.with_extension("json");
        fs::write(&sidecar, b"broken").unwrap();
        assert!(record_in(&path, &cache, &draft, "replacement", "model").is_err());
        assert_eq!(fs::read(&sidecar).unwrap(), b"broken");
        assert_eq!(read_items(&path).unwrap()[0].text, "RAG");
    }

    #[test]
    fn saved_draft_remains_editable_if_the_history_index_is_unreadable() {
        let fixture = Fixture::new();
        let path = fixture.0.join("history.json");
        let a = fixture.audio("a.wav");
        fs::write(&path, b"bad json").unwrap();
        let cache = Mutex::default();
        record_in(&path, &cache, &a, "red", "model").unwrap();
        let target = Target::Draft(a);
        edit_in(&path, &cache, &target, "red", "RAG").unwrap();
        assert_eq!(
            latest_from(&[], &fixture.archive()),
            Some((target, "RAG".into()))
        );
        assert_eq!(fs::read(&path).unwrap(), b"bad json");
    }
}

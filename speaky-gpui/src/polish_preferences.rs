use crate::config::{Config, LlmOpenAIConfig};
use crate::drafts;
use crate::polish::Profile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MINIMUM_REVIEWS: usize = 5;
const PASS_PERCENT: usize = 80;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Model {
    base_url: String,
    model_id: String,
}

impl Model {
    fn from_settings(settings: &LlmOpenAIConfig) -> Result<Self, String> {
        let invalid_url = || "请输入有效的 HTTP 或 HTTPS 整理服务地址。".to_owned();
        let url = reqwest::Url::parse(settings.base_url.trim()).map_err(|_| invalid_url())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid_url());
        }
        let model_id = settings.model.trim();
        if model_id.is_empty() {
            return Err("请填写整理模型 ID。".into());
        }
        Ok(Self {
            base_url: url.as_str().trim_end_matches('/').to_owned(),
            model_id: model_id.to_owned(),
        })
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Ratings {
    selected: Profile,
    votes: Vec<ProfileVotes>,
}

#[derive(Clone, Deserialize, Serialize)]
struct ProfileVotes {
    profile: Profile,
    samples: BTreeMap<String, bool>,
}

impl Ratings {
    fn progress(&self, profile: Profile) -> (usize, usize) {
        self.votes
            .iter()
            .find(|votes| votes.profile == profile)
            .map(|votes| {
                let good = votes.samples.values().filter(|&&good| good).count();
                (good, votes.samples.len())
            })
            .unwrap_or_default()
    }

    fn passes(&self, profile: Profile) -> bool {
        let (good, total) = self.progress(profile);
        total >= MINIMUM_REVIEWS && good * 100 >= total * PASS_PERCENT
    }

    fn record(&mut self, sample: &str, profile: Profile, good: bool) {
        if let Some(votes) = self.votes.iter_mut().find(|votes| votes.profile == profile) {
            votes.samples.insert(sample.to_owned(), good);
        } else {
            self.votes.push(ProfileVotes {
                profile,
                samples: BTreeMap::from([(sample.to_owned(), good)]),
            });
        }

        if !self.passes(self.selected) {
            self.selected = Profile::ALL
                .into_iter()
                .find(|&candidate| self.passes(candidate))
                .unwrap_or_default();
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ModelRatings {
    model: Model,
    ratings: Ratings,
}

fn read_store(path: &Path) -> Result<Vec<ModelRatings>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "无法读取整理评分文件。".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("无法读取整理评分文件：{error}")),
    }
}

fn lock_store(path: &Path) -> io::Result<fs::File> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    Ok(lock)
}

pub struct Preferences {
    path: PathBuf,
    model: Model,
    ratings: Ratings,
}

pub fn load(settings: &LlmOpenAIConfig) -> Result<Preferences, String> {
    Preferences::load_at(Config::config_dir().join("polish-ratings.json"), settings)
}

impl Preferences {
    fn load_at(path: PathBuf, settings: &LlmOpenAIConfig) -> Result<Self, String> {
        let model = Model::from_settings(settings)?;
        let ratings = read_store(&path)?
            .into_iter()
            .find(|entry| entry.model == model)
            .map(|entry| entry.ratings)
            .unwrap_or_default();
        Ok(Self {
            path,
            model,
            ratings,
        })
    }

    pub fn selected(&self) -> Profile {
        self.ratings.selected
    }

    pub fn progress(&self, profile: Profile) -> (usize, usize) {
        self.ratings.progress(profile)
    }

    pub fn vote(&self, sample: &str, profile: Profile) -> Option<bool> {
        self.ratings
            .votes
            .iter()
            .find(|votes| votes.profile == profile)?
            .samples
            .get(sample)
            .copied()
    }

    pub fn next_candidate(&self) -> Profile {
        if self.ratings.passes(self.selected()) {
            return self.selected();
        }
        Profile::ALL
            .into_iter()
            .min_by_key(|&profile| self.progress(profile).1)
            .unwrap_or_default()
    }

    pub fn settled(&self) -> bool {
        self.ratings.passes(self.selected())
    }

    pub fn record(&mut self, sample: &str, profile: Profile, good: bool) -> Result<(), String> {
        if sample.is_empty()
            || sample.len() > 128
            || !sample
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("整理评分需要有效的样本 ID。".into());
        }

        let _write_guard =
            lock_store(&self.path).map_err(|error| format!("无法锁定整理评分文件：{error}"))?;
        let mut store = read_store(&self.path)?;
        let index = match store.iter().position(|entry| entry.model == self.model) {
            Some(index) => index,
            None => {
                store.push(ModelRatings {
                    model: self.model.clone(),
                    ratings: Ratings::default(),
                });
                store.len() - 1
            }
        };
        store[index].ratings.record(sample, profile, good);
        let ratings = store[index].ratings.clone();
        let bytes = serde_json::to_vec(&store).map_err(|_| "无法编码整理评分。")?;
        drafts::write_atomically(&self.path, &bytes)
            .map_err(|error| format!("无法保存整理评分：{error}"))?;
        self.ratings = ratings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("speaky-ratings-{}", uuid::Uuid::new_v4()))
            .join("ratings.json")
    }

    #[test]
    fn ratings_require_five_unique_samples_and_eighty_percent_good() {
        let mut ratings = Ratings::default();
        let profile = Profile::Structured;
        for sample in ["first", "second", "third", "fourth"] {
            ratings.record(sample, profile, true);
        }
        assert_eq!(ratings.selected, Profile::Balanced);
        ratings.record("fourth", profile, false);
        ratings.record("fifth", profile, false);
        assert_eq!(ratings.progress(profile), (3, 5));
        assert_eq!(ratings.selected, Profile::Balanced);
        ratings.record("fifth", profile, true);
        assert_eq!(ratings.progress(profile), (4, 5));
        assert_eq!(ratings.selected, profile);
    }

    #[test]
    fn preferences_reopen_and_isolate_models_and_profiles() {
        let path = temporary_path();
        let settings = LlmOpenAIConfig {
            api_key: "sensitive-key".into(),
            base_url: " https://EXAMPLE.test:443/v1/ ".into(),
            model: " model-a ".into(),
        };
        let mut preferences = Preferences::load_at(path.clone(), &settings).unwrap();
        assert_eq!(preferences.next_candidate(), Profile::Balanced);
        preferences
            .record("sample-1", Profile::Balanced, true)
            .unwrap();
        assert_eq!(preferences.next_candidate(), Profile::Verbatim);
        preferences
            .record("sample-1", Profile::Verbatim, false)
            .unwrap();
        assert_eq!(preferences.progress(Profile::Balanced), (1, 1));
        assert_eq!(preferences.progress(Profile::Verbatim), (0, 1));
        assert_eq!(preferences.next_candidate(), Profile::Structured);

        for sample in ["first", "second", "third", "fourth", "fifth"] {
            preferences
                .record(sample, Profile::Structured, true)
                .unwrap();
        }
        let mut normalized = LlmOpenAIConfig {
            api_key: String::new(),
            base_url: "https://example.test/v1".into(),
            model: "model-a".into(),
        };
        let same = Preferences::load_at(path.clone(), &normalized).unwrap();
        assert_eq!(same.selected(), Profile::Structured);
        assert_eq!(same.next_candidate(), Profile::Structured);
        normalized.model = "model-b".into();
        let another = Preferences::load_at(path.clone(), &normalized).unwrap();
        assert_eq!(another.selected(), Profile::Balanced);
        assert_eq!(another.progress(Profile::Structured), (0, 0));
        normalized.model = "model-a".into();
        normalized.base_url = "https://example.test/other".into();
        let another = Preferences::load_at(path.clone(), &normalized).unwrap();
        assert_eq!(another.progress(Profile::Structured), (0, 0));
        assert!(!fs::read_to_string(&path).unwrap().contains("sensitive-key"));

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn concurrent_handles_merge_votes_and_failed_writes_leave_memory_intact() {
        let path = temporary_path();
        let settings = LlmOpenAIConfig::default();
        let mut first = Preferences::load_at(path.clone(), &settings).unwrap();
        let mut second = Preferences::load_at(path.clone(), &settings).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| first.record("sample-1", Profile::Balanced, true).unwrap());
            scope.spawn(|| second.record("sample-2", Profile::Balanced, true).unwrap());
        });
        let mut reloaded = Preferences::load_at(path.clone(), &settings).unwrap();
        assert_eq!(reloaded.progress(Profile::Balanced), (2, 2));

        fs::write(&path, b"incompatible file").unwrap();
        assert!(reloaded
            .record("sample-3", Profile::Balanced, true)
            .is_err());
        assert_eq!(reloaded.progress(Profile::Balanced), (2, 2));

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

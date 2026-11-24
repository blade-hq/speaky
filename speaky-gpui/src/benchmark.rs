use crate::{engines::Engine, local_models};
use serde::Deserialize;
use std::{
    collections::HashSet,
    env, fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Deserialize)]
pub(super) struct Dataset {
    #[serde(rename = "dataset")]
    pub name: String,
    pub cases: Vec<Sample>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Sample {
    pub id: String,
    pub audio: PathBuf,
    pub expected: String,
    #[serde(default)]
    pub duration: f64,
}

pub(super) fn load() -> Result<Vec<Dataset>, String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let parent = exe.parent().ok_or("无法定位 Speaky 安装目录。")?;
    let packaged = parent.join("ASR-Tests");
    let app_dir = env::var_os("SPEAKY_APP_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("ASR-Tests"));
    let dev_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ASR-Tests");
    let root = std::iter::once(packaged)
        .chain(app_dir)
        .chain(
            parent
                .ancestors()
                .any(|p| p.file_name().is_some_and(|n| n == "target"))
                .then_some(dev_dir),
        )
        .find(|dir| dir.is_dir())
        .ok_or("找不到 ASR-Tests 文件夹，请检查 Speaky 安装目录中的测试集。")?;
    load_from(&root)
}

fn load_from(root: &Path) -> Result<Vec<Dataset>, String> {
    let mut manifests = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| format!("无法读取测试集目录：{e}"))? {
        let path = entry
            .map_err(|e| e.to_string())?
            .path()
            .join("manifest.json");
        if path.is_file() {
            manifests.push(path);
        }
    }
    manifests.sort();
    if manifests.is_empty() {
        return Err("ASR-Tests 文件夹内没有测试集清单。".into());
    }
    manifests
        .into_iter()
        .map(|path| {
            read_manifest(&path).map_err(|err| format!("读取 {} 失败：{err}", path.display()))
        })
        .collect()
}

fn read_manifest(path: &Path) -> Result<Dataset, String> {
    let contents = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut dataset: Dataset =
        serde_json::from_str(contents.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    if dataset.name.trim().is_empty() || dataset.cases.is_empty() {
        return Err("清单缺少数据集名称或录音条目。".into());
    }
    let folder = path.parent().ok_or("无法定位清单目录。")?;
    let canonical_folder = fs::canonicalize(folder).map_err(|e| e.to_string())?;
    let mut ids = HashSet::new();
    for sample in &mut dataset.cases {
        if sample.id.trim().is_empty()
            || !ids.insert(sample.id.clone())
            || sample.expected.trim().is_empty()
        {
            return Err(format!("录音条目 {} 缺少参考文本或编号无效。", sample.id));
        }
        if !sample
            .audio
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
            || !sample
                .audio
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("wav"))
        {
            return Err(format!("{} 的录音路径无效。", sample.id));
        }
        let audio = folder.join(&sample.audio);
        if !audio.is_file()
            || !fs::canonicalize(&audio).is_ok_and(|p| p.starts_with(&canonical_folder))
        {
            return Err(format!("未找到 {} 对应的数据集录音。", sample.id));
        }
        sample.audio = audio;
    }
    Ok(dataset)
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) enum BenchmarkModel {
    Local(String),
    Remote {
        protocol: &'static str,
        model: String,
        endpoint: String,
    },
    Framework {
        framework: &'static str,
        model: String,
        url: String,
    },
}

impl BenchmarkModel {
    pub(super) fn selected(config: &crate::config::Config) -> Option<Self> {
        if config.engine.current == "remote" {
            let service = &config.engine.remote;
            return Some(Self::Remote {
                protocol: service.protocol.label(),
                model: service.label(),
                endpoint: service.endpoint.clone(),
            });
        }
        if let Some(f) = crate::config::Framework::selected(config) {
            let service = f.service(config);
            Some(Self::Framework {
                framework: f.id,
                model: service.model,
                url: service.base_url,
            })
        } else {
            local_models::selected_id(config).map(|id| Self::Local(id.to_owned()))
        }
    }

    pub(super) fn label(&self) -> String {
        match self {
            Self::Local(id) => id.clone(),
            Self::Remote { model, .. } => model.clone(),
            Self::Framework {
                framework, model, ..
            } => crate::config::Framework::ALL
                .iter()
                .find(|f| f.id == *framework)
                .map_or_else(|| model.clone(), |f| f.display_model(model)),
        }
    }
}

pub(super) fn recognize(model: &BenchmarkModel, sample: &Sample) -> Result<String, String> {
    let audio = fs::read(&sample.audio).map_err(|e| format!("无法读取录音 {}：{e}", sample.id))?;
    let engine: Box<dyn Engine> = match model {
        BenchmarkModel::Local(id) => Box::new(local_models::benchmark_engine(id)?),
        BenchmarkModel::Framework { .. } | BenchmarkModel::Remote { .. } => {
            let config = crate::config::Config::load().map_err(|e| e.to_string())?;
            if BenchmarkModel::selected(&config).as_ref() != Some(model) {
                return Err("识别服务或模型已变更，请重新选择。".into());
            }
            crate::engines::recognition_engine(&config)?
        }
    };
    engine
        .transcribe(&audio, "auto")
        .map_err(|e| format!("识别失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_manifest_audio_and_rejects_external_paths() {
        let temp = env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let set = temp.join("set");
        fs::create_dir_all(&set).unwrap();
        fs::write(set.join("sample.wav"), b"audio").unwrap();
        let write_manifest = |audio| {
            let json = serde_json::json!({"dataset":"Test","cases":[
                {"id":"01","audio":audio,"expected":"参考文本","duration":1.5}
            ]});
            fs::write(set.join("manifest.json"), json.to_string()).unwrap();
        };
        write_manifest("sample.wav");
        let sets = load_from(&temp).unwrap();
        assert_eq!(sets[0].cases[0].audio, set.join("sample.wav"));
        assert_eq!(sets[0].cases[0].expected, "参考文本");
        write_manifest("../set/sample.wav");
        assert!(load_from(&temp).is_err());
        fs::remove_file(set.join("manifest.json")).unwrap();
        fs::remove_file(set.join("sample.wav")).unwrap();
        fs::remove_dir(set).unwrap();
        fs::remove_dir(temp).unwrap();
    }
}

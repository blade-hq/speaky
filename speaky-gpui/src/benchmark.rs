use crate::{engines::Engine, local_models};
pub(super) use speaky_core::benchmark_sets::{load, Dataset, Sample};
use std::fs;

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
        let service = match config.engine.current.as_str() {
            "remote" => Some(config.engine.remote.clone()),
            "autodl" => crate::engines::autodl_asr_service(config).ok(),
            _ => None,
        };
        if let Some(service) = service {
            return Some(Self::Remote {
                protocol: service.protocol.label(),
                model: service.label(),
                endpoint: service.endpoint,
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
            crate::model_picker::selected_id(config).map(|id| Self::Local(id.to_owned()))
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
        BenchmarkModel::Local(id) => {
            let config = crate::config::Config::load().map_err(|e| e.to_string())?;
            if crate::model_packs::selected_id(&config) == Some(id.as_str()) {
                Box::new(crate::model_packs::ready_engine(&config)?)
            } else {
                Box::new(local_models::benchmark_engine(id)?)
            }
        }
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

use crate::{
    config::{Config, OpenAIConfig},
    drafts,
    engines::OpenAIEngine,
};
use serde::Deserialize;
use std::{
    env, fs,
    io::ErrorKind,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

#[derive(Clone, Debug, Deserialize)]
pub struct Model {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LocalEndpoint {
    pub id: String,
    base_url: String,
    use_vocabulary_prompt: bool,
}

// A fresh GUI process must register its own lifetime with the runtime. Saved
// ports may belong to a previous session. Hold this lock through preparation
// so startup preloading and the first dictation share a single GPU load.
static SESSION: Mutex<Option<LocalEndpoint>> = Mutex::new(None);

fn helper_path() -> Result<PathBuf, String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("无法定位 Speaky 安装目录。")?;
    let packaged = dir.join("scripts/local-models.ps1");
    if packaged.is_file() {
        return Ok(packaged);
    }
    if let Some(root) = env::var_os("SPEAKY_APP_DIR") {
        let helper = PathBuf::from(root).join("scripts/local-models.ps1");
        if helper.is_absolute() && helper.is_file() {
            return Ok(helper);
        }
    }
    // cargo run puts the binary below target/{debug,release}; a packaged copy
    // should resolve its own scripts, even when a source tree also exists.
    if dir
        .ancestors()
        .any(|p| p.file_name().is_some_and(|n| n == "target"))
    {
        let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/local-models.ps1");
        if helper.is_file() {
            return Ok(helper);
        }
    }
    Err("找不到本地模型组件。请从完整的 Windows 便携包启动 Speaky。".into())
}

fn run_helper<T: serde::de::DeserializeOwned>(
    action: &str,
    model: Option<&str>,
) -> Result<T, String> {
    let script = helper_path()?;
    let powershell = PathBuf::from(env::var_os("SystemRoot").ok_or("无法定位 Windows 目录。")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = Command::new(powershell);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .args(["-Action", action, "-OwnerPid"])
        .arg(std::process::id().to_string());
    if let Some(id) = model {
        command.args(["-ModelId", id]);
    }
    let output = command
        .output()
        .map_err(|e| format!("启动本地模型组件失败：{e}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        let message = error
            .trim()
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("本地模型组件运行失败。");
        return Err(message.to_owned());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("本地模型组件返回的数据无效：{e}"))
}

pub fn list() -> Result<Vec<Model>, String> {
    run_helper("List", None)
}

pub fn selected_id(config: &Config) -> Option<&str> {
    match config.engine.current.as_str() {
        "local" => config
            .engine
            .local
            .as_ref()
            .map(|service| match service.model.as_str() {
                "SenseVoiceSmall-int8" => "SenseVoiceSmall-FP32",
                id => id,
            }),
        "openai"
            if config.engine.openai.model == "Qwen3-ASR-0.6B"
                && config.engine.openai.base_url.trim_end_matches('/')
                    == "http://127.0.0.1:18107/v1" =>
        {
            Some(&config.engine.openai.model)
        }
        _ => None,
    }
}

pub fn selected_label(config: &Config) -> String {
    if let Some(f) = crate::config::Framework::selected(config) {
        return f.display_model(&f.service(config).model);
    }
    if let Some(id) = selected_id(config) {
        return id.to_owned();
    }
    match config.engine.current.as_str() {
        "remote" => config.engine.remote.label(),
        "openai" => config.engine.openai.model.clone(),
        "volc_bigmodel" if !config.engine.volc_bigmodel.api_key.is_empty() => "火山引擎".into(),
        _ => "选择模型".into(),
    }
}

impl LocalEndpoint {
    fn service(&self) -> OpenAIConfig {
        OpenAIConfig {
            api_key: "EMPTY".into(),
            model: self.id.clone(),
            base_url: self.base_url.clone(),
            use_vocabulary_prompt: self.use_vocabulary_prompt,
        }
    }
}

pub fn activate(id: &str) -> Result<LocalEndpoint, String> {
    let mut session = SESSION.lock().map_err(|_| "无法读取本地模型状态。")?;
    prepare_model(id, &mut session)
}

fn prepare_model(id: &str, session: &mut Option<LocalEndpoint>) -> Result<LocalEndpoint, String> {
    // Switching may unload the old model even if starting the next one fails.
    *session = None;
    let endpoint: LocalEndpoint = run_helper("Ensure", Some(id))?;
    let url = reqwest::Url::parse(&endpoint.base_url).map_err(|_| "本地模型服务地址无效。")?;
    if endpoint.id != id
        || url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_none()
        || url.path().trim_end_matches('/') != "/v1"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("本地模型服务返回了意外的地址或模型名称。".into());
    }
    *session = Some(endpoint.clone());
    Ok(endpoint)
}

pub fn release() -> Result<(), String> {
    #[derive(Deserialize)]
    struct Released {
        released: bool,
    }
    let mut session = SESSION.lock().map_err(|_| "无法读取本地模型状态。")?;
    let status: Released = run_helper("Release", None)?;
    if !status.released {
        return Err("上一模型仍在使用 GPU。".into());
    }
    *session = None;
    Ok(())
}

pub fn has_owned_manager() -> bool {
    Config::config_dir()
        .join(format!("local-asr-{}.json", std::process::id()))
        .is_file()
}

pub fn select(endpoint: &LocalEndpoint) -> Result<(), String> {
    save_to(&Config::config_path(), endpoint)
}

fn save_to(path: &Path, endpoint: &LocalEndpoint) -> Result<(), String> {
    let mut yaml = match fs::read_to_string(path) {
        Ok(source) => {
            serde_yaml::from_str::<serde_yaml::Value>(&source).map_err(|e| e.to_string())?
        }
        Err(e) if e.kind() == ErrorKind::NotFound => serde_yaml::Value::Null,
        Err(e) => return Err(e.to_string()),
    };
    if !yaml.is_null() {
        serde_yaml::from_value::<Config>(yaml.clone()).map_err(|e| e.to_string())?;
    }
    yaml["engine"]["local"] =
        serde_yaml::to_value(endpoint.service()).map_err(|e| e.to_string())?;
    yaml["engine"]["current"] = "local".into();
    let data = serde_yaml::to_string(&yaml).map_err(|e| e.to_string())?;
    drafts::write_atomically(path, data.as_bytes()).map_err(|e| e.to_string())
}

pub fn ready_engine(config: &Config) -> Result<OpenAIEngine, String> {
    let id = selected_id(config).ok_or("尚未选择本地语音模型。")?;
    let mut session = SESSION.lock().map_err(|_| "无法读取本地模型状态。")?;
    if let Some(cached) = session.as_ref().filter(|service| service.id == id) {
        let engine = OpenAIEngine::from_local_service(&cached.service());
        // The previous manager may have died and its port may now belong to a different GUI.
        if engine.check_model_for(Some(std::process::id())).is_ok() {
            return Ok(engine);
        }
    }
    let active = prepare_model(id, &mut session)?;
    Ok(OpenAIEngine::from_local_service(&active.service()))
}

pub(super) fn benchmark_engine(id: &str) -> Result<OpenAIEngine, String> {
    let config = Config::load().map_err(|e| format!("无法读取当前模型配置：{e}"))?;
    if selected_id(&config) != Some(id) {
        return Err("所选模型已变更，请重新选择要评测的本地模型。".into());
    }
    let endpoint = {
        let mut session = SESSION.lock().map_err(|_| "无法读取本地模型状态。")?;
        match session.as_ref() {
            Some(active) if active.id != id => {
                return Err("当前加载的模型与选择不一致，请重新选择模型。".into());
            }
            Some(active) => active.clone(),
            // A fresh GUI remembers its selection but has no GPU worker yet.
            None => prepare_model(id, &mut session)?,
        }
    };
    // These short samples use the engine's usual 60-second request timeout.
    let engine = OpenAIEngine::from_service(&endpoint.service());
    engine.check_model_for(Some(std::process::id()))?;
    Ok(engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_selection_survives_reload_and_preserves_custom_service() {
        let path = std::env::temp_dir()
            .join(uuid::Uuid::new_v4().to_string())
            .join("config.yaml");
        drafts::write_atomically(&path, b"engine:\n  current: openai\n  openai:\n    model: private-asr\n    api_key: original\ncustom_field: keep\n").unwrap();
        for id in [
            "SenseVoiceSmall-int8",
            "SenseVoiceSmall-FP32",
            "Whisper-large-v3-turbo",
        ] {
            let endpoint = LocalEndpoint {
                id: id.into(),
                base_url: "http://127.0.0.1:18118/v1".into(),
                use_vocabulary_prompt: false,
            };
            save_to(&path, &endpoint).unwrap();
            let source = fs::read_to_string(&path).unwrap();
            let config: Config = serde_yaml::from_str(&source).unwrap();
            let selected = if id == "SenseVoiceSmall-int8" {
                "SenseVoiceSmall-FP32"
            } else {
                id
            };
            assert_eq!(selected_id(&config), Some(selected));
            assert_eq!(config.engine.openai.api_key, "original");
            assert_eq!(config.engine.openai.model, "private-asr");
            assert!(source.contains("custom_field: keep"));
        }
        fs::remove_file(&path).unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
    }
}

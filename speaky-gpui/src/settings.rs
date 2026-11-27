use super::{
    button, config::Config, drafts, hotkey, onboarding::Anchor, page_heading, Page, Speaky, BORDER,
    MUTED, SURFACE,
};
#[cfg(any(target_os = "macos", windows))]
use super::{engines::OpenAIEngine, service_ui};
use gpui::{
    div, prelude::*, px, rgb, Context, Div, FocusHandle, SharedString, Subscription, Window,
};
#[cfg(not(windows))]
use gpui::{KeyUpEvent, KeystrokeEvent};
use serde_yaml::Value;
use std::{collections::HashMap, fs, io::ErrorKind, path::Path};

use speaky_core::autodl as autodl_api;
use speaky_core::session::{sample_comparison, LANGUAGES, SAMPLE_AUDIO, SAMPLE_REFERENCE};

#[cfg(any(target_os = "macos", windows))]
mod autodl;

#[cfg(any(not(windows), test))]
fn shortcut_modifiers(stroke: &gpui::Keystroke) -> Result<u8, String> {
    let mods = stroke.modifiers;
    if mods.function {
        return Err("系统无法将 Fn 作为全局快捷键的修饰键。".into());
    }
    Ok(hotkey::CTRL * u8::from(mods.control)
        | hotkey::ALT * u8::from(mods.alt)
        | hotkey::SHIFT * u8::from(mods.shift)
        | hotkey::PLATFORM * u8::from(mods.platform))
}

#[cfg(any(not(windows), test))]
fn shortcut_from_keystroke(stroke: &gpui::Keystroke) -> Result<hotkey::Shortcut, String> {
    hotkey::Shortcut::from_key(&stroke.key, shortcut_modifiers(stroke)?)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Device,
    Language,
}

struct ShortcutCapture {
    focus: FocusHandle,
    _activation: Subscription,
    #[cfg(not(windows))]
    _keydown: Subscription,
    #[cfg(windows)]
    _native: crate::hotkey_capture::WindowCapture,
    #[cfg(windows)]
    session: uuid::Uuid,
    previous: hotkey::Shortcut,
    candidate: Option<hotkey::Shortcut>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServiceCheck {
    Detect,
    Sample,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) enum ModelTestTarget {
    Local(Option<String>),
    Volc,
    Remote,
    Autodl,
}

impl ModelTestTarget {
    fn selected(config: &Config) -> Self {
        if crate::engines::uses_local_asr(config) {
            Self::Local(crate::model_picker::selected_id(config).map(str::to_owned))
        } else if config.engine.current == "volc_bigmodel" {
            Self::Volc
        } else if config.engine.current == "autodl" {
            Self::Autodl
        } else {
            Self::Remote
        }
    }
}

pub(super) struct ModelTestFeedback {
    pub(super) notice: Result<String, String>,
    pub(super) metrics: Option<(&'static str, String)>,
}

impl ModelTestFeedback {
    fn notice(notice: Result<String, String>) -> Self {
        Self {
            notice,
            metrics: None,
        }
    }

    fn sample(prefix: &str, seconds: f64, text: &str) -> Self {
        let comparison = sample_comparison(text);
        let metric = if comparison == "样本与参考一致" {
            "一致"
        } else {
            "有差异"
        };
        Self {
            notice: Ok(format!(
                "{prefix}（{seconds:.2} 秒）。{comparison}。样本转写：{text}"
            )),
            metrics: Some((metric, format!("{seconds:.2}s"))),
        }
    }
}

#[derive(Default)]
pub struct SettingsPage {
    config: Option<Config>,
    picker: Option<Picker>,
    shortcut_capture: Option<ShortcutCapture>,
    shortcut_error: Option<String>,
    notice: Option<Result<String, String>>,
    checking: Option<ServiceCheck>,
    pub(super) model_tests: HashMap<ModelTestTarget, ModelTestFeedback>,
    last_model_test: Option<ModelTestTarget>,
    #[cfg(any(target_os = "macos", windows))]
    pending_service: Option<(Value, service_ui::ServiceInput)>,
    pub(crate) polish_report: Option<Result<crate::polish_ui::BenchmarkResult, String>>,
    pub(crate) polish_checking: bool,
    #[cfg(any(target_os = "macos", windows))]
    autodl_catalog: autodl::RewriteCatalog,
}

impl SettingsPage {
    pub(super) fn clear_model_test_feedback(&mut self) {
        self.model_tests.clear();
        self.last_model_test = None;
    }

    pub(super) fn testing_local_model(&self, id: &str) -> bool {
        self.checking == Some(ServiceCheck::Sample)
            && matches!(&self.last_model_test, Some(ModelTestTarget::Local(Some(model))) if model == id)
    }

    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn recognition_checking(&self) -> bool {
        self.checking.is_some()
    }
}

enum Preference {
    Theme(crate::theme::Theme),
    Shortcut(hotkey::Shortcut),
    #[cfg(windows)]
    FloatingButton(bool),
    #[cfg(windows)]
    Volc(service_ui::ServiceInput),
    PreloadLocalModel(bool),
    LocalPack(String),
    Device(Option<String>),
    Language(&'static str),
    #[cfg(any(target_os = "macos", windows))]
    Service(service_ui::ServiceInput, bool),
    #[cfg(any(target_os = "macos", windows))]
    Framework(service_ui::ServiceInput, crate::config::Framework),
    #[cfg(any(target_os = "macos", windows))]
    Remote {
        service: crate::config::RemoteService,
        previous_engine: Value,
    },
    #[cfg(any(target_os = "macos", windows))]
    Autodl {
        service: crate::config::RemoteService,
        previous_engine: Value,
    },
    #[cfg(any(target_os = "macos", windows))]
    AutodlKey(String),
    #[cfg(any(target_os = "macos", windows))]
    AutodlRewrite {
        model: String,
        checked_key: String,
    },
    OrganizerEnabled(bool),
    CustomPrompt(String),
}

pub fn selected_shortcut(config: &Config) -> hotkey::Shortcut {
    #[cfg(windows)]
    let selected = config.core.asr.windows_toggle_hotkey.as_deref();
    #[cfg(not(windows))]
    let selected = config.core.asr.macos_toggle_hotkey.as_deref();
    selected
        .and_then(hotkey::Shortcut::from_id)
        .unwrap_or_default()
}

pub(super) fn save_local_pack(id: &str) -> Result<(), String> {
    save(&Config::config_path(), &Preference::LocalPack(id.into()))
}

fn save(path: &Path, preference: &Preference) -> Result<(), String> {
    let mut yaml: Value = match fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == ErrorKind::NotFound => Value::Null,
        Err(error) => return Err(error.to_string()),
    };
    #[cfg(any(target_os = "macos", windows))]
    let existing = if yaml.is_null() {
        Config::default()
    } else {
        serde_yaml::from_value::<Config>(yaml.clone()).map_err(|error| error.to_string())?
    };
    match preference {
        Preference::Theme(theme) => yaml["appearance"]["theme"] = theme.id().into(),
        #[cfg(windows)]
        Preference::Volc(input) => {
            if input.base_url.trim() != service_ui::VOLC_URL {
                return Err("此模型使用固定的火山引擎协议地址。".into());
            }
            let model = input.model.trim();
            if model.is_empty() || reqwest::header::HeaderValue::from_str(model).is_err() {
                return Err("请填写有效的火山引擎资源 ID。".into());
            }
            if let Some(key) = input
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                reqwest::header::HeaderValue::from_str(key)
                    .map_err(|_| "API 密钥含不支持的字符。")?;
                yaml["engine"]["volc_bigmodel"]["api_key"] = key.into();
            } else {
                let volc = &existing.engine.volc_bigmodel;
                if volc.api_key.is_empty()
                    && (volc.app_key.is_empty() || volc.access_key.is_empty())
                    && !std::env::var_os("SPEAKY_VOLC_API_KEY").is_some_and(|k| !k.is_empty())
                {
                    return Err("请填写火山引擎 API 密钥。".into());
                }
            }
            yaml["engine"]["volc_bigmodel"]["resource_id"] = model.into();
            yaml["engine"]["current"] = "volc_bigmodel".into();
        }
        Preference::Shortcut(preset) => {
            let key = if cfg!(windows) {
                "windows_toggle_hotkey"
            } else {
                "macos_toggle_hotkey"
            };
            yaml["core"]["asr"][key] = preset.id().into();
        }
        Preference::Device(name) => {
            let asr = &mut yaml["core"]["asr"];
            asr["audio_device"] = Value::Null;
            asr["audio_device_name"] = name.clone().map_or(Value::Null, Value::String);
        }
        #[cfg(windows)]
        Preference::FloatingButton(enabled) => {
            yaml["desktop"]["windows_floating_button"] = (*enabled).into();
        }
        Preference::LocalPack(id) => {
            yaml["engine"]["current"] = "local".into();
            yaml["engine"]["local_pack"] = id.as_str().into();
            yaml["engine"]["local"] = Value::Null;
        }
        Preference::PreloadLocalModel(enabled) => {
            yaml["desktop"]["preload_local_model"] = (*enabled).into();
        }
        Preference::Language(language) => {
            yaml["core"]["asr"]["language"] = (*language).into();
        }
        Preference::OrganizerEnabled(enabled) => yaml["llm"]["enabled"] = (*enabled).into(),
        Preference::CustomPrompt(prompt) => {
            yaml["llm"]["custom_prompt"] = prompt.trim().into();
        }
        #[cfg(any(target_os = "macos", windows))]
        Preference::Remote {
            service,
            previous_engine,
        } => {
            if &engine_state(&existing)? != previous_engine {
                return Err("检测期间识别配置已变更，请重新检测。".into());
            }
            yaml["engine"]["remote"] = serde_yaml::to_value(service).map_err(|e| e.to_string())?;
            yaml["engine"]["current"] = "remote".into();
        }
        #[cfg(any(target_os = "macos", windows))]
        Preference::Autodl {
            service,
            previous_engine,
        } => {
            if &engine_state(&existing)? != previous_engine
                || &crate::engines::autodl_asr_service(&existing)? != service
            {
                return Err("检测期间识别配置已变更，请重新检测。".into());
            }
            yaml["engine"]["current"] = "autodl".into();
        }
        #[cfg(any(target_os = "macos", windows))]
        Preference::AutodlKey(key) => save_autodl_key(&mut yaml, &existing, key)?,
        #[cfg(any(target_os = "macos", windows))]
        Preference::AutodlRewrite { model, checked_key } => {
            let model = model.trim();
            if model.is_empty() {
                return Err("请填写 AutoDL 模型 ID。".into());
            }
            if existing.llm.autodl_key() != Some(checked_key.as_str()) {
                return Err("AutoDL 密钥已变更，请重新测试模型。".into());
            }
            save_autodl_key(&mut yaml, &existing, checked_key)?;
            yaml["llm"]["autodl_model"] = model.into();
            yaml["llm"]["enabled"] = true.into();
        }
        #[cfg(any(target_os = "macos", windows))]
        Preference::Service(input, _) | Preference::Framework(input, _) => {
            let organizer = matches!(preference, Preference::Service(_, true));
            let framework = match preference {
                Preference::Framework(_, f) => Some(*f),
                _ => None,
            };
            let url = service_url(&input.base_url)?;
            let model = input.model.trim();
            if model.is_empty() {
                return Err("请填写模型名称。".into());
            }
            let key = input
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty());
            if organizer && autodl_api::is_base_url(url.as_str()) {
                let key = key
                    .or_else(|| existing.llm.autodl_key())
                    .ok_or("请填写 AutoDL 的 API Key。")?;
                save_autodl_key(&mut yaml, &existing, key)?;
                yaml["llm"]["autodl_model"] = model.into();
                yaml["llm"]["enabled"] = true.into();
            } else {
                let profile = framework.map(|f| f.service(&existing));
                let (old_url, old_key) = if let Some(profile) = &profile {
                    (profile.base_url.as_str(), profile.api_key.as_str())
                } else if organizer {
                    if existing.llm.autodl_api_key.trim().is_empty() {
                        if let Some(key) = existing.llm.autodl_key() {
                            yaml["llm"]["autodl_api_key"] = key.into();
                        }
                    }
                    if !existing.llm.autodl_model.is_empty() {
                        yaml["llm"]["autodl_model"] = "".into();
                    }
                    let previous = &existing.llm.openai;
                    (
                        previous.base_url.as_str(),
                        if autodl_api::is_base_url(&previous.base_url) {
                            ""
                        } else {
                            previous.api_key.as_str()
                        },
                    )
                } else {
                    (
                        existing.engine.openai.base_url.as_str(),
                        existing.engine.openai.api_key.as_str(),
                    )
                };
                let same_origin =
                    service_url(old_url).is_ok_and(|old| old.origin() == url.origin());
                let optional_key = framework.is_some() && crate::engines::loopback(&url);
                if key.is_none() && !optional_key && (old_key.trim().is_empty() || !same_origin) {
                    return Err("请填写此服务的 API 密钥。无鉴权的内网服务可填 EMPTY。".into());
                }
                let target = if let Some(framework) = framework {
                    yaml["engine"]["current"] = framework.id.into();
                    &mut yaml["engine"]["frameworks"][framework.id]
                } else if organizer {
                    yaml["llm"]["enabled"] = true.into();
                    &mut yaml["llm"]["openai"]
                } else {
                    yaml["engine"]["current"] = "openai".into();
                    &mut yaml["engine"]["openai"]
                };
                if let Some(key) = key {
                    reqwest::header::HeaderValue::from_str(key)
                        .map_err(|_| "API 密钥含不支持的字符。")?;
                    target["api_key"] = key.into();
                } else if !same_origin {
                    target["api_key"] = "".into();
                }
                target["base_url"] = url.as_str().trim_end_matches('/').into();
                target["model"] = model.into();
            }
        }
    }

    let serialized = serde_yaml::to_string(&yaml).map_err(|error| error.to_string())?;
    drafts::write_atomically(path, serialized.as_bytes()).map_err(|error| error.to_string())
}

#[cfg(any(target_os = "macos", windows))]
fn save_autodl_key(yaml: &mut Value, previous: &Config, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("请填写 AutoDL 的 API Key。".into());
    }
    reqwest::header::HeaderValue::from_str(key).map_err(|_| "API Key 含不支持的字符。")?;
    yaml["llm"]["autodl_api_key"] = key.into();
    if autodl_api::is_base_url(&previous.llm.openai.base_url) {
        yaml["llm"]["openai"]["api_key"] = "".into();
    }
    Ok(())
}

fn service_url(value: &str) -> Result<reqwest::Url, String> {
    let error =
        || "请输入完整的 HTTP 或 HTTPS 服务地址，例如 http://127.0.0.1:8201/v1。".to_owned();
    let url = reqwest::Url::parse(value.trim()).map_err(|_| error())?;
    if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
        return Err(error());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("服务地址应仅包含服务器和 API 路径。API 密钥请单独填写。".into());
    }
    Ok(url)
}

fn model_service_configured(address: &str, model: &str, key: &str) -> bool {
    !model.trim().is_empty()
        && service_url(address)
            .is_ok_and(|url| !key.trim().is_empty() || crate::engines::loopback(&url))
}

pub(super) fn rewrite_configured(config: &Config) -> bool {
    let rewrite = config.llm.organizer_service();
    model_service_configured(&rewrite.base_url, &rewrite.model, &rewrite.api_key)
}

fn external_asr_configured(config: &Config) -> bool {
    let engine = &config.engine;
    if let Some(framework) = crate::config::Framework::selected(config) {
        let service = framework.service(config);
        return model_service_configured(&service.base_url, &service.model, &service.api_key);
    }
    match engine.current.as_str() {
        "autodl" => crate::engines::autodl_asr_service(config).is_ok(),
        "openai" => {
            let service = &engine.openai;
            model_service_configured(&service.base_url, &service.model, &service.api_key)
        }
        "remote" => {
            let remote = &engine.remote;
            !remote.endpoint.trim().is_empty()
                && (remote.protocol != crate::engines::Protocol::OpenAI
                    || !remote.model.trim().is_empty())
        }
        "volc_bigmodel" => volc_asr_configured(config),
        _ => false,
    }
}

fn volc_asr_configured(config: &Config) -> bool {
    let volc = &config.engine.volc_bigmodel;
    !volc.api_key.trim().is_empty()
        || (!volc.access_key.trim().is_empty() && !volc.app_key.trim().is_empty())
        || std::env::var_os("SPEAKY_VOLC_API_KEY").is_some_and(|key| !key.is_empty())
}

#[cfg(any(target_os = "macos", windows))]
fn engine_state(config: &Config) -> Result<Value, String> {
    serde_yaml::to_value(&config.engine).map_err(|e| e.to_string())
}

#[cfg(any(target_os = "macos", windows))]
fn remote_editor_values(config: &Config) -> (String, String, String) {
    let remote = &config.engine.remote;
    if config.engine.current == "remote"
        || (!remote.address.is_empty()
            && (crate::engines::uses_local_asr(config)
                || matches!(config.engine.current.as_str(), "autodl" | "volc_bigmodel")))
    {
        return (
            remote.address.clone(),
            remote.model.clone(),
            remote.api_key.clone(),
        );
    }
    if let Some(framework) = crate::config::Framework::selected(config) {
        let service = framework.service(config);
        return (service.base_url, service.model, service.api_key);
    }
    let service = &config.engine.openai;
    if config.engine.current == "openai" && !crate::engines::uses_local_asr(config) {
        return (
            service.base_url.clone(),
            service.model.clone(),
            service.api_key.clone(),
        );
    }
    (String::new(), String::new(), String::new())
}

#[cfg(any(target_os = "macos", windows))]
fn submitted_key(
    input: &service_ui::ServiceInput,
    previous_url: &str,
    previous_key: &str,
) -> String {
    if let Some(key) = &input.api_key {
        return key.trim().to_owned();
    }
    if let (Ok(old), Ok(new)) = (
        reqwest::Url::parse(previous_url),
        reqwest::Url::parse(input.base_url.trim()),
    ) {
        if old.has_host()
            && old.scheme() == new.scheme()
            && old.host() == new.host()
            && old.port_or_known_default() == new.port_or_known_default()
        {
            return previous_key.to_owned();
        }
    }
    String::new()
}

impl Speaky {
    #[cfg(windows)]
    pub(crate) fn tour_floating_status(&self) -> crate::floating_control::FloatingStatus {
        use crate::floating_control::FloatingStatus;

        if self.floating.is_some() {
            return FloatingStatus::Visible;
        }
        match self.settings.config.as_ref() {
            Some(config) if config.desktop.windows_floating_button => FloatingStatus::Unavailable,
            Some(_) => FloatingStatus::Hidden,
            None => FloatingStatus::Unknown,
        }
    }

    #[cfg(windows)]
    pub(crate) fn set_tour_floating_button(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.apply_preference(Preference::FloatingButton(enabled), cx);
    }

    pub(crate) fn recognition_checking(&self) -> bool {
        self.settings.recognition_checking()
    }

    #[cfg(any(target_os = "macos", windows))]
    pub(super) fn resolve_framework_name(&self, cx: &mut Context<Self>) {
        let Ok(config) = Config::load() else { return };
        let Some(framework) = crate::config::Framework::selected(&config) else {
            return;
        };
        let profile = framework.service(&config);
        if framework.is_builtin_endpoint(&profile.base_url)
            && profile.model == framework.preset_name()
        {
            return;
        }
        let task = cx.background_spawn(async move {
            let engine = OpenAIEngine::from_framework(framework, &config).ok()?;
            engine.check_model().ok()?;
            engine.catalog_id().map(str::to_owned)
        });
        cx.spawn(async move |entity, cx| {
            let Some(name) = task.await else { return };
            if name == profile.model {
                return;
            }
            let _ = entity.update(cx, |view, cx| {
                let Ok(current) = Config::load() else { return };
                let active = framework.service(&current);
                if current.engine.current != framework.id
                    || active.base_url != profile.base_url
                    || active.model != profile.model
                    || active.api_key != profile.api_key
                {
                    return;
                }
                let input = service_ui::ServiceInput {
                    base_url: active.base_url,
                    model: name,
                    api_key: None,
                };
                if save(
                    &Config::config_path(),
                    &Preference::Framework(input, framework),
                )
                .is_ok()
                {
                    view.reload_settings();
                    view.refresh_model_selection();
                    cx.notify();
                }
            });
        })
        .detach();
    }
    #[cfg(any(target_os = "macos", windows))]
    pub(super) fn open_service_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_picker.close();
        self.redesign_config.rewrite = false;
        self.open_page(Page::Models, cx);
        self.edit_service(window, false, cx);
    }

    pub(crate) fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.cancel_shortcut_capture(cx);
        if page != self.page {
            self.model_picker.close();
            self.clear_feedback(self.page);
            self.settings.notice = None;
            self.tour.request_reveal();
        }
        #[cfg(windows)]
        {
            if self.page == Page::Benchmark && page != Page::Benchmark && self.benchmark.playing {
                self.stop_playback();
            }
            if matches!(page, Page::Home | Page::Benchmark) {
                self.refresh_model_selection();
            }
            if page == Page::Benchmark && self.page != page {
                self.load_benchmark(cx);
            }
        }
        self.refresh_model_selection();
        self.page = page;
        if page == Page::Models {
            self.reload_settings();
            self.scan_models(cx);
        }
        if page == Page::History {
            self.reload_settings();
        }
        if page == Page::Settings {
            self.settings.picker = None;
            self.reload_settings();
        }
        if matches!(page, Page::Home | Page::Settings) {
            self.refresh_input_devices(cx);
        }
        cx.notify();
    }

    pub(super) fn open_shortcut_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(Page::Settings, cx);
        self.begin_shortcut_capture(window, cx);
    }

    pub(super) fn guide_shortcut(&self) -> Option<hotkey::Shortcut> {
        self.settings.config.as_ref().map(selected_shortcut)
    }

    pub(super) fn guide_shortcut_capturing(&self) -> bool {
        self.settings.shortcut_capture.is_some()
    }

    pub(super) fn guide_shortcut_error(&self) -> Option<&str> {
        self.settings.shortcut_error.as_deref().or_else(|| {
            self.global_hotkey
                .is_none()
                .then_some(self.hotkey_status.as_str())
                .filter(|message| !message.is_empty())
        })
    }

    pub(super) fn clear_guide_shortcut_error(&mut self) {
        if let Some(error) = self.settings.shortcut_error.take() {
            if matches!(&self.settings.notice, Some(Err(message)) if *message == error) {
                self.settings.notice = None;
            }
        }
    }

    pub(super) fn begin_shortcut_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.shortcut_capture.is_some()
            || self.recording.is_some()
            || self.transcribing
            || self.recognition_model_busy()
        {
            return;
        }
        let Some(config) = &self.settings.config else {
            self.settings.shortcut_error = Some("无法读取快捷键设置，请先重新读取配置。".into());
            cx.notify();
            return;
        };
        self.settings.shortcut_error = None;
        #[cfg(windows)]
        let (native, mut events) = match crate::hotkey_capture::WindowCapture::start(window) {
            Ok(capture) => capture,
            Err(error) => {
                self.settings.shortcut_error = Some(error.clone());
                self.settings.notice = Some(Err(error));
                cx.notify();
                return;
            }
        };
        #[cfg(windows)]
        let session = uuid::Uuid::new_v4();
        if let Some(hotkey) = &mut self.global_hotkey {
            hotkey.suspend();
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        #[cfg(not(windows))]
        let keydown = {
            let editor_window = window.window_handle();
            cx.listener(move |view, event, window, cx| {
                if window.window_handle() == editor_window {
                    view.capture_shortcut_down(event, window, cx);
                }
            })
        };
        self.settings.shortcut_capture = Some(ShortcutCapture {
            focus,
            // Run before application bindings such as Ctrl+Q (Quit).
            #[cfg(not(windows))]
            _keydown: cx.intercept_keystrokes(keydown),
            #[cfg(windows)]
            _native: native,
            #[cfg(windows)]
            session,
            _activation: cx.observe_window_activation(window, |view, window, cx| {
                if !window.is_window_active() {
                    view.cancel_shortcut_capture(cx);
                }
            }),
            previous: selected_shortcut(config),
            candidate: None,
        });
        self.settings.picker = None;
        self.settings.notice = None;
        #[cfg(windows)]
        cx.spawn(async move |entity, cx| {
            while let Some(event) = events.recv().await {
                let keep_listening = entity.update(cx, |view, cx| {
                    let Some(capture) = view
                        .settings
                        .shortcut_capture
                        .as_mut()
                        .filter(|capture| capture.session == session)
                    else {
                        return false;
                    };
                    match event {
                        crate::hotkey_capture::CaptureEvent::Preview(candidate) => {
                            capture.candidate = candidate;
                            cx.notify();
                            true
                        }
                        crate::hotkey_capture::CaptureEvent::Chosen(shortcut) => {
                            view.apply_preference(Preference::Shortcut(shortcut), cx);
                            false
                        }
                        crate::hotkey_capture::CaptureEvent::IncompleteChord => {
                            view.cancel_shortcut_capture(cx);
                            let error =
                                "没有收到完整的组合键，可能被系统或其他应用占用。请换一组试试。"
                                    .to_owned();
                            if view.global_hotkey.is_some()
                                || view.settings.shortcut_error.is_none()
                            {
                                view.settings.shortcut_error = Some(error.clone());
                                view.settings.notice = Some(Err(error));
                            }
                            cx.notify();
                            false
                        }
                    }
                });
                if !keep_listening.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn cancel_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        let Some(capture) = self.settings.shortcut_capture.take() else {
            return;
        };
        if let Some(hotkey) = &mut self.global_hotkey {
            if let Err(error) = hotkey.resume(capture.previous) {
                self.global_hotkey = None;
                self.settings.shortcut_error = Some(error.clone());
                self.settings.notice = Some(Err(error.clone()));
                self.hotkey_status = error;
            }
        }
        cx.notify();
    }

    #[cfg(not(windows))]
    fn capture_shortcut_down(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(capture) = &mut self.settings.shortcut_capture else {
            return;
        };
        cx.stop_propagation();
        window.prevent_default();
        if event.keystroke.key == "escape" {
            self.cancel_shortcut_capture(cx);
            return;
        }
        if capture.candidate.is_some() {
            return;
        }
        match shortcut_from_keystroke(&event.keystroke) {
            Ok(shortcut) => {
                capture.candidate = Some(shortcut);
                self.settings.shortcut_error = None;
                self.settings.notice = None;
            }
            Err(error) => {
                self.settings.shortcut_error = Some(error.clone());
                self.settings.notice = Some(Err(error));
            }
        }
        cx.notify();
    }

    #[cfg(not(windows))]
    fn capture_shortcut_up(
        &mut self,
        event: &KeyUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(capture) = &self.settings.shortcut_capture else {
            return;
        };
        cx.stop_propagation();
        window.prevent_default();
        let Some(shortcut) = capture.candidate else {
            if self.settings.notice.is_none() {
                let error = "没有收到完整的按键，快捷键可能已被占用。请换一个试试。".to_owned();
                self.settings.shortcut_error = Some(error.clone());
                self.settings.notice = Some(Err(error));
                cx.notify();
            }
            return;
        };
        if !shortcut_modifiers(&event.keystroke)
            .is_ok_and(|mods| shortcut.matches_release(&event.keystroke.key, mods))
        {
            return;
        }
        self.apply_preference(Preference::Shortcut(shortcut), cx);
    }

    pub(crate) fn reload_settings(&mut self) {
        match Config::load() {
            Ok(config) => {
                if let Some(previous) = &self.settings.config {
                    if previous.llm.autodl_key() != config.llm.autodl_key() {
                        self.settings.model_tests.remove(&ModelTestTarget::Autodl);
                        #[cfg(any(target_os = "macos", windows))]
                        self.settings.autodl_catalog.clear_listing();
                    }
                    if serde_yaml::to_value(&previous.engine.volc_bigmodel).ok()
                        != serde_yaml::to_value(&config.engine.volc_bigmodel).ok()
                    {
                        self.settings.model_tests.remove(&ModelTestTarget::Volc);
                    }
                    #[cfg(any(target_os = "macos", windows))]
                    if remote_editor_values(previous) != remote_editor_values(&config)
                        || previous.engine.remote.protocol != config.engine.remote.protocol
                    {
                        self.settings.model_tests.remove(&ModelTestTarget::Remote);
                    }
                }
                self.organizer_enabled = config.llm.enabled;
                self.settings.config = Some(config);
            }
            Err(_) => {
                self.settings.model_tests.clear();
                #[cfg(any(target_os = "macos", windows))]
                self.settings.autodl_catalog.clear_listing();
                self.organizer_enabled = false;
                self.settings.config = None;
                self.settings.notice =
                    Some(Err("无法读取配置，请检查 config.yaml 后重试。".into()));
            }
        }
    }

    pub(crate) fn settings_notice(&self) -> Option<&Result<String, String>> {
        self.settings.notice.as_ref().or_else(|| {
            if self.page != Page::Home {
                return None;
            }
            let target = self.settings.last_model_test.as_ref()?;
            self.settings
                .model_tests
                .get(target)
                .map(|feedback| &feedback.notice)
        })
    }

    #[cfg(any(target_os = "macos", windows))]
    fn begin_model_test(&mut self, target: ModelTestTarget, check: ServiceCheck, message: &str) {
        self.settings.notice = None;
        self.settings.checking = Some(check);
        self.settings.last_model_test = Some(target.clone());
        if matches!(target, ModelTestTarget::Local(_)) {
            self.model_picker.unload_feedback = None;
        }
        self.settings
            .model_tests
            .insert(target, ModelTestFeedback::notice(Ok(message.into())));
    }

    #[cfg(any(target_os = "macos", windows))]
    fn finish_model_test(&mut self, target: ModelTestTarget, feedback: ModelTestFeedback) {
        self.settings.last_model_test = Some(target.clone());
        self.settings.model_tests.insert(target, feedback);
    }

    pub(super) fn can_release_local_model(&self) -> bool {
        !self.recognition_model_busy()
            && !self.tour.preparing
            && self.model_switch_blocker().is_none()
    }

    pub(crate) fn release_local_model(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if !self.can_release_local_model()
            || target
                .as_deref()
                .is_some_and(|id| self.model_picker.selected_model() != Some(id))
        {
            return;
        }
        if target.is_some() {
            self.settings.notice = None;
            self.model_picker.unload_feedback = None;
        }
        self.model_picker.switching_model = target.clone();
        self.model_picker.switching = Some("正在卸载本地模型…".into());
        let task = cx.background_spawn(async {
            crate::model_packs::release()?;
            #[cfg(windows)]
            if crate::local_models::has_owned_manager() {
                crate::local_models::release()?;
            }
            Ok::<_, String>(())
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.model_picker.switching = None;
                view.model_picker.switching_model = None;
                view.refresh_model_selection();
                if result.is_ok() {
                    view.model_picker.note_model_unloaded();
                }
                let feedback = result
                    .map(|()| "已卸载模型。下次听写或测试时会自动加载。".to_owned())
                    .map_err(|error| format!("卸载本地模型失败：{error}"));
                if target.is_some() && target.as_deref() == view.model_picker.selected_model() {
                    if view.page == Page::Settings {
                        view.settings.notice = Some(feedback.clone());
                    }
                    view.model_picker.unload_feedback = Some(feedback);
                } else if feedback.is_err() {
                    view.settings.notice = Some(feedback);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn theme_switch(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        use crate::theme::{self, Theme};
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .p(px(2.))
            .rounded(px(8.))
            .bg(rgb(crate::RAIL()))
            .children([Theme::Dark, Theme::Light].map(|theme| {
                let label = match (compact, theme) {
                    (true, Theme::Dark) => "暗色",
                    (true, Theme::Light) => "浅色",
                    (false, Theme::Dark) => "暗色（Typeless）",
                    (false, Theme::Light) => "浅色（Speaky 绿）",
                };
                let id = match (compact, theme) {
                    (true, Theme::Dark) => "header-theme-dark",
                    (true, Theme::Light) => "header-theme-light",
                    (false, Theme::Dark) => "settings-theme-dark",
                    (false, Theme::Light) => "settings-theme-light",
                };
                button(label, theme::current() == theme)
                    .h(px(if compact { 23. } else { 28. }))
                    .px_2()
                    .text_size(px(if compact { 11. } else { 12. }))
                    .id(id)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.apply_preference(Preference::Theme(theme), cx);
                    }))
            }))
    }

    fn apply_preference(&mut self, preference: Preference, cx: &mut Context<Self>) {
        self.settings.notice = None;
        if let Err(error) = self.update_preference(preference, cx) {
            self.settings.notice = Some(Err(error));
        }
        cx.notify();
    }

    fn update_preference(
        &mut self,
        preference: Preference,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let selected_theme = match &preference {
            Preference::Theme(theme) => Some(*theme),
            _ => None,
        };
        #[cfg(any(target_os = "macos", windows))]
        let changing_service = matches!(
            &preference,
            Preference::Service(_, false)
                | Preference::Framework(_, _)
                | Preference::Remote { .. }
                | Preference::Autodl { .. }
        );
        #[cfg(windows)]
        let changing_service = changing_service || matches!(&preference, Preference::Volc(_));
        #[cfg(not(any(target_os = "macos", windows)))]
        let changing_service = false;
        #[cfg(windows)]
        let owned_gpu = crate::local_models::has_owned_manager();
        #[cfg(not(windows))]
        let owned_gpu = false;
        let leaving_local = changing_service
            && (self.settings.config.as_ref().is_some_and(|config| {
                crate::engines::uses_local_asr(config) || crate::engines::uses_gemini_asr(config)
            }) || owned_gpu);
        if let Some(reason) = changing_service
            .then(|| self.model_switch_blocker())
            .flatten()
        {
            return Err(format!("{reason}。"));
        }
        let persist = || save(&Config::config_path(), &preference);
        let result = if selected_theme.is_none()
            && (self.recording.is_some() || self.transcribing || self.recognition_model_busy())
        {
            Err("正在录音、转写或切换模型，请稍后修改设置。".into())
        } else if let Preference::Shortcut(shortcut) = &preference {
            if let Some(hotkey) = &mut self.global_hotkey {
                hotkey.change(*shortcut, persist)
            } else {
                hotkey::GlobalHotkey::register(*shortcut).and_then(|(handle, presses)| {
                    persist()?;
                    listen_for_hotkey(presses, cx);
                    self.global_hotkey = Some(handle);
                    Ok(())
                })
            }
            .map(|()| self.hotkey_status = format!("全局快捷键 {}", shortcut.label()))
        } else {
            persist()
        };

        let changing_shortcut = matches!(preference, Preference::Shortcut(_));
        if changing_shortcut {
            self.settings.shortcut_error = result.as_ref().err().cloned();
        }
        let restore_shortcut = changing_shortcut && result.is_err();
        if result.is_ok() {
            if let Some(theme) = selected_theme {
                crate::theme::set(theme);
                #[cfg(windows)]
                crate::window_chrome::refresh_frames(cx);
                cx.refresh_windows();
            }
            if changing_shortcut {
                self.settings.shortcut_capture = None;
                self.tour_shortcut_changed();
            }
            let organizer_changed = match preference {
                Preference::OrganizerEnabled(_) => true,
                #[cfg(any(target_os = "macos", windows))]
                Preference::Service(_, organizer) => organizer,
                #[cfg(any(target_os = "macos", windows))]
                Preference::AutodlKey(_) | Preference::AutodlRewrite { .. } => true,
                _ => false,
            };
            if organizer_changed {
                self.settings.polish_report = None;
            }
            self.settings.picker = None;
            self.reload_settings();
            if changing_service {
                self.settings.clear_model_test_feedback();
                self.refresh_model_selection();
                self.redesign_config.try_download = false;
            }
            #[cfg(windows)]
            if matches!(preference, Preference::PreloadLocalModel(true)) {
                if let Some(config) = self.settings.config.clone() {
                    self.preload_selected_model(config, cx);
                }
            }
            if leaving_local
                && self.settings.config.as_ref().is_some_and(|config| {
                    !crate::engines::uses_local_asr(config)
                        && !crate::engines::uses_gemini_asr(config)
                })
            {
                self.release_local_model(None, cx);
            }
            #[cfg(any(target_os = "macos", windows))]
            if matches!(preference, Preference::Framework(_, _)) {
                self.resolve_framework_name(cx);
            }
        }
        #[cfg(windows)]
        let result = result.and_then(|()| match preference {
            Preference::FloatingButton(enabled) => self
                .set_floating_button_enabled(enabled, cx)
                .map_err(|error| format!("已保存设置，桌面悬浮按钮更新失败：{error}")),
            _ => Ok(()),
        });
        if restore_shortcut {
            self.cancel_shortcut_capture(cx);
        }
        result
    }

    #[cfg(any(target_os = "macos", windows))]
    fn edit_organizer_prompt(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(config) = &self.settings.config else {
            return;
        };
        let current = &config.llm.custom_prompt;
        let initial = if current.trim().is_empty() {
            crate::polish::Profile::Balanced.prompt()
        } else {
            current.clone()
        };
        let result = crate::terms_ui::edit_text(
            window,
            &initial,
            "编辑整理提示词",
            "自定义提示词用于之后的整理。留空并保存可恢复内置提示词。",
        );
        match result {
            Ok(receiver) => cx
                .spawn(async move |entity, cx| {
                    let Ok(result) = receiver.await else { return };
                    let _ = entity.update(cx, |view, cx| match result {
                        Ok(Some(prompt)) => {
                            view.apply_preference(Preference::CustomPrompt(prompt), cx)
                        }
                        Ok(None) => {}
                        Err(error) => {
                            view.settings.notice = Some(Err(error));
                            cx.notify();
                        }
                    });
                })
                .detach(),
            Err(error) => {
                self.settings.notice = Some(Err(error));
                cx.notify();
            }
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn edit_service(&mut self, window: &mut gpui::Window, organizer: bool, cx: &mut Context<Self>) {
        self.edit_service_preset(window, organizer, false, cx);
    }

    fn edit_deepseek(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.edit_service_preset(window, true, true, cx);
    }

    #[cfg(windows)]
    fn edit_volc(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(config) = &self.settings.config else {
            return;
        };
        match service_ui::edit_volc(window, &config.engine.volc_bigmodel.resource_id) {
            Ok(receive) => cx
                .spawn(async move |entity, cx| {
                    let Ok(answer) = receive.await else { return };
                    let _ = entity.update(cx, |v, cx| match answer {
                        Ok(Some(input)) => v.apply_preference(Preference::Volc(input), cx),
                        Ok(None) => {}
                        Err(e) => {
                            v.settings.notice = Some(Err(e));
                            cx.notify();
                        }
                    });
                })
                .detach(),
            Err(e) => {
                self.settings.notice = Some(Err(e));
                cx.notify();
            }
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn edit_service_preset(
        &mut self,
        window: &mut gpui::Window,
        organizer: bool,
        deepseek: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = &self.settings.config else {
            return;
        };
        let previous_engine = match engine_state(config) {
            Ok(state) => state,
            Err(error) => {
                self.settings.notice = Some(Err(error));
                cx.notify();
                return;
            }
        };
        let (base_url, model, saved_key) = if deepseek {
            (
                "https://api.deepseek.com/v1".into(),
                "deepseek-chat".into(),
                String::new(),
            )
        } else if organizer {
            let service = &config.llm.openai;
            (
                service.base_url.clone(),
                service.model.clone(),
                String::new(),
            )
        } else if let Some((_, draft)) = self
            .settings
            .pending_service
            .as_ref()
            .filter(|(engine, _)| engine == &previous_engine)
        {
            (
                draft.base_url.clone(),
                draft.model.clone(),
                draft.api_key.clone().unwrap_or_default(),
            )
        } else {
            remote_editor_values(config)
        };
        let editor = service_ui::edit_model(window, &base_url, &model, organizer);
        match editor {
            Ok(receiver) => cx
                .spawn(async move |entity, cx| {
                    let Ok(result) = receiver.await else { return };
                    let _ = entity.update(cx, |view, cx| match result {
                        Ok(Some(input)) if organizer => {
                            view.apply_preference(Preference::Service(input, true), cx)
                        }
                        Ok(Some(input)) => {
                            let key = submitted_key(&input, &base_url, &saved_key);
                            view.detect_recognition_service(input, key, previous_engine, cx)
                        }
                        Ok(None) => {}
                        Err(error) => {
                            view.settings.notice = Some(Err(error));
                            cx.notify();
                        }
                    });
                })
                .detach(),
            Err(error) => {
                self.settings.notice = Some(Err(error));
                cx.notify();
            }
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn detect_recognition_service(
        &mut self,
        input: service_ui::ServiceInput,
        key: String,
        previous_engine: Value,
        cx: &mut Context<Self>,
    ) {
        if self.settings.recognition_checking() {
            return;
        }
        self.settings.pending_service = Some((
            previous_engine.clone(),
            service_ui::ServiceInput {
                base_url: input.base_url.clone(),
                model: input.model.clone(),
                api_key: (!key.is_empty()).then(|| key.clone()),
            },
        ));
        self.begin_model_test(
            ModelTestTarget::Remote,
            ServiceCheck::Detect,
            "正在识别接口并转写内置样本…",
        );
        let task = cx.background_spawn(async move {
            let timer = std::time::Instant::now();
            crate::engines::detect_service(input.base_url.trim(), input.model.trim(), &key)
                .map(|(service, text)| (service, text, timer.elapsed().as_secs_f64()))
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.settings.checking = None;
                let feedback = match result {
                    Ok((service, text, elapsed)) => {
                        match view.update_preference(
                            Preference::Remote {
                                service,
                                previous_engine,
                            },
                            cx,
                        ) {
                            Ok(()) => {
                                view.settings.pending_service = None;
                                view.refresh_model_selection();
                                ModelTestFeedback::sample("已启用并通过测试", elapsed, &text)
                            }
                            Err(error) => {
                                view.reload_settings();
                                ModelTestFeedback::notice(Err(error))
                            }
                        }
                    }
                    Err(error) => ModelTestFeedback::notice(Err(error)),
                };
                view.finish_model_test(ModelTestTarget::Remote, feedback);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[cfg(any(target_os = "macos", windows))]
    fn check_autodl(&mut self, activate: bool, cx: &mut Context<Self>) {
        if self.settings.recognition_checking() || self.model_switch_blocker().is_some() {
            return;
        }
        let setup = Config::load()
            .map_err(|error| error.to_string())
            .and_then(|config| {
                let service = crate::engines::autodl_asr_service(&config)?;
                Ok((service, engine_state(&config)?))
            });
        let (service, previous_engine) = match setup {
            Ok(setup) => setup,
            Err(error) => {
                self.finish_model_test(
                    ModelTestTarget::Autodl,
                    ModelTestFeedback::notice(Err(error)),
                );
                cx.notify();
                return;
            }
        };
        self.begin_model_test(
            ModelTestTarget::Autodl,
            ServiceCheck::Sample,
            "正在用 AutoDL 转写内置中文样本…",
        );
        let task = cx.background_spawn(async move {
            let timer = std::time::Instant::now();
            let result = crate::engines::check_service(&service);
            (service, result, timer.elapsed().as_secs_f64())
        });
        cx.spawn(async move |entity, cx| {
            let (service, result, elapsed) = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.settings.checking = None;
                let result = result.and_then(|text| {
                    let current = Config::load().map_err(|error| error.to_string())?;
                    if crate::engines::autodl_asr_service(&current)? != service {
                        return Err("检测期间 AutoDL 配置已变更，请重新检测。".into());
                    }
                    let prefix = if activate {
                        view.update_preference(
                            Preference::Autodl {
                                service,
                                previous_engine,
                            },
                            cx,
                        )?;
                        "已启用并通过测试"
                    } else {
                        "测试通过"
                    };
                    Ok(ModelTestFeedback::sample(prefix, elapsed, &text))
                });
                view.finish_model_test(
                    ModelTestTarget::Autodl,
                    result.unwrap_or_else(|error| ModelTestFeedback::notice(Err(error))),
                );
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn check_recognition_service(&mut self, cx: &mut Context<Self>) {
        if self.settings.recognition_checking()
            || self.recognition_model_busy()
            || self.model_picker.preloading
        {
            return;
        }
        self.reload_settings();
        let Some(config) = self.settings.config.clone() else {
            return;
        };
        let previous_engine = engine_state(&config);
        let target = ModelTestTarget::selected(&config);
        self.begin_model_test(
            target.clone(),
            ServiceCheck::Sample,
            "正在转写内置中文样本…",
        );
        let load = if let ModelTestTarget::Local(Some(id)) = &target {
            Some(self.begin_model_loading(id.clone()))
        } else {
            None
        };
        let (engine_ready, engine_prepared) = tokio::sync::oneshot::channel();
        let worker = load.clone();
        let task = cx.background_spawn(async move {
            let timer = std::time::Instant::now();
            let text = if config.engine.current == "remote" {
                crate::engines::check_service(&config.engine.remote)?
            } else {
                let engine = match &worker {
                    Some(load) => crate::engines::recognition_engine_cancellable(&config, load)?,
                    None => crate::engines::recognition_engine(&config)?,
                };
                let _ = engine_ready.send(());
                engine
                    .transcribe(SAMPLE_AUDIO, "zh")
                    .map_err(|error| error.to_string())?
            };
            if text.trim().is_empty() {
                return Err("服务返回空白转写，请检查模型后重试。".into());
            }
            Ok(ModelTestFeedback::sample(
                "测试通过",
                timer.elapsed().as_secs_f64(),
                &text,
            ))
        });
        cx.spawn(async move |entity, cx| {
            if load.is_some() {
                let prepared = engine_prepared.await.is_ok();
                let _ = entity.update(cx, |view, cx| {
                    view.model_picker.finish_loading();
                    if prepared {
                        view.model_picker.note_model_loaded();
                    }
                    cx.notify();
                });
            }
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.settings.checking = None;
                let current = Config::load()
                    .map_err(|e| e.to_string())
                    .and_then(|c| engine_state(&c));
                let feedback = if load.is_some_and(|load| load.is_cancelled()) {
                    let message = crate::model_picker::stopped_model_load_message(&result);
                    view.model_picker.load_feedback = Some(message.clone());
                    ModelTestFeedback::notice(Ok(message))
                } else if current.is_ok() && current == previous_engine {
                    result.unwrap_or_else(|error| ModelTestFeedback::notice(Err(error)))
                } else {
                    view.reload_settings();
                    ModelTestFeedback::notice(Err("识别配置已变更，请重新检测。".into()))
                };
                view.finish_model_test(target, feedback);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn v30_settings(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.recording.is_some() || self.transcribing || self.recognition_model_busy();
        let head = page_heading("设置", "")
            .when(busy, |head| head.child(detail("录音和转写结束后可修改。")));
        let body = div().max_w(px(740.)).flex().flex_col().gap_4().child(head);
        let Some(config) = &self.settings.config else {
            return body.child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .child(detail("无法读取配置，请检查 config.yaml。"))
                    .child(
                        button("重新读取", false).id("settings-reload").on_click(
                            cx.listener(|view, _, _, cx| view.open_page(Page::Settings, cx)),
                        ),
                    )
                    .children(self.tour.mark(Anchor::SettingsError)),
            );
        };
        let asr = &config.core.asr;
        let shortcut = selected_shortcut(config);
        let inputs = self.diagnostic_input_devices();
        let available_device = inputs.and_then(|inputs| {
            inputs.configured_name(asr.audio_device, asr.audio_device_name.as_deref())
        });
        let saved_device = available_device
            .or(asr.audio_device_name.as_deref())
            .map(str::to_owned)
            .or_else(|| asr.audio_device.map(|index| format!("输入设备 {index}")));
        let unavailable_device = saved_device
            .as_deref()
            .filter(|_| inputs.is_some() && available_device.is_none());
        let device_label = if unavailable_device.is_some() {
            "不可用"
        } else {
            saved_device.as_deref().unwrap_or("系统默认")
        };
        let use_default = asr.audio_device_name.is_none() && asr.audio_device.is_none();
        let device_options = std::iter::once((None, "系统默认".to_owned())).chain(
            inputs.into_iter().flat_map(|inputs| {
                inputs
                    .available()
                    .iter()
                    .map(|(_, name)| (Some(name.clone()), name.clone()))
            }),
        );
        let language = if asr.language.is_empty() {
            "auto"
        } else {
            asr.language
                .split(['-', '_'])
                .next()
                .unwrap_or(&asr.language)
        };
        let language_label = LANGUAGES
            .iter()
            .find(|(id, _)| *id == language)
            .map_or(asr.language.as_str(), |(_, label)| *label);
        let shortcut_section = section(
            "快捷键",
            [self
                .shortcut_row(shortcut, busy, cx)
                .relative()
                .children(self.tour.mark(Anchor::Shortcut))],
        );
        let language_section = section(
            "语言",
            [self
                .setting_picker(Picker::Language, "识别语言", None, language_label, busy, cx)
                .relative()
                .children(self.tour.mark(Anchor::Language))
                .when(
                    !busy && self.settings.picker == Some(Picker::Language),
                    |row| {
                        row.child(options(LANGUAGES.map(|(id, label)| {
                            button(label, id == language).id(id).on_click(cx.listener(
                                move |view, _, _, cx| {
                                    view.apply_preference(Preference::Language(id), cx)
                                },
                            ))
                        })))
                    },
                )],
        );
        let audio_section = section(
            "音频",
            [
                self.setting_picker(
                    Picker::Device,
                    "输入设备",
                    unavailable_device,
                    device_label,
                    busy,
                    cx,
                )
                .border_t_1()
                .border_color(rgb(BORDER()))
                .when(
                    !busy && self.settings.picker == Some(Picker::Device),
                    |row| {
                        row.child(options(device_options.enumerate().map(
                            |(index, (id, label))| {
                                let selected = match id.as_deref() {
                                    None => use_default,
                                    Some(name) => available_device == Some(name),
                                };
                                button(label, selected)
                                    .id(("device", index))
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.apply_preference(Preference::Device(id.clone()), cx)
                                    }))
                            },
                        )))
                    },
                )
                .relative()
                .children(self.tour.mark(Anchor::Input)),
                self.input_status(config, cx).px_4().py_2(),
            ],
        );
        let general = section(
            "常规",
            [
                setting_row("外观", None, self.theme_switch(false, cx)),
                #[cfg(windows)]
                setting_row(
                    "登录时启动应用",
                    None,
                    div().child(
                        button("系统启动设置", false)
                            .id("windows-startup-settings")
                            .on_click(|_, _, cx| cx.open_url("ms-settings:startupapps")),
                    ),
                ),
                #[cfg(windows)]
                {
                    let enabled = config.desktop.windows_floating_button;
                    setting_row(
                        "桌面悬浮按钮",
                        Some("单击开始听写，再点一次结束。拖动按钮可调整位置。"),
                        div().child(
                            button(
                                if enabled {
                                    "隐藏按钮"
                                } else {
                                    "显示按钮"
                                },
                                false,
                            )
                            .id("toggle-floating-button")
                            .when(busy, |control| control.opacity(0.5))
                            .when(!busy, |control| {
                                control.on_click(cx.listener(move |view, _, _, cx| {
                                    view.apply_preference(Preference::FloatingButton(!enabled), cx)
                                }))
                            }),
                        ),
                    )
                    .relative()
                    .children(self.tour.mark(Anchor::Floating))
                    .border_t_1()
                    .border_color(rgb(BORDER()))
                },
            ],
        );
        body.child(shortcut_section)
            .child(language_section)
            .child(audio_section)
            .child(general)
    }

    pub(super) fn guide_show_model_tab(&mut self, rewrite: bool, cx: &mut Context<Self>) {
        self.redesign_config.rewrite = rewrite;
        if self.page == Page::Models {
            self.tour.request_reveal();
            cx.notify();
        } else {
            self.open_page(Page::Models, cx);
        }
    }

    pub(super) fn guide_model_ready(&self) -> bool {
        self.settings
            .config
            .as_ref()
            .is_some_and(|config| self.guide_asr_status(config).1)
    }

    pub(super) fn guide_model_status(&self) -> (String, String) {
        let Some(config) = self.settings.config.as_ref() else {
            return ("无法读取识别配置".into(), "无法读取改写配置".into());
        };
        let asr_label = self.model_picker.model_label().trim();
        let (asr, _) = self.guide_asr_status(config);
        let asr = if asr_label.is_empty() {
            asr
        } else {
            format!("{asr_label} · {asr}")
        };
        let rewrite = if !config.llm.enabled {
            "已关闭".into()
        } else {
            let status = if self.settings.polish_checking {
                "正在测试"
            } else if !rewrite_configured(config) {
                "已开启，待配置"
            } else {
                match self.settings.polish_report.as_ref() {
                    Some(Ok(_)) => "已开启，已测试",
                    Some(Err(_)) => "已开启，测试失败",
                    None => "已开启，未测试",
                }
            };
            let service = config.llm.organizer_service();
            let label = service.model.trim();
            if label.is_empty() {
                status.into()
            } else {
                format!("{label} · {status}")
            }
        };
        (asr, rewrite)
    }

    fn guide_asr_status(&self, config: &Config) -> (String, bool) {
        let picker = &self.model_picker;
        if let Some(notice) = picker
            .download_notice()
            .or_else(|| picker.switching_notice(false).map(str::to_owned))
        {
            return (notice, false);
        }
        if picker.preloading {
            return ("正在加载".into(), false);
        }
        if self.settings.recognition_checking() {
            return ("正在测试".into(), false);
        }
        let local = crate::engines::uses_local_asr(config);
        if local && picker.unload_feedback.as_ref().is_some_and(Result::is_ok) {
            return ("已卸载，使用时加载".into(), false);
        }
        let feedback = self
            .settings
            .model_tests
            .get(&ModelTestTarget::selected(config));
        if let Some(feedback) = feedback {
            if feedback.notice.is_err() {
                return ("测试失败，可重试".into(), false);
            }
            if feedback.metrics.is_some() {
                return ("测试通过".into(), true);
            }
        }
        if local {
            return (
                picker.guide_local_model_status().into(),
                picker.selected_model_loaded(),
            );
        }
        let status = if external_asr_configured(config) {
            "已配置，未测试"
        } else {
            "待配置"
        };
        (status.into(), false)
    }

    pub(crate) fn v30_models(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        use crate::redesign_config::{action, catalog, header, model_row};
        let rewrite = self.redesign_config.rewrite;
        if !rewrite && self.model_picker.animates_loading() {
            window.request_animation_frame();
        }
        let busy = self.recording.is_some()
            || self.starting
            || self.transcribing
            || self.recognition_model_busy()
            || self.settings.recognition_checking();
        let tabs = div().flex().items_center().flex_wrap().gap_2().children(
            [
                (false, "models-tab-asr", "语音转文字"),
                (true, "models-tab-llm", "AI 润色"),
            ]
            .into_iter()
            .map(|(show_rewrite, id, title)| {
                button(title, rewrite == show_rewrite)
                    .id(id)
                    .relative()
                    .when(!show_rewrite, |button| {
                        button.children(self.tour.mark(Anchor::Model))
                    })
                    .when(show_rewrite && !rewrite, |button| {
                        button.children(self.tour.mark(Anchor::Rewrite))
                    })
                    .focusable()
                    .tab_stop(true)
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.guide_show_model_tab(show_rewrite, cx);
                    }))
            }),
        );
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(page_heading("模型选择", ""));
        let Some(config) = &self.settings.config else {
            return page.child(detail("无法读取模型配置。")).child(
                button("重试", false)
                    .id("models-reload")
                    .relative()
                    .children(self.tour.mark(Anchor::SettingsError))
                    .on_click(cx.listener(|v, _, _, cx| v.open_page(Page::Models, cx))),
            );
        };
        page = page.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(tabs)
                .child(
                    button("+ 连接自定义 API", false)
                        .id("catalog-add-service")
                        .when(!busy && !self.settings.polish_checking, |b| {
                            b.on_click(cx.listener(move |v, _, window, cx| {
                                v.edit_service(window, rewrite, cx)
                            }))
                        }),
                ),
        );
        if rewrite {
            let enabled = config.llm.enabled;
            let llm = &config.llm.openai;
            let configured = rewrite_configured(config);
            let hosted_on_autodl =
                autodl_api::is_base_url(&config.llm.organizer_service().base_url);
            let custom_configured = !autodl_api::is_base_url(&llm.base_url)
                && model_service_configured(&llm.base_url, &llm.model, &llm.api_key);
            let endpoint = reqwest::Url::parse(&llm.base_url).ok();
            let deepseek = endpoint.as_ref().and_then(|u| u.host_str()) == Some("api.deepseek.com");
            let mut toggle = button(
                if enabled {
                    "已开启改写"
                } else {
                    "已关闭改写 · 建议开启"
                },
                enabled,
            )
            .id("catalog-toggle-rewrite")
            .relative()
            .children(self.tour.mark(Anchor::Rewrite))
            .when(!busy && !self.settings.polish_checking, |toggle| {
                toggle.focusable().tab_stop(true)
            });
            if !busy && !self.settings.polish_checking {
                toggle = toggle.on_click(cx.listener(move |v, _, window, cx| {
                    if enabled || configured {
                        v.apply_preference(Preference::OrganizerEnabled(!enabled), cx);
                    } else if hosted_on_autodl {
                        v.edit_autodl_key(window, cx);
                    } else {
                        v.edit_service(window, true, cx);
                    }
                }));
            }
            let acts = div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(
                    action("配置")
                        .id("catalog-edit-llm")
                        .when(!busy && !self.settings.polish_checking, |b| {
                            b.on_click(cx.listener(|v, _, w, cx| v.edit_service(w, true, cx)))
                        }),
                )
                .when(custom_configured && hosted_on_autodl, |row| {
                    let base_url = llm.base_url.clone();
                    let model = llm.model.clone();
                    row.child(action("使用").id("catalog-use-saved-rewrite").when(
                        !busy && !self.settings.polish_checking,
                        |b| {
                            b.on_click(cx.listener(move |view, _, _, cx| {
                                view.apply_preference(
                                    Preference::Service(
                                        service_ui::ServiceInput {
                                            base_url: base_url.clone(),
                                            model: model.clone(),
                                            api_key: None,
                                        },
                                        true,
                                    ),
                                    cx,
                                );
                            }))
                        },
                    ))
                })
                .when(custom_configured && !hosted_on_autodl, |row| {
                    row.child(
                        action(if self.settings.polish_checking {
                            "测试中…"
                        } else {
                            "测试连接"
                        })
                        .id("catalog-test-llm")
                        .when(!busy && !self.settings.polish_checking, |b| {
                            b.on_click(cx.listener(|v, _, _, cx| v.benchmark(cx)))
                        }),
                    )
                });
            let mut table = catalog()
                .child(header("质量", "适合"))
                .child(self.autodl_rewrite_row(config, busy, cx));
            if !deepseek {
                table = table.child(model_row(
                    "DeepSeek".into(),
                    false,
                    false,
                    "",
                    "Chat Completion 兼容服务",
                    "整理与标点",
                    None,
                    div().child(
                        action("配置连接")
                            .id("catalog-configure-deepseek")
                            .when(!busy && !self.settings.polish_checking, |b| {
                                b.on_click(cx.listener(|v, _, w, cx| v.edit_deepseek(w, cx)))
                            }),
                    ),
                ));
            }
            table = table.child(model_row(
                if deepseek {
                    format!("DeepSeek · {}", llm.model)
                } else if custom_configured {
                    llm.model.clone()
                } else {
                    "自定义 OpenAI 兼容接口".into()
                },
                false,
                enabled && !hosted_on_autodl,
                if custom_configured {
                    "已配置"
                } else {
                    "待配置"
                },
                endpoint
                    .as_ref()
                    .and_then(|u| u.host_str())
                    .filter(|_| !autodl_api::is_base_url(&llm.base_url))
                    .unwrap_or("Chat Completion 兼容接口")
                    .to_owned(),
                "文字改写",
                None,
                acts,
            ));
            page = page
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(detail(
                            "开启后，转写原文会发送给所选服务。改写失败时保留原文。",
                        ))
                        .child(toggle),
                )
                .child(table.when(!enabled, |t| t.opacity(0.78)))
                .child(self.benchmark_result())
                .child(
                    action("改写偏好与提示词")
                        .id("catalog-rewrite-preferences")
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.redesign_config.rewrite_preferences =
                                !v.redesign_config.rewrite_preferences;
                            cx.notify();
                        })),
                )
                .when(self.redesign_config.rewrite_preferences, |v| {
                    v.child(self.organizer_section(config, busy, cx))
                });
        } else {
            let current = config.engine.current.as_str();
            let local = crate::engines::uses_local_asr(config);
            let catalog_header = catalog()
                .child(header("样本比对", "特点"))
                .child(self.autodl_model_row(config, busy, cx));
            let mut table = catalog_header.child(self.v30_local_rows(cx));
            let has_volc = volc_asr_configured(config);
            let active_volc = current == "volc_bigmodel" && has_volc;
            let mut volc_actions = div().flex().gap_1();
            if active_volc {
                volc_actions = volc_actions.child(self.v30_test_asr("catalog-test-volc", busy, cx));
            }
            #[cfg(windows)]
            {
                volc_actions =
                    volc_actions.child(action("配置").id("catalog-edit-volc").when(!busy, |b| {
                        b.on_click(cx.listener(|v, _, w, cx| v.edit_volc(w, cx)))
                    }));
            }
            table = table.child(model_row(
                "火山 Doubao-语音识别".into(),
                false,
                active_volc,
                if active_volc {
                    "已选择"
                } else if has_volc {
                    "已配置"
                } else {
                    "待配置"
                },
                "火山引擎流式语音识别",
                "流式听写",
                self.settings.model_tests.get(&ModelTestTarget::Volc),
                volc_actions,
            ));
            let remote_active = !local
                && (matches!(current, "remote" | "openai")
                    || crate::config::Framework::selected(config).is_some());
            let remote = &config.engine.remote;
            let name = if remote_active {
                crate::model_picker::selected_label(config)
            } else if !remote.address.is_empty() {
                remote.label()
            } else {
                "自定义识别服务".into()
            };
            let mut acts = div().flex().gap_1().child(
                action("配置").id("catalog-edit-asr").when(!busy, |b| {
                    b.on_click(cx.listener(|v, _, w, cx| v.edit_service(w, false, cx)))
                }),
            );
            if remote_active {
                acts = acts.child(self.v30_test_asr("catalog-test-remote", busy, cx));
            } else if !remote.address.is_empty() {
                acts = acts.child(
                    action("使用并测试")
                        .id("catalog-use-remote")
                        .when(!busy, |b| {
                            b.on_click(cx.listener(|v, _, _, cx| {
                                let Some(config) = v.settings.config.as_ref() else {
                                    return;
                                };
                                let remote = &config.engine.remote;
                                let Ok(engine) = engine_state(config) else {
                                    return;
                                };
                                v.detect_recognition_service(
                                    service_ui::ServiceInput {
                                        base_url: remote.address.clone(),
                                        model: remote.model.clone(),
                                        api_key: None,
                                    },
                                    remote.api_key.clone(),
                                    engine,
                                    cx,
                                );
                            }))
                        }),
                );
            }
            table = table.child(model_row(
                name,
                false,
                remote_active,
                if remote_active {
                    "已选择"
                } else if !remote.address.is_empty() {
                    "已配置"
                } else {
                    "待配置"
                },
                if current == "remote" || !remote.address.is_empty() {
                    remote.protocol.label()
                } else {
                    "OpenAI 兼容转写、FunASR、whisper.cpp、AutoDL Gemini"
                },
                if remote.protocol == crate::engines::Protocol::Gemini {
                    "停录时发送音频至 AutoDL"
                } else {
                    "服务转写"
                },
                self.settings.model_tests.get(&ModelTestTarget::Remote),
                acts,
            ));
            page = page.child(table);
            if self.redesign_config.try_download && !self.tour.is_active() {
                page = page.child(
                    crate::card().child(detail("模型下载与校验完成。")).child(
                        button("立刻试用", true)
                            .id("catalog-try-download")
                            .when(!busy, |b| {
                                b.on_click(cx.listener(|v, _, _, cx| {
                                    v.redesign_config.try_download = false;
                                    v.open_page(Page::Home, cx);
                                    v.toggle_recording(crate::RecordingStart::Button, cx);
                                }))
                            }),
                    ),
                );
            }
            let expanded = self.redesign_config.advanced;
            page = page.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        action(if expanded {
                            "高级（离线导入 / 导出） ▴"
                        } else {
                            "高级（离线导入 / 导出） ▾"
                        })
                        .id("catalog-advanced")
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.redesign_config.advanced = !expanded;
                            cx.notify();
                        })),
                    )
                    .child(
                        action("刷新本地模型")
                            .id("catalog-rescan")
                            .when(!busy, |b| {
                                b.on_click(cx.listener(|v, _, _, cx| v.scan_models(cx)))
                            }),
                    ),
            );
            if expanded {
                page = page
                    .child(section(
                        "离线模型",
                        [setting_row(
                            "导入与导出",
                            Some("复制完整模型包目录即可迁移。导入后刷新本地模型。"),
                            div().child(
                                action("打开模型包目录")
                                    .id("catalog-open-pack-directory")
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        match crate::model_packs::prepare_directory() {
                                            Ok(path) => cx.reveal_path(&path),
                                            Err(e) => {
                                                v.settings.notice = Some(Err(e));
                                                cx.notify();
                                            }
                                        }
                                    })),
                            ),
                        )],
                    ))
                    .child(self.advanced_asr(config, busy, cx));
                #[cfg(windows)]
                {
                    page = page.child(
                        action("识别评测")
                            .id("catalog-benchmark")
                            .on_click(cx.listener(|v, _, _, cx| v.open_page(Page::Benchmark, cx))),
                    );
                }
            }
        }
        page
    }

    fn autodl_model_row(&self, config: &Config, busy: bool, cx: &mut Context<Self>) -> Div {
        use crate::redesign_config::{action, model_row};

        let configured = crate::engines::autodl_asr_service(config).is_ok();
        let active = config.engine.current == "autodl";
        let can_test = !busy && configured;
        let actions =
            div()
                .flex()
                .gap_1()
                .when(!configured, |row| {
                    row.child(action("配置 API Key").id("catalog-autodl-key").when(
                        !busy && !self.settings.polish_checking,
                        |b| {
                            b.on_click(
                                cx.listener(|view, _, window, cx| view.edit_autodl_key(window, cx)),
                            )
                        },
                    ))
                })
                .when(configured, |row| {
                    row.child(action("测试样本").id("catalog-test-autodl").when(
                        can_test,
                        |button| {
                            button.on_click(
                                cx.listener(|view, _, _, cx| view.check_autodl(false, cx)),
                            )
                        },
                    ))
                })
                .when(configured && !active, |row| {
                    row.child(action("测试并使用").id("catalog-use-autodl").when(
                        can_test,
                        |button| {
                            button
                                .on_click(cx.listener(|view, _, _, cx| view.check_autodl(true, cx)))
                        },
                    ))
                });
        model_row(
            crate::engines::AUTODL_ASR_MODEL.to_owned(),
            false,
            active,
            if !configured {
                "待配置"
            } else if active {
                "已选择"
            } else {
                "已配置"
            },
            "",
            "云端转写",
            self.settings.model_tests.get(&ModelTestTarget::Autodl),
            actions,
        )
    }

    fn v30_test_asr(
        &self,
        id: &'static str,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        crate::redesign_config::action(if self.settings.recognition_checking() {
            "检测中…"
        } else {
            "测试连接"
        })
        .id(id)
        .when(!busy, |b| {
            b.on_click(cx.listener(|v, _, _, cx| v.check_recognition_service(cx)))
        })
    }

    fn advanced_asr(&self, config: &Config, busy: bool, cx: &mut Context<Self>) -> Div {
        let local = crate::engines::uses_local_asr(config);
        let framework = crate::config::Framework::selected(config);
        let remote = &config.engine.remote;
        let volc = &config.engine.volc_bigmodel;
        let volc_configured = config.engine.current == "volc_bigmodel"
            && (!volc.api_key.is_empty()
                || (!volc.app_key.is_empty() && !volc.access_key.is_empty())
                || std::env::var_os("SPEAKY_VOLC_API_KEY").is_some_and(|key| !key.is_empty()));
        let configured = local
            || volc_configured
            || framework.is_some()
            || matches!(
                config.engine.current.as_str(),
                "remote" | "openai" | "autodl"
            );
        let service_summary = if config.engine.current == "autodl" {
            crate::engines::AUTODL_ASR_MODEL.to_owned()
        } else if config.engine.current == "remote" {
            if remote.model.is_empty() {
                format!("{} · {}", remote.protocol.label(), remote.endpoint)
            } else {
                format!(
                    "{} · {} · {}",
                    remote.protocol.label(),
                    remote.model,
                    remote.endpoint
                )
            }
        } else if let Some(f) = framework {
            let profile = f.service(config);
            format!(
                "OpenAI 兼容转写 · {} · {}",
                f.display_model(&profile.model),
                profile.base_url
            )
        } else if local {
            #[cfg(windows)]
            let model = crate::model_picker::selected_label(config);
            #[cfg(not(windows))]
            let model = config
                .engine
                .local
                .as_ref()
                .map(|s| s.model.clone())
                .unwrap_or_default();
            format!("本机 GPU · {model}")
        } else if volc_configured {
            "火山引擎".into()
        } else if configured {
            let service = &config.engine.openai;
            format!("OpenAI 兼容转写 · {} · {}", service.model, service.base_url)
        } else {
            "填写服务地址后自动检测".into()
        };
        let mut actions = div().flex().flex_wrap().justify_end().gap_1().flex_none();
        let mut sample_action = div().flex_none();
        #[cfg(any(target_os = "macos", windows))]
        {
            let enabled = !busy && !self.settings.recognition_checking();
            actions = actions.child(
                button(
                    if self.settings.checking == Some(ServiceCheck::Detect) {
                        "检测中…"
                    } else {
                        "配置服务…"
                    },
                    false,
                )
                .id("settings-edit-service")
                .when(enabled, |button| {
                    button.on_click(
                        cx.listener(|view, _, window, cx| view.edit_service(window, false, cx)),
                    )
                }),
            );
            sample_action = sample_action.child(
                button(
                    if self.settings.checking == Some(ServiceCheck::Sample) {
                        "检测中…"
                    } else {
                        "检测可用性"
                    },
                    false,
                )
                .id("settings-transcribe-sample")
                .when(enabled, |button| {
                    button
                        .on_click(cx.listener(|view, _, _, cx| view.check_recognition_service(cx)))
                }),
            );
        }
        let recognition = section(
            "识别",
            [
                setting_row("接入方式", Some(&service_summary), actions)
                    .relative()
                    .children(self.tour.mark(Anchor::Service)),
                div().when(local, |row| {
                    let can_unload = !busy && self.can_release_local_model();
                    let target = crate::model_picker::selected_id(config).map(str::to_owned);
                    row.child(setting_row(
                        "本地模型包",
                        None,
                        div().child(
                            button("释放本地模型", false)
                                .id("settings-release-local-model")
                                .when(!can_unload, |button| button.opacity(0.5))
                                .when(can_unload, |button| {
                                    button.on_click(cx.listener(move |view, _, _, cx| {
                                        view.release_local_model(target.clone(), cx);
                                    }))
                                }),
                        ),
                    ))
                }),
                {
                    let enabled = config.desktop.preload_local_model;
                    let description = self
                        .model_picker
                        .preload_notice()
                        .filter(|_| {
                            enabled
                                && local
                                && (self.page != Page::Models || !self.model_picker.preloading)
                        })
                        .unwrap_or("在 Speaky 启动时提前加载选中的本地识别模型。");
                    setting_row(
                        "启动时预加载模型",
                        Some(description),
                        div().child(
                            button(if enabled { "已开启" } else { "已关闭" }, enabled)
                                .id("settings-preload-local-model")
                                .when(busy, |control| control.opacity(0.5))
                                .when(!busy, |control| {
                                    control.on_click(cx.listener(move |view, _, _, cx| {
                                        view.apply_preference(
                                            Preference::PreloadLocalModel(!enabled),
                                            cx,
                                        )
                                    }))
                                }),
                        ),
                    )
                    .border_t_1()
                    .border_color(rgb(BORDER()))
                },
                div().when(configured, |row| {
                    row.border_t_1()
                        .border_color(rgb(BORDER()))
                        .child(setting_row(
                            "内置中文样本",
                            Some(&format!("参考文字：{SAMPLE_REFERENCE}")),
                            sample_action,
                        ))
                }),
            ],
        );
        recognition
    }

    fn organizer_section(&self, config: &Config, busy: bool, cx: &mut Context<Self>) -> Div {
        let llm = &config.llm;
        let service = llm.organizer_service();
        let hosted_on_autodl = autodl_api::is_base_url(&service.base_url);
        let checking = self.settings.polish_checking;
        let custom = !llm.custom_prompt.trim().is_empty();
        let summary = if llm.enabled {
            format!(
                "{} · {}",
                service.model,
                service_url(&service.base_url)
                    .map_or_else(|_| "地址无效".into(), |url| url.to_string())
            )
        } else {
            "未启用（建议开启）".into()
        };
        let mut controls = div().flex().gap_1();
        #[cfg(any(target_os = "macos", windows))]
        {
            controls = controls.child(
                button(
                    if hosted_on_autodl {
                        "修改 API Key"
                    } else {
                        "编辑模型"
                    },
                    false,
                )
                .id("organizer-edit")
                .when(!busy && !checking, |control| {
                    control.on_click(cx.listener(move |view, _, window, cx| {
                        if hosted_on_autodl {
                            view.edit_autodl_key(window, cx);
                        } else {
                            view.edit_service(window, true, cx);
                        }
                    }))
                }),
            );
        }
        if llm.enabled {
            controls = controls
                .child(
                    button(
                        if checking {
                            "测试中…"
                        } else {
                            "测试大模型"
                        },
                        false,
                    )
                    .id("organizer-benchmark")
                    .when(!checking, |control| {
                        control.on_click(cx.listener(|view, _, _, cx| view.benchmark(cx)))
                    }),
                )
                .child(button("停用", false).id("organizer-disable").when(
                    !busy && !checking,
                    |control| {
                        control.on_click(cx.listener(|view, _, _, cx| {
                            view.apply_preference(Preference::OrganizerEnabled(false), cx)
                        }))
                    },
                ));
        } else if !service.api_key.trim().is_empty() {
            controls = controls.child(button("启用", false).id("organizer-enable").when(
                !busy && !checking,
                |control| {
                    control.on_click(cx.listener(|view, _, _, cx| {
                        view.apply_preference(Preference::OrganizerEnabled(true), cx)
                    }))
                },
            ));
        }
        section(
            "语音整理",
            [
                setting_row("整理模型", Some(&summary), controls),
                div().when(llm.enabled, |row| {
                    let mut actions = div().flex().gap_1();
                    #[cfg(any(target_os = "macos", windows))]
                    {
                        actions = actions.child(
                            button(
                                if custom {
                                    "编辑提示词"
                                } else {
                                    "添加提示词"
                                },
                                false,
                            )
                            .id("organizer-edit-prompt")
                            .when(!busy, |control| {
                                control.on_click(cx.listener(|view, _, window, cx| {
                                    view.edit_organizer_prompt(window, cx)
                                }))
                            }),
                        );
                    }
                    if custom && !busy {
                        actions = actions.child(
                            button("恢复内置", false)
                                .id("organizer-reset-prompt")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.apply_preference(
                                        Preference::CustomPrompt(String::new()),
                                        cx,
                                    )
                                })),
                        );
                    }
                    row.border_t_1()
                        .border_color(rgb(BORDER()))
                        .child(setting_row(
                            "整理提示词",
                            Some(if custom { "自定义" } else { "内置" }),
                            actions,
                        ))
                }),
                div().when(llm.enabled, |content| {
                    content
                        .px_3()
                        .pb_1()
                        .child(detail("点击整理时，仅将原转写文字发送到此模型。"))
                }),
                self.benchmark_result(),
            ],
        )
    }

    fn shortcut_row(&self, shortcut: hotkey::Shortcut, busy: bool, cx: &mut Context<Self>) -> Div {
        if let Some(capture) = &self.settings.shortcut_capture {
            let prompt = capture.candidate.map_or_else(
                || {
                    if cfg!(windows) {
                        "按下任意单键或组合键，松开后保存。".to_owned()
                    } else {
                        "请按想用的快捷键，Esc 取消。".to_owned()
                    }
                },
                |shortcut| format!("松开 {} 后保存。", shortcut.label()),
            );
            return setting_row(
                "全局快捷键",
                Some(&prompt),
                div().child(
                    button("取消", false)
                        .id("cancel-shortcut")
                        .focusable()
                        .tab_stop(true)
                        .on_click(cx.listener(|view, _, _, cx| view.cancel_shortcut_capture(cx))),
                ),
            )
            .track_focus(&capture.focus)
            .when(!cfg!(windows), |row| {
                #[cfg(not(windows))]
                let row = row.capture_key_up(cx.listener(Self::capture_shortcut_up));
                row
            })
            .on_mouse_down_out(cx.listener(|view, _, _, cx| view.cancel_shortcut_capture(cx)));
        }
        let description = if self.global_hotkey.is_none() {
            self.hotkey_status.as_str()
        } else if cfg!(windows) {
            "先点聊天输入框，按一次开始听写，再按一次结束。"
        } else {
            "按一次开始录音，再按一次停止并保存。"
        };
        setting_row(
            "全局快捷键",
            Some(description),
            div().child(
                button(format!("{} · 更改", shortcut.label()), false)
                    .id("change-shortcut")
                    .when(!busy, |button| {
                        button
                            .focusable()
                            .tab_stop(true)
                            .focus(|style| style.border_1().border_color(rgb(crate::ACCENT())))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.begin_shortcut_capture(window, cx)
                            }))
                    }),
            ),
        )
    }

    fn setting_picker(
        &self,
        picker: Picker,
        label: &'static str,
        description: Option<&str>,
        current: &str,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let expanded = self.settings.picker == Some(picker);
        let control = div().flex_none().max_w(px(190.)).child(
            button(
                format!("{current}  {}", if expanded { "▴" } else { "▾" }),
                false,
            )
            .id(("settings-picker", picker as usize))
            .when(!busy, |button| {
                button.on_click(cx.listener(move |view, _, _, cx| {
                    if picker == Picker::Device && !expanded {
                        view.refresh_input_devices(cx);
                        view.tour.request_reveal();
                    }
                    view.settings.picker = (!expanded).then_some(picker);
                    cx.notify();
                }))
            }),
        );
        div().child(setting_row(label, description, control))
    }
}

pub fn listen_for_hotkey(mut presses: hotkey::HotkeyEvents, cx: &mut Context<Speaky>) {
    cx.spawn(async move |entity, cx| {
        while presses.recv().await.is_some() {
            if entity
                .update(cx, |view, cx| {
                    if !view.handle_tour_hotkey(cx) {
                        view.toggle_recording(super::RecordingStart::Desktop, cx);
                    }
                })
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
}

pub(crate) fn detail(text: impl Into<SharedString>) -> Div {
    div()
        .mt_0p5()
        .text_size(px(12.))
        .line_height(px(17.))
        .text_color(rgb(MUTED()))
        .child(text.into())
}

fn section(heading: &'static str, rows: impl IntoIterator<Item = Div>) -> Div {
    div()
        .child(
            div()
                .px_2()
                .mb_2()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(MUTED()))
                .child(heading),
        )
        .child(
            div()
                .bg(rgb(SURFACE()))
                .rounded(px(12.))
                .border_1()
                .border_color(rgb(BORDER()))
                .children(rows),
        )
}

fn setting_row(label: &'static str, description: Option<&str>, control: Div) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_2()
        .min_h(px(43.))
        .text_size(px(13.))
        .line_height(px(18.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .child(label)
                .when_some(description, |row, text| row.child(detail(text.to_owned()))),
        )
        .child(control)
}

fn options(elements: impl IntoIterator<Item = impl IntoElement>) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .px_3()
        .pb_2()
        .children(elements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_preference_preserves_other_settings_and_reload_choice() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.yaml");
        let mut original = serde_yaml::to_value(Config::default()).unwrap();
        original["appearance"]["future_setting"] = "keep".into();
        original["engine"]["openai"]["api_key"] = "test-key".into();
        original["core"]["asr"]["windows_toggle_hotkey"] = "key:4:f1".into();
        original["extension"]["keep"] = true.into();
        fs::write(&path, serde_yaml::to_string(&original).unwrap()).unwrap();

        for theme in [crate::theme::Theme::Dark, crate::theme::Theme::Light] {
            save(&path, &Preference::Theme(theme)).unwrap();
            let actual: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let mut expected = original.clone();
            expected["appearance"]["theme"] = theme.id().into();
            assert_eq!(actual, expected);
            let reloaded: Config = serde_yaml::from_value(actual).unwrap();
            assert_eq!(
                crate::theme::resolve(&reloaded.appearance.theme, gpui::WindowAppearance::Dark),
                theme
            );
        }

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn volc_editor_retains_keys_only_for_the_fixed_volc_endpoint() {
        let path = std::env::temp_dir().join(format!("speaky-volc-{}.yaml", uuid::Uuid::new_v4()));
        let set = |url: &str, key: Option<&str>| {
            save(
                &path,
                &Preference::Volc(service_ui::ServiceInput {
                    base_url: url.into(),
                    model: "volc.bigasr.sauc.duration".into(),
                    api_key: key.map(str::to_owned),
                }),
            )
        };
        set(service_ui::VOLC_URL, Some("test-volc-key")).unwrap();
        let saved = fs::read(&path).unwrap();
        assert!(set("wss://example.com", None).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
        set(service_ui::VOLC_URL, None).unwrap();
        let config: Config = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config.engine.current, "volc_bigmodel");
        assert_eq!(config.engine.volc_bigmodel.api_key, "test-volc-key");
        fs::remove_file(path).unwrap();
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn framework_settings_are_isolated_and_local_bridges_need_no_key() {
        let path = std::env::temp_dir()
            .join(uuid::Uuid::new_v4().to_string())
            .join("config.yaml");
        let funasr = crate::config::Framework::ALL[0];
        let kaldi = crate::config::Framework::ALL[7];
        let input = |url: &str, model: &str, key: Option<&str>| service_ui::ServiceInput {
            base_url: url.into(),
            model: model.into(),
            api_key: key.map(str::to_owned),
        };
        save(
            &path,
            &Preference::Service(
                input("https://example.com/v1", "previous", Some("original")),
                false,
            ),
        )
        .unwrap();
        save(
            &path,
            &Preference::Framework(
                input("https://remote.example/v1", "zh", Some("remote-key")),
                funasr,
            ),
        )
        .unwrap();
        save(
            &path,
            &Preference::Framework(input("http://127.0.0.1:18768/v1", "kaldi", None), kaldi),
        )
        .unwrap();
        let load = || serde_yaml::from_str::<Config>(&fs::read_to_string(&path).unwrap()).unwrap();
        let config = load();
        assert_eq!(config.engine.current, "kaldi");
        assert_eq!(config.engine.openai.api_key, "original");
        assert_eq!(funasr.service(&config).api_key, "remote-key");
        assert_eq!(kaldi.service(&config).api_key, "");
        assert_eq!(
            crate::config::Framework::ALL[1].service(&config).base_url,
            "http://127.0.0.1:18762/v1"
        );
        assert!(save(
            &path,
            &Preference::Framework(input("https://other.example/v1", "zh", None), funasr)
        )
        .is_err());
        assert_eq!(load().engine.current, "kaldi");
        save(
            &path,
            &Preference::Framework(input("http://localhost:18761/v1", "small", None), funasr),
        )
        .unwrap();
        assert_eq!(funasr.service(&load()).api_key, "");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn compares_sample_words_across_script_spacing_and_punctuation() {
        for transcript in [
            SAMPLE_REFERENCE,
            "「你好！」 語音識別測試。",
            "你 好 語音识别测试",
        ] {
            assert_eq!(sample_comparison(transcript), "样本与参考一致");
        }
        for transcript in ["语音识别测试", "你好，语音识别测试 2", "hello", ""] {
            assert_eq!(sample_comparison(transcript), "样本与参考有差异");
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn auto_detection_reuses_credentials_only_for_the_same_origin() {
        let mut input = service_ui::ServiceInput {
            base_url: "wss://asr.example/speech".into(),
            model: String::new(),
            api_key: None,
        };
        assert_eq!(
            submitted_key(&input, "wss://asr.example/old", "old-key"),
            "old-key"
        );
        for previous in [
            "wss://other.example",
            "wss://asr.example:444",
            "ws://asr.example",
        ] {
            assert!(submitted_key(&input, previous, "old-key").is_empty());
        }
        input.api_key = Some("new-key".into());
        assert_eq!(
            submitted_key(&input, "https://other.example", "old-key"),
            "new-key"
        );
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn verified_service_persists_route_and_rejects_stale_results() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = dir.join("config.yaml");
        let previous_engine = engine_state(&Config::default()).unwrap();
        let remote = crate::config::RemoteService {
            address: "https://asr.example/prefix".into(),
            endpoint: "https://asr.example/prefix/inference".into(),
            protocol: crate::engines::Protocol::WhisperCpp,
            model: String::new(),
            api_key: "token".into(),
        };
        let preference = Preference::Remote {
            service: remote.clone(),
            previous_engine,
        };
        save(&path, &Preference::Language("en")).unwrap();
        save(&path, &preference).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        let config: Config = serde_yaml::from_str(&saved).unwrap();
        assert_eq!(config.engine.current, "remote");
        assert_eq!(config.engine.remote, remote);
        assert_eq!(config.core.asr.language, "en");
        assert!(save(&path, &preference).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn selecting_autodl_reuses_the_rewrite_key_and_preserves_other_models() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        let original = "engine:\n  current: local\n  local_pack: local-test\n  remote:\n    address: https://asr.example/v1\n    endpoint: https://asr.example/v1/audio/transcriptions\n    protocol: openai\nllm:\n  enabled: false\n  openai:\n    base_url: https://www.autodl.art/api/v1\n    api_key: test-token\n";
        fs::write(&path, original).unwrap();
        let config: Config = serde_yaml::from_str(original).unwrap();
        let preference = Preference::Autodl {
            service: crate::engines::autodl_asr_service(&config).unwrap(),
            previous_engine: engine_state(&config).unwrap(),
        };
        save(&path, &preference).unwrap();
        let saved: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let mut expected: Value = serde_yaml::from_str(original).unwrap();
        expected["engine"]["current"] = "autodl".into();
        assert_eq!(saved, expected);

        fs::write(&path, original.replace("test-token", "different-token")).unwrap();
        assert!(save(&path, &preference).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn autodl_and_custom_rewrite_switch_without_changing_asr_or_sharing_custom_keys() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        fs::write(&path, "engine:\n  current: autodl\nllm:\n  enabled: false\n  openai:\n    base_url: https://www.autodl.art/api/v1\n    model: old-model\n    api_key: legacy-key\n").unwrap();
        let load = || serde_yaml::from_str::<Config>(&fs::read_to_string(&path).unwrap()).unwrap();

        save(
            &path,
            &Preference::Service(
                service_ui::ServiceInput {
                    base_url: "https://api.deepseek.com/v1".into(),
                    model: "deepseek-chat".into(),
                    api_key: Some("deepseek-key".into()),
                },
                true,
            ),
        )
        .unwrap();
        assert_eq!(load().llm.autodl_key(), Some("legacy-key"));
        assert_eq!(load().llm.organizer_service().api_key, "deepseek-key");

        let select = Preference::AutodlRewrite {
            model: "Qwen3.5-397B-A17B".into(),
            checked_key: "legacy-key".into(),
        };
        save(&path, &select).unwrap();
        let config = load();
        assert_eq!(config.llm.openai.api_key, "deepseek-key");
        assert_eq!(config.llm.organizer_service().model, "Qwen3.5-397B-A17B");
        assert_eq!(config.llm.organizer_service().api_key, "legacy-key");
        assert_eq!(config.engine.current, "autodl");
        assert_eq!(
            crate::engines::autodl_asr_service(&config).unwrap().api_key,
            "legacy-key"
        );

        save(&path, &Preference::AutodlKey("new-key".into())).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(save(&path, &select).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
        let config = load();
        assert_eq!(config.llm.organizer_service().api_key, "new-key");
        assert_eq!(
            crate::engines::autodl_asr_service(&config).unwrap().api_key,
            "new-key"
        );

        save(
            &path,
            &Preference::Service(
                service_ui::ServiceInput {
                    base_url: "https://api.deepseek.com/v1".into(),
                    model: "deepseek-chat".into(),
                    api_key: None,
                },
                true,
            ),
        )
        .unwrap();
        let config = load();
        assert!(config.llm.autodl_model.is_empty());
        assert_eq!(config.llm.organizer_service().api_key, "deepseek-key");
        assert_eq!(
            crate::engines::autodl_asr_service(&config).unwrap().api_key,
            "new-key"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn floating_button_defaults_on_and_persists_only_its_own_setting() {
        assert!(Config::default().desktop.windows_floating_button);
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.yaml");
        let original = "desktop:\n  auto_start: false\n  other_setting: keep\ncore:\n  asr:\n    windows_toggle_hotkey: control_shift_d\n";
        fs::write(&path, original).unwrap();
        let old: Config = serde_yaml::from_str(original).unwrap();
        assert!(old.desktop.windows_floating_button);
        let initial: Value = serde_yaml::from_str(original).unwrap();
        for enabled in [false, true] {
            save(&path, &Preference::FloatingButton(enabled)).unwrap();
            let yaml: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let config: Config = serde_yaml::from_value(yaml.clone()).unwrap();
            assert_eq!(config.desktop.windows_floating_button, enabled);
            let mut expected = initial.clone();
            expected["desktop"]["windows_floating_button"] = enabled.into();
            assert_eq!(yaml, expected);
        }
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn model_preloading_is_opt_in_and_persists_without_changing_model_selection() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.yaml");
        let original = "engine:\n  current: local\n  local:\n    model: Qwen3-ASR-1.7B\ndesktop:\n  auto_start: false\n";
        fs::write(&path, original).unwrap();
        assert!(!Config::default().desktop.preload_local_model);
        let old: Config = serde_yaml::from_str(original).unwrap();
        assert!(!old.desktop.preload_local_model);

        let mut expected: Value = serde_yaml::from_str(original).unwrap();
        for enabled in [true, false] {
            save(&path, &Preference::PreloadLocalModel(enabled)).unwrap();
            let yaml: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let updated: Config = serde_yaml::from_value(yaml.clone()).unwrap();
            assert_eq!(updated.desktop.preload_local_model, enabled);
            expected["desktop"]["preload_local_model"] = enabled.into();
            assert_eq!(yaml, expected);
        }
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn organizer_custom_prompt_can_be_changed_and_reset() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("config.yaml");
        save(&path, &Preference::OrganizerEnabled(true)).unwrap();
        let load = || {
            serde_yaml::from_str::<Config>(&fs::read_to_string(&path).unwrap())
                .unwrap()
                .llm
        };
        assert!(load().custom_prompt.is_empty());

        for prompt in ["保留数字。\n按主题整理。", "保留英文，用短句。"] {
            save(&path, &Preference::CustomPrompt(prompt.into())).unwrap();
            assert_eq!(load().custom_prompt, prompt);
        }

        save(&path, &Preference::CustomPrompt(" \n".into())).unwrap();
        assert!(load().custom_prompt.is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn updates_preferences_without_losing_existing_fields() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("config.yaml");
        save(
            &path,
            &Preference::Shortcut(hotkey::Shortcut::ControlShiftD),
        )
        .unwrap();
        let mut before: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let other_platform = if cfg!(windows) {
            "macos_toggle_hotkey"
        } else {
            "windows_toggle_hotkey"
        };
        before["core"]["asr"][other_platform] = hotkey::Shortcut::OptionShiftSpace.id().into();
        before["core"]["asr"]["audio_device"] = 2.into();
        before["engine"]["openai"]["api_key"] = "test-key".into();
        before["extension"]["keep"] = true.into();
        fs::write(&path, serde_yaml::to_string(&before).unwrap()).unwrap();

        save(&path, &Preference::Language("yue")).unwrap();
        save(&path, &Preference::Device(Some("Test microphone".into()))).unwrap();
        let updated: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let config: Config = serde_yaml::from_value(updated.clone()).unwrap();
        assert_eq!(config.core.asr.language, "yue");
        assert_eq!(
            config.core.asr.audio_device_name.as_deref(),
            Some("Test microphone")
        );
        assert_eq!(config.core.asr.audio_device, None);
        assert_eq!(selected_shortcut(&config), hotkey::Shortcut::ControlShiftD);
        assert_eq!(
            updated["core"]["asr"][other_platform],
            before["core"]["asr"][other_platform]
        );
        assert_eq!(updated["engine"], before["engine"]);
        assert_eq!(updated["extension"], before["extension"]);
        let custom = shortcut_from_keystroke(&gpui::Keystroke::parse("f8").unwrap()).unwrap();
        save(&path, &Preference::Shortcut(custom)).unwrap();
        let reloaded: Config = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(selected_shortcut(&reloaded), custom);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn changing_service_origin_requires_its_own_key() {
        use service_ui::ServiceInput;

        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("config.yaml");
        let service = |url: &str, key: Option<&str>| {
            Preference::Service(
                ServiceInput {
                    base_url: url.into(),
                    model: "test-model".into(),
                    api_key: key.map(str::to_owned),
                },
                false,
            )
        };
        save(
            &path,
            &service("https://first.example/v1", Some("first-key")),
        )
        .unwrap();
        save(&path, &service("https://first.example/api/", None)).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        assert!(save(&path, &service("http://second.example/v1", None)).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        save(&path, &service("http://second.example/v1/", Some("EMPTY"))).unwrap();
        let config: Config = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.engine.current, "openai");
        assert_eq!(config.engine.openai.base_url, "http://second.example/v1");
        assert_eq!(config.engine.openai.api_key, "EMPTY");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn organizer_configuration_is_independent_and_opt_in() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("config.yaml");
        assert!(!Config::default().llm.enabled);
        let service = |base_url: &str, api_key: Option<&str>| service_ui::ServiceInput {
            base_url: base_url.into(),
            model: "test-model".into(),
            api_key: api_key.map(str::to_owned),
        };
        save(
            &path,
            &Preference::Service(service("https://asr.example/v1", Some("asr-key")), false),
        )
        .unwrap();
        assert!(save(
            &path,
            &Preference::Service(service("https://llm.example/v1", None), true),
        )
        .is_err());
        save(
            &path,
            &Preference::Service(service("https://llm.example/v1", Some("llm-key")), true),
        )
        .unwrap();
        let config: Config = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(config.llm.enabled);
        assert_eq!(config.llm.openai.api_key, "llm-key");
        assert_eq!(config.engine.openai.base_url, "https://asr.example/v1");
        assert_eq!(config.engine.openai.api_key, "asr-key");
        save(&path, &Preference::OrganizerEnabled(false)).unwrap();
        let config: Config = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!config.llm.enabled);
        assert_eq!(config.llm.openai.api_key, "llm-key");
        fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(any(target_os = "macos", windows))]
use super::service_ui;
use super::{
    audio::AudioRecorder, button, config::Config, drafts, hotkey, onboarding::Anchor, page_heading,
    Page, Speaky, BORDER, MUTED, SURFACE,
};
use gpui::{
    div, prelude::*, px, rgb, Context, Div, FocusHandle, SharedString, Subscription, Window,
};
#[cfg(not(windows))]
use gpui::{KeyUpEvent, KeystrokeEvent};
use serde_yaml::Value;
use std::{fs, io::ErrorKind, path::Path};

use speaky_core::session::{sample_comparison, LANGUAGES, SAMPLE_AUDIO, SAMPLE_REFERENCE};

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

#[derive(Default)]
pub struct SettingsPage {
    config: Option<Config>,
    devices: Vec<(u32, String)>,
    picker: Option<Picker>,
    shortcut_capture: Option<ShortcutCapture>,
    notice: Option<Result<String, String>>,
    checking: Option<ServiceCheck>,
    #[cfg(any(target_os = "macos", windows))]
    pending_service: Option<(Value, service_ui::ServiceInput)>,
    pub(crate) polish_report: Option<Result<crate::polish_ui::BenchmarkResult, String>>,
    pub(crate) polish_checking: bool,
}

impl SettingsPage {
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn recognition_checking(&self) -> bool {
        self.checking.is_some()
    }

    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn guide_can_configure_service(&self) -> bool {
        self.config.is_some() && !self.recognition_checking()
    }
}

enum Preference {
    Shortcut(hotkey::Shortcut),
    #[cfg(windows)]
    FloatingButton(bool),
    PreloadLocalModel(bool),
    LocalPack(String),
    Device(Option<String>),
    Language(&'static str),
    #[cfg(any(target_os = "macos", windows))]
    Service(service_ui::ServiceInput, bool),
    #[cfg(any(target_os = "macos", windows))]
    Remote {
        service: crate::config::RemoteService,
        previous_engine: Value,
    },
    OrganizerEnabled(bool),
    LearnPreferences(bool),
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
        }
        Preference::PreloadLocalModel(enabled) => {
            yaml["desktop"]["preload_local_model"] = (*enabled).into();
        }
        Preference::Language(language) => {
            yaml["core"]["asr"]["language"] = (*language).into();
        }
        Preference::OrganizerEnabled(enabled) => yaml["llm"]["enabled"] = (*enabled).into(),
        Preference::LearnPreferences(enabled) => {
            yaml["llm"]["learn_preferences"] = (*enabled).into();
        }
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
        Preference::Service(input, _) => {
            let organizer = matches!(preference, Preference::Service(_, true));
            let (old_url, old_key) = if organizer {
                (&existing.llm.openai.base_url, &existing.llm.openai.api_key)
            } else {
                (
                    &existing.engine.openai.base_url,
                    &existing.engine.openai.api_key,
                )
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
            let same_origin = service_url(old_url).is_ok_and(|old| old.origin() == url.origin());
            if key.is_none() && (old_key.trim().is_empty() || !same_origin) {
                return Err("请填写此服务的 API 密钥。无鉴权的内网服务可填 EMPTY。".into());
            }
            let target = if organizer {
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

    let serialized = serde_yaml::to_string(&yaml).map_err(|error| error.to_string())?;
    drafts::write_atomically(path, serialized.as_bytes()).map_err(|error| error.to_string())
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

#[cfg(any(target_os = "macos", windows))]
fn engine_state(config: &Config) -> Result<Value, String> {
    serde_yaml::to_value(&config.engine).map_err(|e| e.to_string())
}

#[cfg(any(target_os = "macos", windows))]
fn remote_editor_values(config: &Config) -> (String, String, String) {
    let remote = &config.engine.remote;
    if config.engine.current == "remote"
        || (!remote.address.is_empty() && crate::engines::uses_local_asr(config))
    {
        return (
            remote.address.clone(),
            remote.model.clone(),
            remote.api_key.clone(),
        );
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
    pub(super) fn open_service_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_picker.close();
        self.open_page(Page::Settings, cx);
        self.edit_service(window, false, cx);
    }

    pub(crate) fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.cancel_shortcut_capture(cx);
        if page != self.page {
            self.tour.request_reveal();
        }
        self.refresh_model_selection();
        self.page = page;
        if page == Page::Settings {
            self.settings.picker = None;
            self.settings.devices = AudioRecorder::get_devices();
            self.reload_settings();
        }
        cx.notify();
    }

    pub(super) fn open_shortcut_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(Page::Settings, cx);
        if self.recording.is_none() && !self.transcribing && !self.recognition_model_busy() {
            self.begin_shortcut_capture(window, cx);
        }
    }

    fn begin_shortcut_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.shortcut_capture.is_some() {
            return;
        }
        let Some(config) = &self.settings.config else {
            return;
        };
        #[cfg(windows)]
        let (native, mut events) = match crate::hotkey_capture::WindowCapture::start(window) {
            Ok(capture) => capture,
            Err(error) => {
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
                self.settings.notice = None;
            }
            Err(error) => self.settings.notice = Some(Err(error)),
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
                self.settings.notice = Some(Err(
                    "没有收到完整的按键，快捷键可能已被占用。请换一个试试。".into(),
                ));
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
                self.organizer_enabled = config.llm.enabled;
                self.organizer_learning = config.llm.learns_preferences();
                self.settings.config = Some(config);
            }
            Err(_) => {
                self.organizer_enabled = false;
                self.organizer_learning = false;
                self.settings.config = None;
                self.settings.notice =
                    Some(Err("无法读取配置，请检查 config.yaml 后重试。".into()));
            }
        }
    }

    pub(crate) fn settings_notice(&self) -> Option<&Result<String, String>> {
        self.settings.notice.as_ref()
    }

    pub(crate) fn release_local_model(&mut self, cx: &mut Context<Self>) {
        self.model_picker.switching = Some("正在释放本地模型包…".into());
        let task = cx.background_spawn(async { crate::model_packs::release() });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.model_picker.switching = None;
                view.refresh_model_selection();
                if let Err(error) = result {
                    view.settings.notice = Some(Err(format!("卸载本地模型失败：{error}")));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_preference(&mut self, preference: Preference, cx: &mut Context<Self>) {
        let changing_service = matches!(
            &preference,
            Preference::Service(_, false) | Preference::Remote { .. }
        );
        if let Some(reason) = changing_service
            .then(|| self.model_switch_blocker())
            .flatten()
        {
            self.settings.notice = Some(Err(format!("{reason}。")));
            cx.notify();
            return;
        }
        let persist = || save(&Config::config_path(), &preference);
        let result =
            if self.recording.is_some() || self.transcribing || self.recognition_model_busy() {
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

        let restore_shortcut = matches!(preference, Preference::Shortcut(_)) && result.is_err();
        if result.is_ok() {
            if matches!(preference, Preference::Shortcut(_)) {
                self.settings.shortcut_capture = None;
            }
            let organizer_changed = match preference {
                Preference::OrganizerEnabled(_) => true,
                #[cfg(any(target_os = "macos", windows))]
                Preference::Service(_, organizer) => organizer,
                _ => false,
            };
            if organizer_changed {
                self.settings.polish_report = None;
            }
            self.settings.picker = None;
            self.reload_settings();
            if matches!(preference, Preference::PreloadLocalModel(true)) {
                if let Some(config) = self.settings.config.clone() {
                    self.preload_selected_model(config, cx);
                }
            }
            if changing_service {
                self.release_local_model(cx);
            }
        }
        #[cfg(windows)]
        let result = result.and_then(|()| match preference {
            Preference::FloatingButton(enabled) => self
                .set_floating_button_enabled(enabled, cx)
                .map_err(|error| format!("已保存设置，桌面悬浮按钮更新失败：{error}")),
            _ => Ok(()),
        });
        self.settings.notice = Some(result.map(|()| "已保存设置。".into()));
        if restore_shortcut {
            self.cancel_shortcut_capture(cx);
        }
        cx.notify();
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
        let (base_url, model, saved_key) = if organizer {
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
        self.settings.checking = Some(ServiceCheck::Detect);
        self.settings.notice = Some(Ok("正在识别接口并转写内置样本…".into()));
        let task = cx.background_spawn(async move {
            let timer = std::time::Instant::now();
            crate::engines::detect_service(input.base_url.trim(), input.model.trim(), &key)
                .map(|(service, text)| (service, text, timer.elapsed().as_secs_f64()))
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.settings.checking = None;
                match result {
                    Ok((service, text, elapsed)) => {
                        let label = service.protocol.label();
                        view.apply_preference(
                            Preference::Remote {
                                service,
                                previous_engine,
                            },
                            cx,
                        );
                        if matches!(view.settings.notice, Some(Ok(_))) {
                            view.settings.pending_service = None;
                            view.refresh_model_selection();
                            view.settings.notice = Some(Ok(format!(
                                "已启用 {label}（检测用时 {elapsed:.2} 秒）。{}。样本转写：{text}",
                                sample_comparison(&text)
                            )));
                        } else {
                            view.reload_settings();
                        }
                    }
                    Err(error) => view.settings.notice = Some(Err(error)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[cfg(any(target_os = "macos", windows))]
    fn check_recognition_service(&mut self, cx: &mut Context<Self>) {
        if self.settings.recognition_checking() || self.recognition_model_busy() {
            return;
        }
        let Some(config) = self.settings.config.clone() else {
            return;
        };
        let previous_engine = engine_state(&config);
        self.settings.checking = Some(ServiceCheck::Sample);
        self.settings.notice = Some(Ok("正在转写内置中文样本…".into()));
        let task = cx.background_spawn(async move {
            let timer = std::time::Instant::now();
            let (label, text) = if config.engine.current == "remote" {
                let remote = &config.engine.remote;
                (
                    remote.protocol.label().to_string(),
                    crate::engines::check_service(remote)?,
                )
            } else {
                let engine = crate::engines::recognition_engine(&config)?;
                let text = engine
                    .transcribe(SAMPLE_AUDIO, "zh")
                    .map_err(|error| error.to_string())?;
                (engine.name().to_string(), text)
            };
            if text.trim().is_empty() {
                return Err("服务返回空白转写，请检查模型后重试。".into());
            }
            Ok(format!(
                "{label} 可用（{:.2} 秒）。{}。样本转写：{text}",
                timer.elapsed().as_secs_f64(),
                sample_comparison(&text)
            ))
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.settings.checking = None;
                let current = Config::load()
                    .map_err(|e| e.to_string())
                    .and_then(|c| engine_state(&c));
                view.settings.notice = Some(if current.is_ok() && current == previous_engine {
                    result
                } else {
                    view.reload_settings();
                    Err("识别配置已变更，请重新检测。".into())
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn settings_page(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.recording.is_some() || self.transcribing || self.recognition_model_busy();
        let head = page_heading("设置", "调整麦克风、快捷键、识别服务和语音整理。")
            .when(busy, |head| head.child(detail("录音和转写结束后可修改。")));
        let body = div().flex().flex_col().gap_4().child(head);
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
        let device = asr.audio_device_name.clone().or_else(|| {
            asr.audio_device.and_then(|selected| {
                self.settings
                    .devices
                    .iter()
                    .find(|(index, _)| *index == selected)
                    .map(|(_, name)| name.clone())
                    .or_else(|| Some(format!("输入设备 {selected}")))
            })
        });
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
        let device_options = std::iter::once((None, "系统默认".to_owned())).chain(
            self.settings
                .devices
                .iter()
                .map(|(_, name)| (Some(name.clone()), name.clone())),
        );

        let capture = section(
            "录音",
            [
                self.shortcut_row(shortcut, busy, cx),
                self.setting_picker(
                    Picker::Device,
                    "输入设备",
                    None,
                    device.as_deref().unwrap_or("系统默认"),
                    busy,
                    cx,
                )
                .border_t_1()
                .border_color(rgb(BORDER))
                .when(
                    !busy && self.settings.picker == Some(Picker::Device),
                    |row| {
                        row.child(options(device_options.enumerate().map(
                            |(index, (id, label))| {
                                button(label, device == id).id(("device", index)).on_click(
                                    cx.listener(move |view, _, _, cx| {
                                        view.apply_preference(Preference::Device(id.clone()), cx)
                                    }),
                                )
                            },
                        )))
                    },
                )
                .relative()
                .children(self.tour.mark(Anchor::Input)),
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
                    .border_t_1()
                    .border_color(rgb(BORDER))
                },
            ],
        );
        let local = crate::engines::uses_local_asr(config);
        let remote = &config.engine.remote;
        let volc = &config.engine.volc_bigmodel;
        let volc_configured = config.engine.current == "volc_bigmodel"
            && (!volc.api_key.is_empty()
                || (!volc.app_key.is_empty() && !volc.access_key.is_empty())
                || std::env::var_os("SPEAKY_VOLC_API_KEY").is_some_and(|key| !key.is_empty()));
        let configured = local
            || volc_configured
            || matches!(config.engine.current.as_str(), "remote" | "openai");
        let service_summary = if config.engine.current == "remote" {
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
        } else if local {
            self.model_picker.label().to_owned()
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
                setting_row(
                    "接入方式",
                    Some(&service_summary),
                    actions.child(self.model_picker_button(cx)),
                )
                .relative()
                .children(self.tour.mark(Anchor::Service)),
                div().children(self.model_picker_panel(cx)),
                div().when(local, |row| {
                    row.child(setting_row(
                        "本地模型包",
                        None,
                        div().child(
                            button("释放本地模型", false)
                                .id("settings-release-local-model")
                                .when(busy || self.model_switch_blocker().is_some(), |button| {
                                    button.opacity(0.5)
                                })
                                .when(!busy && self.model_switch_blocker().is_none(), |button| {
                                    button.on_click(cx.listener(|view, _, _, cx| {
                                        view.release_local_model(cx);
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
                        .filter(|_| enabled && local)
                        .unwrap_or("在 Speaky 启动时提前加载选中的本地模型包。");
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
                    .border_color(rgb(BORDER))
                },
                self.setting_picker(Picker::Language, "识别语言", None, language_label, busy, cx)
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
                    ),
                div().when(configured, |row| {
                    row.border_t_1()
                        .border_color(rgb(BORDER))
                        .child(setting_row(
                            "内置中文样本",
                            Some(&format!("参考文字：{SAMPLE_REFERENCE}")),
                            sample_action,
                        ))
                }),
            ],
        );
        body.child(capture)
            .child(recognition)
            .child(self.organizer_section(config, busy, cx))
    }

    fn organizer_section(&self, config: &Config, busy: bool, cx: &mut Context<Self>) -> Div {
        let llm = &config.llm;
        let service = &llm.openai;
        let checking = self.settings.polish_checking;
        let custom = !llm.custom_prompt.trim().is_empty();
        let preferences = llm
            .learns_preferences()
            .then(|| crate::polish_preferences::load(service));
        let summary = if llm.enabled {
            format!(
                "{} · {}",
                service.model,
                service_url(&service.base_url)
                    .map_or_else(|_| "地址无效".into(), |url| url.to_string())
            )
        } else {
            "未启用".into()
        };
        let mut controls = div().flex().gap_1();
        #[cfg(any(target_os = "macos", windows))]
        {
            controls = controls.child(button("编辑模型", false).id("organizer-edit").when(
                !busy && !checking,
                |control| {
                    control.on_click(
                        cx.listener(|view, _, window, cx| view.edit_service(window, true, cx)),
                    )
                },
            ));
        }
        if llm.enabled {
            controls = controls
                .child(
                    button(
                        if checking {
                            "测试中…"
                        } else {
                            "运行固定基准"
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
                        .border_color(rgb(BORDER))
                        .child(setting_row(
                            "整理提示词",
                            Some(if custom { "自定义" } else { "内置" }),
                            actions,
                        ))
                }),
                div().when(llm.enabled, |row| {
                    let learning = llm.learn_preferences;
                    let summary = if custom && learning {
                        "使用自定义提示词时暂停"
                    } else if learning {
                        "已开启"
                    } else {
                        "已跳过"
                    };
                    let action = div()
                        .flex_none()
                        .when(!busy && (!custom || learning), |action| {
                            action.child(
                                button(
                                    if learning {
                                        "跳过评分"
                                    } else {
                                        "开启评分"
                                    },
                                    false,
                                )
                                .id("organizer-toggle-learning")
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.apply_preference(
                                            Preference::LearnPreferences(!learning),
                                            cx,
                                        )
                                    },
                                )),
                            )
                        });
                    row.border_t_1()
                        .border_color(rgb(BORDER))
                        .child(setting_row("偏好评分", Some(summary), action))
                }),
                div().when_some(preferences, |content, preferences| {
                    let mut action = div().flex_none();
                    let summary = match preferences {
                        Ok(preferences) => {
                            let status = if preferences.settled() {
                                let selected = preferences.selected();
                                action = action.child(
                                    button("复制提示词", false)
                                        .id("organizer-copy-prompt")
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                selected.prompt(),
                                            ));
                                        })),
                                );
                                format!("已采用 {}", selected.label())
                            } else {
                                format!("下一次试用 {}", preferences.next_candidate().label())
                            };
                            let styles = crate::polish::Profile::ALL.map(|profile| {
                                let (good, total) = preferences.progress(profile);
                                format!("{} {good}/{total}", profile.label())
                            });
                            format!(
                                "{status} · {}\n同一风格至少评 5 条，好评率达到 80% 后采用。",
                                styles.join(" · ")
                            )
                        }
                        Err(error) => error,
                    };
                    content
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .child(setting_row("个人偏好", Some(&summary), action))
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
                        button.on_click(cx.listener(|view, _, window, cx| {
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
                        view.settings.devices = AudioRecorder::get_devices();
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
                    view.toggle_recording(super::RecordingStart::Desktop, cx)
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
        .text_color(rgb(MUTED))
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
                .text_color(rgb(MUTED))
                .child(heading),
        )
        .child(
            div()
                .bg(rgb(SURFACE))
                .rounded(px(12.))
                .border_1()
                .border_color(rgb(BORDER))
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
        let original =
            "engine:\n  current: local\n  local_pack: sample-pack\ndesktop:\n  auto_start: false\n";
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
    fn organizer_scoring_is_optional_and_pauses_for_custom_prompts() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("config.yaml");
        save(&path, &Preference::OrganizerEnabled(true)).unwrap();
        let load = || {
            serde_yaml::from_str::<Config>(&fs::read_to_string(&path).unwrap())
                .unwrap()
                .llm
        };
        assert!(!load().learns_preferences());
        assert!(load().custom_prompt.is_empty());
        save(&path, &Preference::LearnPreferences(true)).unwrap();
        assert!(load().learns_preferences());

        for prompt in ["保留数字。\n按主题整理。", "保留英文，用短句。"] {
            save(&path, &Preference::CustomPrompt(prompt.into())).unwrap();
            let llm = load();
            assert_eq!(llm.custom_prompt, prompt);
            assert!(llm.learn_preferences);
            assert!(!llm.learns_preferences());
        }

        save(&path, &Preference::CustomPrompt(" \n".into())).unwrap();
        assert!(load().learns_preferences());
        save(&path, &Preference::LearnPreferences(false)).unwrap();
        assert!(!load().learns_preferences());
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

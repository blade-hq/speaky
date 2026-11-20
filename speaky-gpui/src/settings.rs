use super::{
    audio::AudioRecorder, button, config::Config, drafts, hotkey, Page, Speaky, BORDER, MUTED,
};
#[cfg(target_os = "macos")]
use super::{
    engines::{Engine, EngineError, OpenAIEngine},
    service_ui,
};
use gpui::{
    div, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba, BoxShadow, Context,
    Div, SharedString,
};
use serde_yaml::Value;
use std::{fs, io::ErrorKind, path::Path};

const LANGUAGES: [(&str, &str); 6] = [
    ("auto", "自动检测"),
    ("zh", "中文（普通话）"),
    ("yue", "粤语"),
    ("en", "英语"),
    ("ja", "日语"),
    ("ko", "韩语"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Shortcut,
    Device,
    Language,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServiceCheck {
    Connection,
    Sample,
}

#[derive(Default)]
pub struct SettingsPage {
    config: Option<Config>,
    devices: Vec<(u32, String)>,
    picker: Option<Picker>,
    notice: Option<Result<String, String>>,
    checking: Option<ServiceCheck>,
}

enum Preference {
    Shortcut(hotkey::Shortcut),
    Device(Option<String>),
    Language(&'static str),
    #[cfg(target_os = "macos")]
    Service(service_ui::ServiceInput),
}

pub fn selected_shortcut(config: &Config) -> hotkey::Shortcut {
    hotkey::Shortcut::ALL
        .into_iter()
        .find(|preset| Some(preset.id()) == config.core.asr.macos_toggle_hotkey.as_deref())
        .unwrap_or_default()
}

fn save(path: &Path, preference: &Preference) -> Result<(), String> {
    let mut yaml: Value = match fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == ErrorKind::NotFound => Value::Null,
        Err(error) => return Err(error.to_string()),
    };
    let existing = if yaml.is_null() {
        Config::default()
    } else {
        serde_yaml::from_value::<Config>(yaml.clone()).map_err(|error| error.to_string())?
    };
    match preference {
        Preference::Shortcut(preset) => {
            yaml["core"]["asr"]["macos_toggle_hotkey"] = preset.id().into();
        }
        Preference::Device(name) => {
            let asr = &mut yaml["core"]["asr"];
            asr["audio_device"] = Value::Null;
            asr["audio_device_name"] = name.clone().map_or(Value::Null, Value::String);
        }
        Preference::Language(language) => {
            yaml["core"]["asr"]["language"] = (*language).into();
        }
        #[cfg(target_os = "macos")]
        Preference::Service(input) => {
            let old = existing.engine.openai;
            let url = service_url(&input.base_url)?;
            let model = input.model.trim();
            if model.is_empty() {
                return Err("请填写模型 ID。".into());
            }
            let key = input
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty());
            if key.is_none()
                && (old.api_key.trim().is_empty()
                    || service_url(&old.base_url).map_or(true, |old| old.origin() != url.origin()))
            {
                return Err("请填写此服务的 API 密钥。无鉴权的内网服务可填 EMPTY。".into());
            }
            if let Some(key) = key {
                reqwest::header::HeaderValue::from_str(key)
                    .map_err(|_| "API 密钥含不支持的字符。")?;
                yaml["engine"]["openai"]["api_key"] = key.into();
            }
            yaml["engine"]["current"] = "openai".into();
            yaml["engine"]["openai"]["base_url"] = url.as_str().trim_end_matches('/').into();
            yaml["engine"]["openai"]["model"] = model.into();
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

impl Speaky {
    pub(crate) fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        if page == Page::Settings {
            self.settings.picker = None;
            self.settings.devices = AudioRecorder::get_devices();
            self.reload_settings();
        }
        cx.notify();
    }

    fn reload_settings(&mut self) {
        match Config::load() {
            Ok(config) => self.settings.config = Some(config),
            Err(_) => {
                self.settings.config = None;
                self.settings.notice =
                    Some(Err("无法读取配置，请检查 config.yaml 后重试。".into()));
            }
        }
    }

    pub(crate) fn settings_notice(&self) -> Option<&Result<String, String>> {
        self.settings.notice.as_ref()
    }

    fn apply_preference(&mut self, preference: Preference, cx: &mut Context<Self>) {
        let persist = || save(&Config::config_path(), &preference);
        let result = if self.recording.is_some() || self.transcribing {
            Err("请在录音和转写结束后修改设置。".into())
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

        if result.is_ok() {
            self.settings.picker = None;
            self.reload_settings();
        }
        self.settings.notice = Some(result.map(|()| "已保存设置。".into()));
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    fn edit_recognition_service(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(config) = &self.settings.config else {
            return;
        };
        let service = &config.engine.openai;
        match service_ui::edit(window, &service.base_url, &service.model) {
            Ok(receiver) => cx
                .spawn(async move |entity, cx| {
                    let Ok(result) = receiver.await else { return };
                    let _ = entity.update(cx, |view, cx| match result {
                        Ok(Some(input)) => view.apply_preference(Preference::Service(input), cx),
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

    #[cfg(target_os = "macos")]
    fn check_recognition_service(&mut self, check: ServiceCheck, cx: &mut Context<Self>) {
        if self.settings.checking.is_some() {
            return;
        }
        let Some(config) = &self.settings.config else {
            return;
        };
        let service = config.engine.openai.clone();
        let Ok(url) = service_url(&service.base_url) else {
            self.settings.notice = Some(Err("请先设置有效的服务地址。".into()));
            cx.notify();
            return;
        };
        if service.api_key.trim().is_empty() {
            self.settings.notice = Some(Err("请先设置 API 密钥。".into()));
            cx.notify();
            return;
        }
        self.settings.checking = Some(check);
        self.settings.notice = None;
        let task = cx.background_spawn(async move {
            let engine = OpenAIEngine::new(&service.api_key, &service.model, url.as_str());
            match check {
                ServiceCheck::Connection => engine
                    .check_model()
                    .map(|()| "连接成功，服务已列出所选模型。".into()),
                ServiceCheck::Sample => {
                    let audio = include_bytes!("../assets/settings-sample.wav");
                    let timer = std::time::Instant::now();
                    match engine.transcribe(audio, "zh") {
                        Ok(text) if text.trim().is_empty() => Err("服务未返回文字。".into()),
                        Ok(text) => Ok(format!(
                            "返回（{:.2} 秒）  {text}",
                            timer.elapsed().as_secs_f64(),
                        )),
                        Err(EngineError::RateLimited(_)) => Err("服务限流，请稍后再试。".into()),
                        Err(EngineError::InvalidResponse(_)) => {
                            Err("转写接口返回的格式不兼容。".into())
                        }
                        Err(_) => Err("样本转写失败，请检查服务地址、模型和 API 密钥。".into()),
                    }
                }
            }
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.settings.checking = None;
                view.settings.notice = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn settings_page(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.recording.is_some() || self.transcribing;
        let head = div()
            .child(
                div()
                    .text_size(px(19.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("设置"),
            )
            .when(busy, |head| head.child(detail("录音和转写结束后可修改。")));
        let body = div().flex().flex_col().gap_3().child(head);
        let Some(config) = &self.settings.config else {
            return body
                .child(detail("无法读取配置，请检查 config.yaml。"))
                .child(
                    button("重新读取", false)
                        .id("settings-reload")
                        .on_click(cx.listener(|view, _, _, cx| view.open_page(Page::Settings, cx))),
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
                self.setting_picker(
                    Picker::Shortcut,
                    "全局快捷键",
                    self.global_hotkey
                        .is_none()
                        .then_some(self.hotkey_status.as_str()),
                    shortcut.label(),
                    busy,
                    cx,
                )
                .when(
                    !busy && self.settings.picker == Some(Picker::Shortcut),
                    |row| {
                        row.child(options(hotkey::Shortcut::ALL.map(|preset| {
                            button(preset.label(), preset == shortcut)
                                .id(preset.id())
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.apply_preference(Preference::Shortcut(preset), cx)
                                }))
                        })))
                    },
                ),
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
                ),
            ],
        );
        let service = &config.engine.openai;
        let configured = config.engine.current == "openai";
        let service_summary = if configured {
            format!(
                "{} · {}",
                service.model,
                service_url(&service.base_url)
                    .map_or_else(|_| "地址无效".into(), |url| url.to_string()),
            )
        } else {
            format!("当前服务：{}", config.engine.current)
        };
        let mut actions = div().flex().flex_wrap().justify_end().gap_1().flex_none();
        let mut sample_action = div().flex_none();
        #[cfg(target_os = "macos")]
        {
            actions = actions
                .child(button("编辑", false).id("settings-edit-service").when(
                    !busy && self.settings.checking.is_none(),
                    |button| {
                        button.on_click(cx.listener(|view, _, window, cx| {
                            view.edit_recognition_service(window, cx)
                        }))
                    },
                ))
                .when(configured, |actions| {
                    actions.child(
                        button(
                            if self.settings.checking == Some(ServiceCheck::Connection) {
                                "检查中…"
                            } else {
                                "检查连接"
                            },
                            false,
                        )
                        .id("settings-check-service")
                        .when(self.settings.checking.is_none(), |button| {
                            button.on_click(cx.listener(|view, _, _, cx| {
                                view.check_recognition_service(ServiceCheck::Connection, cx)
                            }))
                        }),
                    )
                });
            sample_action = sample_action.child(
                button(
                    if self.settings.checking == Some(ServiceCheck::Sample) {
                        "转写中…"
                    } else {
                        "试转写"
                    },
                    false,
                )
                .id("settings-transcribe-sample")
                .when(self.settings.checking.is_none(), |button| {
                    button.on_click(cx.listener(|view, _, _, cx| {
                        view.check_recognition_service(ServiceCheck::Sample, cx)
                    }))
                }),
            );
        }
        let recognition = section(
            "识别",
            [
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
                div()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(setting_row("识别服务", Some(&service_summary), actions)),
                div().when(configured, |row| {
                    row.border_t_1()
                        .border_color(rgb(BORDER))
                        .child(setting_row(
                            "内置中文样本",
                            Some("无需麦克风"),
                            sample_action,
                        ))
                }),
            ],
        );
        body.child(capture).child(recognition)
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
            if entity.update(cx, Speaky::toggle_recording).is_err() {
                break;
            }
        }
    })
    .detach();
}

fn detail(text: impl Into<SharedString>) -> Div {
    div()
        .mt_0p5()
        .text_size(px(11.))
        .line_height(px(14.))
        .text_color(rgb(MUTED))
        .child(text.into())
}

fn section(heading: &'static str, rows: impl IntoIterator<Item = Div>) -> Div {
    div()
        .child(
            div()
                .px_2()
                .mb_1()
                .text_size(px(12.))
                .text_color(rgb(MUTED))
                .child(heading),
        )
        .child(
            div()
                .bg(linear_gradient(
                    145.,
                    linear_color_stop(rgba(0x343d577d), 0.),
                    linear_color_stop(rgba(0x202639b0), 1.),
                ))
                .rounded(px(12.))
                .border_1()
                .border_color(rgb(BORDER))
                .shadow(vec![BoxShadow {
                    color: rgba(0x080a123d).into(),
                    offset: point(px(0.), px(5.)),
                    blur_radius: px(14.),
                    spread_radius: px(0.),
                }])
                .children(rows),
        )
}

fn setting_row(label: &'static str, description: Option<&str>, control: Div) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .min_h(px(36.))
        .text_size(px(12.))
        .line_height(px(16.))
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
    fn updates_preferences_without_losing_existing_fields() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("config.yaml");
        save(
            &path,
            &Preference::Shortcut(hotkey::Shortcut::ControlShiftD),
        )
        .unwrap();
        let mut before: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
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
        assert_eq!(updated["engine"], before["engine"]);
        assert_eq!(updated["extension"], before["extension"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn changing_service_origin_requires_its_own_key() {
        use service_ui::ServiceInput;

        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("config.yaml");
        let service = |url: &str, key: Option<&str>| {
            Preference::Service(ServiceInput {
                base_url: url.into(),
                model: "test-model".into(),
                api_key: key.map(str::to_owned),
            })
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
}

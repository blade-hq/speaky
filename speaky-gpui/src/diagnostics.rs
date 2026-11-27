use crate::{
    audio::{AudioRecorder, InputDevices},
    button,
    config::{AsrConfig, Config},
    Speaky, ACCENT, MUTED,
};
use gpui::{div, prelude::*, px, rgb, Context, Div};
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Default)]
pub(crate) struct Diagnostics {
    pub checking: bool,
    inputs: Option<InputDevices>,
    refreshing_inputs: bool,
    permission: Option<bool>,
    level: Arc<AtomicU32>,
    notice: String,
}

impl Speaky {
    pub(super) fn refresh_input_devices(&mut self, cx: &mut Context<Self>) {
        if self.diagnostics.refreshing_inputs {
            return;
        }
        self.diagnostics.refreshing_inputs = true;
        cx.spawn(async move |entity, cx| {
            let inputs = cx
                .background_spawn(async { InputDevices::discover() })
                .await;
            let _ = entity.update(cx, |view, cx| {
                view.diagnostics.refreshing_inputs = false;
                if view.diagnostics.inputs.as_ref() != Some(&inputs) {
                    view.diagnostics.inputs = Some(inputs);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn diagnostic_input_devices(&self) -> Option<&InputDevices> {
        self.diagnostics.inputs.as_ref()
    }

    pub(super) fn microphone_label(&self, asr: &AsrConfig) -> &str {
        if let Some(recording) = &self.recording {
            return recording.recorder.opened_input_name().unwrap_or("正在录音");
        }
        self.diagnostic_input_devices()
            .map_or("正在检查输入设备…", |inputs| {
                inputs
                    .preferred_name(asr.audio_device, asr.audio_device_name.as_deref())
                    .unwrap_or("未检测到可用麦克风")
            })
    }

    pub(crate) fn test_microphone(&mut self, cx: &mut Context<Self>) {
        if self.diagnostics.checking || self.recording.is_some() || self.starting {
            return;
        }
        self.diagnostics.checking = true;
        self.diagnostics.notice = "正在检测麦克风，请说一句话…".into();
        self.diagnostics.level.store(0, Ordering::Relaxed);
        let level = self.diagnostics.level.clone();
        cx.spawn(async move |entity, cx| {
            let allowed = crate::onboarding::microphone_permission().await;
            let result = if allowed {
                cx.background_spawn(async move {
                    let config = Config::load().map_err(|e| e.to_string())?;
                    let input = config.core.asr;
                    let mut recorder = AudioRecorder::new_with_name(
                        input.audio_device,
                        input.audio_device_name.as_deref(),
                        input.audio_gain,
                    )?;
                    recorder.start()?;
                    let device = recorder
                        .opened_input_name()
                        .unwrap_or("当前输入")
                        .to_owned();
                    let mut peak = 0.0f32;
                    for _ in 0..60 {
                        let value = recorder.get_audio_level();
                        peak = peak.max(value);
                        level.store(value.to_bits(), Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    recorder.stop();
                    let status = if peak > 0.001 {
                        "检测到声音"
                    } else {
                        "未检测到声音，请检查输入音量"
                    };
                    Ok::<_, String>(format!("上次检测：{device} · {status}。"))
                })
                .await
            } else {
                Err("麦克风权限未开通，请在系统设置中允许访问。".into())
            };
            let _ = entity.update(cx, |view, cx| {
                view.diagnostics.checking = false;
                // Windows has no separate permission prompt for this desktop app.
                // A failed stream does not distinguish denied access from a missing device.
                view.diagnostics.permission = (!cfg!(windows) || result.is_ok()).then_some(allowed);
                view.diagnostics.level.store(0, Ordering::Relaxed);
                view.diagnostics.notice = result.unwrap_or_else(|e| format!("麦克风检测失败：{e}"));
                view.refresh_input_devices(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn input_status(&self, config: &Config, cx: &mut Context<Self>) -> Div {
        let name = self.microphone_label(&config.core.asr);
        let level = self.diagnostic_level();
        let text = if self.diagnostics.notice.is_empty() {
            "麦克风：待检测"
        } else {
            &self.diagnostics.notice
        };
        let rewrite = config.llm.organizer_service();
        let llm = if config.llm.enabled {
            rewrite.model.as_str()
        } else {
            "未启用（建议开启）"
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(12.))
            .text_color(rgb(MUTED()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(crate::icons::Icon::Microphone.render(ACCENT()))
                    .child(name.to_owned())
                    .child(
                        div()
                            .w(px(70.))
                            .h(px(5.))
                            .rounded_full()
                            .bg(rgb(crate::BORDER()))
                            .child(
                                div()
                                    .w(px(70. * (level * 6.).sqrt().clamp(0., 1.)))
                                    .h_full()
                                    .rounded_full()
                                    .bg(rgb(ACCENT())),
                            ),
                    )
                    .child(
                        button(
                            if self.diagnostics.checking {
                                "检测中…"
                            } else {
                                "测试麦克风"
                            },
                            false,
                        )
                        .id("test-microphone")
                        .on_click(cx.listener(|view, _, _, cx| view.test_microphone(cx))),
                    ),
            )
            .child(text.to_owned())
            .when(cfg!(target_os = "macos"), |row| {
                #[cfg(target_os = "macos")]
                let row = row.child(if self.mac_dictation_access {
                    "辅助功能：已开通"
                } else {
                    "辅助功能：未开通"
                });
                row
            })
            .child(format!(
                "ASR · {}   /   LLM · {llm}",
                crate::model_picker::selected_label(config)
            ))
    }

    pub(super) fn diagnostic_level(&self) -> f32 {
        self.recording
            .as_ref()
            .map(|r| r.recorder.get_audio_level())
            .unwrap_or_else(|| f32::from_bits(self.diagnostics.level.load(Ordering::Relaxed)))
    }

    pub(super) fn diagnostic_message(&self) -> &str {
        &self.diagnostics.notice
    }

    pub(super) fn diagnostic_permission(&self) -> &'static str {
        match self.diagnostics.permission {
            Some(true) => "麦克风权限 · 已开通",
            Some(false) => "麦克风权限 · 未开通",
            None => "麦克风权限 · 待检测",
        }
    }
}

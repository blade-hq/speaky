use crate::{
    audio::AudioRecorder, button, config::Config, pipeline::RewriteDebug, Speaky, ACCENT, MUTED,
};
use gpui::{div, prelude::*, px, rgb, Context, Div};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Default)]
pub(crate) struct Diagnostics {
    pub checking: bool,
    level: Arc<AtomicU32>,
    notice: String,
    debug: Option<(PathBuf, Result<RewriteDebug, String>)>,
}

impl Speaky {
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
                    let mut peak = 0.0f32;
                    for _ in 0..60 {
                        let value = recorder.get_audio_level();
                        peak = peak.max(value);
                        level.store(value.to_bits(), Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    recorder.stop();
                    Ok::<_, String>(
                        if peak > 0.001 {
                            "麦克风可用，已检测到声音。"
                        } else {
                            "麦克风已连接，未检测到声音，请检查输入设备或音量。"
                        }
                        .to_owned(),
                    )
                })
                .await
            } else {
                Err("麦克风权限未开通，请在系统设置中允许访问。".into())
            };
            let _ = entity.update(cx, |view, cx| {
                view.diagnostics.checking = false;
                view.diagnostics.notice = result.unwrap_or_else(|e| format!("麦克风检测失败：{e}"));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn input_status(&self, cx: &mut Context<Self>) -> Div {
        let config = Config::load().unwrap_or_default();
        let name = config
            .core
            .asr
            .audio_device_name
            .as_deref()
            .unwrap_or("系统默认麦克风");
        let level = self
            .recording
            .as_ref()
            .map(|r| r.recorder.get_audio_level())
            .unwrap_or_else(|| f32::from_bits(self.diagnostics.level.load(Ordering::Relaxed)));
        let text = if self.diagnostics.notice.is_empty() {
            "麦克风：待检测"
        } else {
            &self.diagnostics.notice
        };
        let llm = if config.llm.enabled {
            config.llm.openai.model.as_str()
        } else {
            "未启用（建议开启）"
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(12.))
            .text_color(rgb(MUTED))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(crate::icons::Icon::Microphone.render(ACCENT))
                    .child(name.to_owned())
                    .child(
                        div()
                            .w(px(70.))
                            .h(px(5.))
                            .rounded_full()
                            .bg(rgb(crate::BORDER))
                            .child(
                                div()
                                    .w(px(70. * (level * 6.).sqrt().clamp(0., 1.)))
                                    .h_full()
                                    .rounded_full()
                                    .bg(rgb(ACCENT)),
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
                crate::model_picker::selected_label(&config)
            ))
    }

    pub(crate) fn rewrite_debug(&self, path: &Path, cx: &mut Context<Self>) -> Div {
        if !Config::load().is_ok_and(|c| c.desktop.debug_mode)
            || !path.with_extension("debug.json").is_file()
        {
            return div();
        }
        let selected = self.diagnostics.debug.as_ref().filter(|(p, _)| p == path);
        let path = path.to_owned();
        div()
            .mt_2()
            .child(
                button(
                    if selected.is_some() {
                        "收起改写请求"
                    } else {
                        "查看改写请求"
                    },
                    false,
                )
                .id("show-rewrite-debug")
                .on_click(cx.listener(move |view, _, _, cx| {
                    if view
                        .diagnostics
                        .debug
                        .as_ref()
                        .is_some_and(|(p, _)| p == &path)
                    {
                        view.diagnostics.debug = None;
                    } else {
                        let result = std::fs::read(path.with_extension("debug.json"))
                            .map_err(|e| e.to_string())
                            .and_then(|bytes| {
                                serde_json::from_slice(&bytes).map_err(|e| e.to_string())
                            });
                        view.diagnostics.debug = Some((path.clone(), result));
                    }
                    cx.notify();
                })),
            )
            .when_some(selected, |body, (_, result)| {
                let text = match result {
                    Ok(debug) => format!(
                        "模型：{}\n系统提示词：\n{}\n实际输入：\n{}{}",
                        debug.model,
                        debug.system,
                        debug.input,
                        debug
                            .error
                            .as_ref()
                            .map(|e| format!("\n调用失败：{e}"))
                            .unwrap_or_default()
                    ),
                    Err(error) => format!("无法读取调试记录：{error}"),
                };
                body.child(
                    div()
                        .mt_2()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(crate::SIDEBAR))
                        .text_size(px(12.))
                        .child(text),
                )
            })
    }
}

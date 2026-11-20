#[path = "../../speaky-tauri/src-tauri/src/audio/mod.rs"]
mod audio;
#[path = "../../speaky-tauri/src-tauri/src/config/mod.rs"]
mod config;
mod drafts;
#[path = "../../speaky-tauri/src-tauri/src/engines/mod.rs"]
mod engines;
#[path = "../../speaky-tauri/src-tauri/src/history.rs"]
mod history;
mod hotkey;

use gpui::{
    div, point, prelude::*, px, rgb, size, App, Application, Bounds, ClipboardItem, Context,
    MouseButton, ScrollHandle, Window, WindowBounds, WindowOptions,
};
use std::{path::PathBuf, time::Duration};

#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../../speaky-tauri/src-tauri/Info.plist");

struct Recording {
    recorder: audio::AudioRecorder,
    draft: PathBuf,
    preview_cursor: usize,
    preview_pending: bool,
    preview: String,
}

struct Speaky {
    recording: Option<Recording>,
    _hotkey: Option<hotkey::GlobalHotkey>,
    hotkey_status: String,
    status: String,
    result: String,
    transcribing: bool,
    pending_preview: String,
    transcript_scroll: ScrollHandle,
}

impl Speaky {
    fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        if self.transcribing {
            return;
        }

        if let Some(mut recording) = self.recording.take() {
            let audio = recording.recorder.stop();
            if audio.is_empty() {
                self.status = "没有收到音频，请检查麦克风权限。".into();
                cx.notify();
                return;
            }

            self.pending_preview = recording.preview;
            self.transcribe(recording.draft, Some(audio), cx);
        } else {
            let result = config::Config::load()
                .map_err(|error| error.to_string())
                .and_then(|config| {
                    let asr = config.core.asr;
                    let mut recorder = audio::AudioRecorder::new_with_name(
                        asr.audio_device,
                        asr.audio_device_name.as_deref(),
                        asr.audio_gain,
                    )?;
                    recorder.start()?;
                    Ok(recorder)
                });
            match result {
                Ok(recorder) => {
                    self.pending_preview.clear();
                    self.transcript_scroll.set_offset(point(px(0.), px(0.)));
                    self.recording = Some(Recording {
                        recorder,
                        draft: drafts::new_path(),
                        preview_cursor: 0,
                        preview_pending: false,
                        preview: String::new(),
                    });
                    self.status = "录音中。切换应用后仍会继续录音。".into();
                }
                Err(error) => self.status = error,
            }
        }
        cx.notify();
    }

    fn checkpoint(&mut self, cx: &mut Context<Self>) {
        if let Some(recording) = &mut self.recording {
            if let Err(error) = drafts::save(&recording.draft, &recording.recorder.wav_snapshot()) {
                self.status = format!("保存录音草稿失败：{error}");
                cx.notify();
            }
            if !recording.preview_pending {
                if let Some((next, audio)) = recording
                    .recorder
                    .wav_preview_since(recording.preview_cursor)
                {
                    recording.preview_cursor = next;
                    let draft = recording.draft.clone();
                    self.preview(draft, audio, cx);
                }
            }
        }
    }

    fn preview(&mut self, draft: PathBuf, audio: Vec<u8>, cx: &mut Context<Self>) {
        let Some(recording) = self
            .recording
            .as_mut()
            .filter(|recording| recording.draft == draft && !recording.preview_pending)
        else {
            return;
        };
        recording.preview_pending = true;
        let task = cx.background_spawn(async move {
            let config = config::Config::load().map_err(|error| error.to_string())?;
            let engine = engines::create_engine(&config)
                .ok_or_else(|| "请先配置语音识别服务。".to_owned())?;
            engine
                .transcribe(&audio, &config.core.asr.language)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                let Some(recording) = view
                    .recording
                    .as_mut()
                    .filter(|recording| recording.draft == draft)
                else {
                    return;
                };
                recording.preview_pending = false;
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        if !recording.preview.is_empty() {
                            recording.preview.push(' ');
                        }
                        recording.preview.push_str(text.trim());
                        view.transcript_scroll.scroll_to_bottom();
                        cx.notify();
                    }
                    Ok(_) => {}
                    Err(error) => log::debug!("Recording preview failed: {error}"),
                }
            });
        })
        .detach();
    }

    fn transcribe(&mut self, draft: PathBuf, final_audio: Option<Vec<u8>>, cx: &mut Context<Self>) {
        if self.recording.is_some() || self.transcribing {
            return;
        }
        self.transcribing = true;
        self.status = "正在转写…".into();
        let task = cx.background_spawn(async move {
            let audio = match final_audio {
                Some(audio) => {
                    drafts::save(&draft, &audio)
                        .map_err(|error| format!("保存录音失败：{error}"))?;
                    audio
                }
                None => std::fs::read(&draft).map_err(|error| error.to_string())?,
            };
            let transcribe = || {
                let config = config::Config::load().map_err(|error| error.to_string())?;
                let engine = engines::create_engine(&config)
                    .ok_or_else(|| "请先配置语音识别服务。".to_owned())?;
                let text = engine
                    .transcribe(&audio, &config.core.asr.language)
                    .map_err(|error| error.to_string())?;
                if text.trim().is_empty() {
                    return Err("未识别到文字。".to_owned());
                }
                history::add(&text, engine.name(), false);
                Ok(text)
            };
            transcribe().map_err(|error| format!("{error}\n录音已保存至 {}", draft.display()))
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.transcribing = false;
                match result {
                    Ok(text) => {
                        view.result = text;
                        view.pending_preview.clear();
                        view.transcript_scroll.set_offset(point(px(0.), px(0.)));
                        view.status = "转写完成，可以复制文字。".into();
                    }
                    Err(error) => view.status = error,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for Speaky {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let record_label = if self.recording.is_some() {
            "结束录音"
        } else if self.transcribing {
            "转写中"
        } else {
            "开始录音"
        };
        let preview = self
            .recording
            .as_ref()
            .map(|recording| recording.preview.as_str())
            .unwrap_or(&self.pending_preview);
        let show_preview = self.recording.is_some() || !preview.is_empty();
        let transcript = if show_preview || self.transcribing {
            preview
        } else {
            &self.result
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(rgb(0xf7f8fa))
            .text_color(rgb(0x161d2c))
            .child(div().text_xl().child("Speaky"))
            .child(self.hotkey_status.clone())
            .child(self.status.clone())
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x4231c5))
                            .text_color(rgb(0xffffff))
                            .child(record_label)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, _, _, cx| view.toggle_recording(cx)),
                            ),
                    )
                    .when(
                        !show_preview && !self.transcribing && !self.result.is_empty(),
                        |actions| {
                            actions.child(
                                div()
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(0xe5e7ec))
                                    .child("复制转写")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|view, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                view.result.clone(),
                                            ));
                                            view.status = "文字已复制。".into();
                                            cx.notify();
                                        }),
                                    ),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .id("transcript")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.transcript_scroll)
                    .when(show_preview, |body| {
                        body.child(div().mb_2().child("听写预览"))
                    })
                    .child(transcript.to_owned())
                    .when(self.recording.is_none() && !self.transcribing, |body| {
                        let transcripts = history::recent(5);
                        let body = body.when(!transcripts.is_empty(), |body| {
                            body.child(div().mt_4().child("最近的转写")).children(
                                transcripts.into_iter().map(|item| {
                                    let preview = item.text.chars().take(60).collect::<String>();
                                    div()
                                        .mt_2()
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(0xe5e7ec))
                                        .child(preview)
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |view, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    item.text.clone(),
                                                ));
                                                view.result.clone_from(&item.text);
                                                view.pending_preview.clear();
                                                view.transcript_scroll
                                                    .set_offset(point(px(0.), px(0.)));
                                                view.status = "历史转写已复制。".into();
                                                cx.notify();
                                            }),
                                        )
                                }),
                            )
                        });
                        let drafts = drafts::recent();
                        body.when(!drafts.is_empty(), |body| {
                            body.child(div().mt_4().child("最近的录音草稿")).children(
                                drafts.into_iter().enumerate().map(|(index, draft)| {
                                    div()
                                        .mt_2()
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(0xe5e7ec))
                                        .child(format!("重新转写草稿 {}", index + 1))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |view, _, _, cx| {
                                                view.transcribe(draft.clone(), None, cx);
                                            }),
                                        )
                                }),
                            )
                        })
                    }),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(520.), px(360.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    let (hotkey, hotkey_status) = match hotkey::GlobalHotkey::register() {
                        Ok((handle, mut presses)) => {
                            cx.spawn(async move |entity, cx| {
                                while presses.recv().await.is_some() {
                                    if entity.update(cx, Speaky::toggle_recording).is_err() {
                                        break;
                                    }
                                }
                            })
                            .detach();
                            (Some(handle), format!("全局快捷键 {}", hotkey::LABEL))
                        }
                        Err(error) => (None, error),
                    };
                    let executor = cx.background_executor().clone();
                    cx.spawn(async move |entity, cx| loop {
                        executor.timer(Duration::from_secs(2)).await;
                        if entity.update(cx, Speaky::checkpoint).is_err() {
                            break;
                        }
                    })
                    .detach();
                    Speaky {
                        recording: None,
                        _hotkey: hotkey,
                        hotkey_status,
                        status: "准备录音".into(),
                        result: history::recent(1)
                            .into_iter()
                            .next()
                            .map(|item| item.text)
                            .unwrap_or_default(),
                        transcribing: false,
                        pending_preview: String::new(),
                        transcript_scroll: ScrollHandle::new(),
                    }
                })
            },
        )
        .expect("could not open Speaky");
        cx.activate(true);
    });
}

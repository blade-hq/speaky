use super::*;
use speaky_core::{autodl, pipeline, polish};
use std::time::Duration;

const ORGANIZER_ENABLED: usize = 200;
const ORGANIZER_ADDRESS: usize = 201;
const ORGANIZER_MODEL: usize = 202;
const ORGANIZER_KEY: usize = 203;
const ORGANIZER_PROMPT: usize = 204;
const ORGANIZER_SAVE: usize = 205;
const ORGANIZER_TEST: usize = 206;
const ORGANIZER_RESULT: usize = 207;
const MIC_TEST: usize = 208;
const ASR_TEST: usize = 209;
const DIAGNOSTICS_RESULT: usize = 210;
const DEBUG_MODE: usize = 211;
pub(super) const ORGANIZE_HISTORY: usize = 212;
const ORGANIZER_SAMPLE: &str = "嗯明天下午两点开会然后请大家提前看一下方案。";

impl Window {
    pub(super) fn organizer_layout(&mut self) {
        self.heading("文本整理");
        self.control(
            ORGANIZER_ENABLED,
            "BUTTON",
            "转录结束后自动整理",
            (20, 89, 260, 23),
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        );
        self.send(
            ORGANIZER_ENABLED,
            BM_SETCHECK,
            usize::from(self.config.llm.enabled),
            0,
        );
        let service = self.config.llm.organizer_service().into_owned();
        for (id, label, value, y, style) in [
            (
                ORGANIZER_ADDRESS,
                "API 地址",
                service.base_url.as_str(),
                122,
                0,
            ),
            (ORGANIZER_MODEL, "模型 ID", service.model.as_str(), 154, 0),
            (
                ORGANIZER_KEY,
                "API 密钥",
                service.api_key.as_str(),
                186,
                ES_PASSWORD as u32,
            ),
        ] {
            self.label(label, 20, y + 3, 78);
            self.control(
                id,
                "EDIT",
                value,
                (105, y, 497, 25),
                WS_TABSTOP | ES_AUTOHSCROLL as u32 | style,
            );
        }
        self.label("自定义提示词（留空使用默认提示词）", 20, 220, 560);
        let prompt = self
            .config
            .llm
            .custom_prompt
            .replace("\r\n", "\n")
            .replace('\n', "\r\n");
        self.control(
            ORGANIZER_PROMPT,
            "EDIT",
            &prompt,
            (20, 243, 582, 64),
            WS_TABSTOP | WS_VSCROLL | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
        );
        self.button(ORGANIZER_SAVE, "保存设置", 20, 317, 113);
        self.button(ORGANIZER_TEST, "测试整理服务", 143, 317, 141);
        self.diagnostic_output(ORGANIZER_RESULT, 357, 72, "测试结果会显示在这里。");
    }

    pub(super) fn diagnostics_layout(&mut self) {
        self.heading("设置与诊断");
        self.button(NAV_MODELS, "识别模型与麦克风设置", 20, 91, 204);
        self.button(NAV_ORGANIZER, "文本整理设置", 235, 91, 148);
        self.control(
            DEBUG_MODE,
            "BUTTON",
            "在检测结果中显示调试信息",
            (20, 143, 330, 24),
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        );
        self.send(
            DEBUG_MODE,
            BM_SETCHECK,
            usize::from(self.config.desktop.debug_mode),
            0,
        );
        self.label(
            "麦克风检测会监听三秒。语音识别检测使用内置音频。",
            20,
            181,
            582,
        );
        self.button(MIC_TEST, "检测麦克风", 20, 213, 139);
        self.button(ASR_TEST, "检测语音识别", 168, 213, 144);
        self.diagnostic_output(DIAGNOSTICS_RESULT, 257, 172, "检测结果会显示在这里。");
    }

    fn diagnostic_output(&mut self, id: usize, y: i32, height: i32, text: &str) {
        self.control(
            id,
            "EDIT",
            text,
            (20, y, 582, height),
            WS_TABSTOP | WS_VSCROLL | (ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32,
        );
    }

    fn organizer_settings(&self) -> Result<Config, String> {
        let mut candidate = self.config.clone();
        let previous = self.config.llm.organizer_service();
        let address = self.text(ORGANIZER_ADDRESS).trim().to_owned();
        let model = self.text(ORGANIZER_MODEL).trim().to_owned();
        let key = self.text(ORGANIZER_KEY).trim().to_owned();
        if address.trim_end_matches('/') != previous.base_url.trim_end_matches('/')
            && !key.is_empty()
            && key == previous.api_key
        {
            self.set(ORGANIZER_KEY, "");
            return Err("API 地址已更改，请重新输入对应的 API 密钥。".into());
        }
        candidate.llm.enabled = self.send(ORGANIZER_ENABLED, BM_GETCHECK, 0, 0) == 1;
        candidate.llm.custom_prompt = self.text(ORGANIZER_PROMPT).replace("\r\n", "\n");
        if autodl::is_base_url(&address) {
            candidate.llm.autodl_api_key = key.clone();
            candidate.llm.autodl_model = model.clone();
            if autodl::is_base_url(&candidate.llm.openai.base_url) {
                candidate.llm.openai.api_key.clear();
            }
            if model.is_empty() {
                return Err("请输入 AutoDL 整理模型 ID。".into());
            }
        } else {
            if autodl::is_base_url(&previous.base_url) {
                candidate.llm.autodl_api_key =
                    self.config.llm.autodl_key().unwrap_or_default().into();
            }
            candidate.llm.autodl_model.clear();
            candidate.llm.openai.base_url = address;
            candidate.llm.openai.model = model;
            candidate.llm.openai.api_key = key;
        }
        Ok(candidate)
    }

    pub(super) fn diagnostics_command(&mut self, id: usize) -> bool {
        if !matches!(
            id,
            ORGANIZER_SAVE | ORGANIZER_TEST | MIC_TEST | ASR_TEST | DEBUG_MODE | ORGANIZE_HISTORY
        ) {
            return false;
        }
        if self.audio_busy() || self.auxiliary_busy || self.editing {
            if id == DEBUG_MODE {
                self.send(
                    DEBUG_MODE,
                    BM_SETCHECK,
                    usize::from(self.config.desktop.debug_mode),
                    0,
                );
            }
            self.status("请先完成当前操作。");
            return true;
        }
        match id {
            ORGANIZER_SAVE => match self.organizer_settings().and_then(|candidate| {
                candidate.save().map_err(|error| error.to_string())?;
                Ok(candidate)
            }) {
                Ok(candidate) => {
                    self.config = candidate;
                    self.status("文本整理设置已保存。");
                }
                Err(error) => self.status(error),
            },
            ORGANIZER_TEST => match self.organizer_settings() {
                Ok(candidate) => self.test_organizer(candidate),
                Err(error) => self.status(error),
            },
            DEBUG_MODE => {
                let mut candidate = self.config.clone();
                candidate.desktop.debug_mode = self.send(DEBUG_MODE, BM_GETCHECK, 0, 0) == 1;
                match candidate.save() {
                    Ok(()) => {
                        self.config = candidate;
                        self.status("调试设置已保存。");
                    }
                    Err(error) => {
                        self.send(
                            DEBUG_MODE,
                            BM_SETCHECK,
                            usize::from(self.config.desktop.debug_mode),
                            0,
                        );
                        self.status(format!("保存调试设置失败：{error}"));
                    }
                }
            }
            MIC_TEST => self.test_microphone(),
            ASR_TEST => self.test_recognition(),
            ORGANIZE_HISTORY => self.organize_history(),
            _ => unreachable!(),
        }
        true
    }

    fn test_organizer(&mut self, candidate: Config) {
        self.auxiliary_busy = true;
        self.set(ORGANIZER_RESULT, "正在测试文本整理服务…");
        self.status("正在测试文本整理服务…");
        let updates = self.updates.clone();
        thread::spawn(move || {
            let service = candidate.llm.organizer_service();
            let prompt = if candidate.llm.custom_prompt.trim().is_empty() {
                polish::Profile::Balanced.prompt()
            } else {
                candidate.llm.custom_prompt.clone()
            };
            let result = polish::organize_with_prompt(&service, &prompt, ORGANIZER_SAMPLE);
            let result = if candidate.desktop.debug_mode {
                let details = format!(
                    "模型：{}\n输入：{ORGANIZER_SAMPLE}\n提示词：{prompt}",
                    service.model
                );
                result
                    .map(|text| format!("{text}\n\n{details}"))
                    .map_err(|error| format!("{error}\n\n{details}"))
            } else {
                result
            };
            updates.send(Event::OrganizerTest(result));
        });
    }

    fn test_microphone(&mut self) {
        self.auxiliary_busy = true;
        self.set(DIAGNOSTICS_RESULT, "正在检测麦克风，请说一句话…");
        self.status("正在检测麦克风，请说一句话…");
        let input = self.config.core.asr.clone();
        let updates = self.updates.clone();
        thread::spawn(move || {
            let result = (|| {
                let mut recorder = AudioRecorder::new_with_name(
                    input.audio_device,
                    input.audio_device_name.as_deref(),
                    input.audio_gain,
                )?;
                recorder.start()?;
                let name = recorder
                    .opened_input_name()
                    .unwrap_or("当前输入设备")
                    .to_owned();
                let mut peak = 0.0_f32;
                for _ in 0..60 {
                    thread::sleep(Duration::from_millis(50));
                    peak = peak.max(recorder.get_audio_level());
                }
                recorder.stop();
                if peak > 0.001 {
                    Ok(format!("{name}：检测到声音，峰值 {peak:.3}。"))
                } else {
                    Err(format!("{name}：未检测到声音，请检查输入设备和音量。"))
                }
            })();
            updates.send(Event::MicrophoneTest(result));
        });
    }

    fn test_recognition(&mut self) {
        self.auxiliary_busy = true;
        self.set(DIAGNOSTICS_RESULT, "正在用内置音频检测语音识别…");
        self.status("正在检测语音识别…");
        let config = self.config.clone();
        let updates = self.updates.clone();
        thread::spawn(move || {
            let result = engines::recognition_engine(&config).and_then(|engine| {
                let result = engine
                    .transcribe(SAMPLE_AUDIO, "zh")
                    .map_err(|error| error.to_string())?;
                if result.trim().is_empty() {
                    return Err("识别服务没有返回文字。".into());
                }
                Ok(result)
            });
            updates.send(Event::RecognitionTest(result));
        });
    }

    fn organize_history(&mut self) {
        let Some(path) = self.selected_draft() else {
            self.status("请选择一条已转录的录音。");
            return;
        };
        self.auxiliary_busy = true;
        self.status("正在整理所选记录…");
        let config = self.config.clone();
        let updates = self.updates.clone();
        thread::spawn(move || {
            let result = drafts::list()
                .into_iter()
                .find(|entry| entry.path == path)
                .and_then(|entry| match entry.state {
                    drafts::State::Transcribed { text } if !text.trim().is_empty() => Some(text),
                    _ => None,
                })
                .ok_or_else(|| "请选择一条已转录的录音。".into())
                .and_then(|source| pipeline::organize_saved(&config, &path, &source).map(|_| ()));
            updates.send(Event::OrganizedHistory(path, result));
        });
    }

    pub(super) fn diagnostics_event(&mut self, event: Event) -> Option<Event> {
        let (output, finished, result) = match event {
            Event::OrganizerTest(result) => (ORGANIZER_RESULT, "文本整理测试", result),
            Event::MicrophoneTest(result) => (DIAGNOSTICS_RESULT, "麦克风检测", result),
            Event::RecognitionTest(result) => (DIAGNOSTICS_RESULT, "语音识别检测", result),
            Event::OrganizedHistory(path, result) => {
                self.auxiliary_busy = false;
                match result {
                    Ok(()) => {
                        if self.panel == Panel::History
                            && self.selected_draft().as_ref() == Some(&path)
                        {
                            self.organized_visible = true;
                            self.fill_history();
                        }
                        self.status("整理稿已保存。");
                    }
                    Err(error) => self.status(format!("整理未完成：{error}")),
                }
                return None;
            }
            other => return Some(other),
        };
        self.auxiliary_busy = false;
        let (message, details) = match result {
            Ok(details) => (format!("{finished}通过。"), details),
            Err(details) => (format!("{finished}失败。"), details),
        };
        let details = details.replace("\r\n", "\n").replace('\n', "\r\n");
        self.set(output, &format!("{message}\r\n{details}"));
        self.status(message);
        None
    }
}

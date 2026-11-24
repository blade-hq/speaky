//! Clients for user-configured transcription endpoints. Detection and ordinary
//! dictation exercise the same request, with a shorter deadline for detection.
mod funasr;
#[cfg(test)]
mod tests;

use super::{http_status_error, openai::RUNTIME, Engine, EngineError, EngineResult, OpenAIEngine};
use reqwest::{header::HeaderValue, multipart, Client, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Protocol {
    #[default]
    #[serde(rename = "openai")]
    OpenAI,
    WhisperCpp,
    #[serde(rename = "funasr")]
    FunAsr,
}

impl Protocol {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::OpenAI => "OpenAI 兼容转写",
            Self::WhisperCpp => "whisper.cpp 原生 HTTP",
            Self::FunAsr => "FunASR WebSocket",
        }
    }
}

pub(crate) struct RemoteEngine {
    protocol: Protocol,
    endpoint: Url,
    openai: Option<OpenAIEngine>,
    authorization: Option<HeaderValue>,
    client: Client,
}

impl RemoteEngine {
    pub(crate) fn new(
        protocol: Protocol,
        endpoint: &str,
        model: &str,
        api_key: &str,
    ) -> Result<Self, String> {
        let endpoint = Url::parse(endpoint.trim()).map_err(|_| "服务地址无效。")?;
        let valid_scheme = match protocol {
            Protocol::FunAsr => matches!(endpoint.scheme(), "ws" | "wss"),
            _ => matches!(endpoint.scheme(), "http" | "https"),
        };
        if !valid_scheme
            || !endpoint.has_host()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(format!("请检查 {} 的服务地址。", protocol.label()));
        }
        if protocol == Protocol::OpenAI && model.trim().is_empty() {
            return Err("OpenAI 兼容转写需要模型 ID。".into());
        }
        let authorization = if api_key.trim().is_empty() {
            None
        } else {
            let mut value = HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))
                .map_err(|_| "API 密钥包含无效字符。")?;
            value.set_sensitive(true);
            Some(value)
        };
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| "无法初始化识别服务连接。")?;
        Ok(Self {
            protocol,
            openai: (protocol == Protocol::OpenAI).then(|| {
                OpenAIEngine::at_endpoint(
                    api_key.trim(),
                    model.trim(),
                    endpoint.to_string(),
                    client.clone(),
                )
            }),
            endpoint,
            authorization,
            client,
        })
    }

    pub(crate) fn probe(&self, audio: &[u8], timeout: Duration) -> EngineResult {
        self.run(audio, "zh", timeout)
    }

    fn run(&self, audio: &[u8], language: &str, timeout: Duration) -> EngineResult {
        RUNTIME.block_on(async {
            tokio::time::timeout(timeout, async {
                match self.protocol {
                    Protocol::FunAsr => {
                        funasr::transcribe(
                            &self.endpoint,
                            self.authorization.as_ref(),
                            audio,
                            language,
                        )
                        .await
                    }
                    Protocol::OpenAI => {
                        self.openai
                            .as_ref()
                            .unwrap()
                            .transcribe_async(audio, language, &[], true)
                            .await
                    }
                    Protocol::WhisperCpp => self.http_transcribe(audio, language).await,
                }
            })
            .await
            .map_err(|_| EngineError::NetworkError("服务转写超时，请检查地址和模型状态。".into()))?
        })
    }

    async fn http_transcribe(&self, audio: &[u8], language: &str) -> EngineResult {
        let file = multipart::Part::bytes(audio.to_owned())
            .file_name("sample.wav")
            .mime_str("audio/wav")
            .map_err(|_| EngineError::AudioProcessingError("无法创建音频请求。".into()))?;
        let mut form = multipart::Form::new()
            .part("file", file)
            .text("response_format", "json");
        let language = language.split(['-', '_']).next().unwrap_or("").trim();
        if !language.is_empty() && language != "auto" {
            form = form.text("language", language.to_owned());
        } else {
            form = form.text("language", "auto");
        }
        let mut request = self.client.post(self.endpoint.clone()).multipart(form);
        if let Some(header) = &self.authorization {
            request = request.header(reqwest::header::AUTHORIZATION, header.clone());
        }
        let mut response = request.send().await.map_err(|err| {
            EngineError::NetworkError(
                if err.is_timeout() {
                    "连接超时。"
                } else {
                    "无法连接转写服务，请检查地址、网络和 TLS 配置。"
                }
                .into(),
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(status.as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| EngineError::NetworkError("读取转写响应失败。".into()))?
        {
            if body.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(EngineError::InvalidResponse("转写响应过大。".into()));
            }
            body.extend_from_slice(&chunk);
        }
        #[derive(Deserialize)]
        struct Transcript {
            text: String,
        }
        let result: Transcript = serde_json::from_slice(&body)
            .map_err(|_| EngineError::InvalidResponse("服务未返回包含 text 的转写结果。".into()))?;
        Ok(result.text.trim().to_owned())
    }
}

impl Engine for RemoteEngine {
    fn name(&self) -> &str {
        self.openai
            .as_ref()
            .map_or(self.protocol.label(), Engine::name)
    }

    fn is_available(&self) -> bool {
        true
    }

    fn transcribe(&self, audio: &[u8], language: &str) -> EngineResult {
        self.run(audio, language, Duration::from_secs(3600))
    }
}

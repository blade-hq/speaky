use super::{Engine, EngineError, EngineResult, TranscriptionPass};
use log::{debug, error};
use once_cell::sync::Lazy;
use reqwest::multipart;
use serde::Deserialize;
use std::time::Duration;
use tokio::runtime::Runtime;

#[derive(Deserialize)]
struct CatalogModel {
    id: String,
    framework: Option<String>,
    speaky_owner: Option<u32>,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Debug, Clone)]
struct ResolvedModel {
    wire_id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonTranscript {
    Text(String),
    Object { text: String },
}

fn decode_transcript(body: &str, is_json: bool) -> EngineResult {
    let text = if is_json {
        match serde_json::from_str::<JsonTranscript>(body)
            .map_err(|error| EngineError::InvalidResponse(error.to_string()))?
        {
            JsonTranscript::Text(text) | JsonTranscript::Object { text } => text,
        }
    } else {
        body.to_owned()
    };
    Ok(text.trim().to_owned())
}

/// Recognition is called from a worker thread, so provide a runtime for the
/// async HTTP implementation instead of assuming a Tokio context exists.
pub(crate) static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("speaky-http")
        .worker_threads(2)
        .build()
        .expect("Failed to create Tokio runtime")
});

/// OpenAI Whisper API engine
///
/// This engine uses OpenAI's Whisper API for speech recognition.
/// It requires an API key to be configured in the application settings.
#[derive(Debug, Clone)]
pub struct OpenAIEngine {
    api_key: String,
    model: String,
    base_url: String,
    use_vocabulary_prompt: bool,
    client: reqwest::Client,
    local: bool,
    framework: Option<crate::config::Framework>,
    name: String,
    resolved: std::sync::OnceLock<ResolvedModel>,
}

impl OpenAIEngine {
    pub fn from_framework(
        framework: crate::config::Framework,
        config: &crate::config::Config,
    ) -> Result<Self, String> {
        let service = framework.service(config);
        let url = reqwest::Url::parse(&service.base_url).map_err(|_| "识别框架的服务地址无效。")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.has_host()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("识别框架需要完整的 HTTP 或 HTTPS 服务地址。".into());
        }
        if service.model.trim().is_empty() || (!loopback(&url) && service.api_key.trim().is_empty())
        {
            return Err("请设置模型名称和远程识别服务的 API 密钥。".into());
        }
        let mut engine = Self::with_timeout(
            &service.api_key,
            &service.model,
            url.as_str(),
            Duration::from_secs(3600),
        );
        engine.name = framework.display_model(&service.model);
        engine.framework = Some(framework);
        Ok(engine)
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            request
        } else {
            request.bearer_auth(&self.api_key)
        }
    }

    pub fn from_service(service: &crate::config::OpenAIConfig) -> Self {
        let mut engine = Self::new(&service.api_key, &service.model, &service.base_url)
            .with_vocabulary_prompt(service.use_vocabulary_prompt);
        engine.name = service.model.clone();
        engine
    }

    #[cfg(windows)]
    pub fn from_local_service(service: &crate::config::OpenAIConfig) -> Self {
        // Transcribing a full local recording can outlast a short API request.
        let mut engine = Self::with_timeout(
            &service.api_key,
            &service.model,
            &service.base_url,
            Duration::from_secs(3600),
        )
        .with_vocabulary_prompt(service.use_vocabulary_prompt);
        engine.name = service.model.clone();
        engine.local = true;
        engine
    }

    /// Create a new OpenAI engine instance
    ///
    /// # Arguments
    /// * `api_key` - OpenAI API key
    /// * `model` - Model name (e.g., "whisper-1", "gpt-4o-transcribe")
    /// * `base_url` - Base URL for API requests
    pub fn new(api_key: &str, model: &str, base_url: &str) -> Self {
        Self::with_timeout(api_key, model, base_url, Duration::from_secs(60))
    }

    pub(crate) fn with_timeout(
        api_key: &str,
        model: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Self {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout);
        if reqwest::Url::parse(base_url).is_ok_and(|url| loopback(&url)) {
            builder = builder.no_proxy();
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());

        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            use_vocabulary_prompt: false,
            client,
            local: false,
            framework: None,
            name: "OpenAI Whisper".into(),
            resolved: Default::default(),
        }
    }

    pub fn with_vocabulary_prompt(mut self, enabled: bool) -> Self {
        self.use_vocabulary_prompt = enabled;
        self
    }

    pub fn check_model(&self) -> Result<(), String> {
        self.check_model_for(None)
    }

    pub fn catalog_id(&self) -> Option<&str> {
        self.resolved.get().map(|m| m.wire_id.as_str())
    }

    pub(crate) fn check_model_for(&self, owner: Option<u32>) -> Result<(), String> {
        let resolved = RUNTIME.block_on(self.catalog_model(owner))?;
        let _ = self.resolved.set(resolved);
        Ok(())
    }

    async fn catalog_model(&self, owner: Option<u32>) -> Result<ResolvedModel, String> {
        #[derive(Deserialize)]
        struct ModelList {
            data: Vec<CatalogModel>,
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .map_err(|_| "无法初始化服务检查。")?;
        let response = self
            .authenticate(client.get(format!("{}/models", self.base_url)))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    "连接超时，请检查地址和网络。"
                } else {
                    "无法连接，请检查地址、网络和密钥格式。"
                }
            })?;
        let status = response.status();
        match status.as_u16() {
            401 | 403 => return Err("服务拒绝访问，请检查 API 密钥和权限。".into()),
            404 => return Err("找不到模型列表，请检查服务地址是否包含 API 前缀。".into()),
            _ if !status.is_success() => return Err(format!("服务返回 HTTP {status}。")),
            _ => {}
        }
        let body = response.text().await.map_err(|_| "无法读取模型列表。")?;
        let models: ModelList =
            serde_json::from_str(&body).map_err(|_| "服务未返回兼容的模型列表。")?;
        self.match_catalog(&models.data, owner)
    }

    fn match_catalog(
        &self,
        models: &[CatalogModel],
        owner: Option<u32>,
    ) -> Result<ResolvedModel, String> {
        let mut matching = models.iter().filter(|m| {
            !m.id.trim().is_empty()
                && owner.is_none_or(|pid| m.speaky_owner == Some(pid))
                && self
                    .framework
                    .is_none_or(|f| m.framework.as_deref() == Some(f.id))
        });
        // Early desktop bridges exposed the framework ID as the only model ID.
        let legacy = self
            .framework
            .filter(|f| f.is_builtin_endpoint(&self.base_url) && self.model == f.preset_name());
        let entry = matching
            .clone()
            .find(|m| m.id == self.model)
            .or_else(|| {
                let mut aliases = matching
                    .clone()
                    .filter(|m| self.framework.is_some() && m.aliases.contains(&self.model));
                let first = aliases.next()?;
                aliases.next().is_none().then_some(first)
            })
            .or_else(|| {
                matching.find(|m| legacy.is_some_and(|f| m.id == f.id) && m.aliases.is_empty())
            })
            .ok_or_else(|| format!("服务模型列表未匹配所选模型：{}。", self.model))?;
        let name = if legacy.is_some_and(|f| f.id == entry.id) {
            self.model.clone()
        } else {
            self.framework
                .map_or_else(|| entry.id.clone(), |f| f.display_model(&entry.id))
        };
        Ok(ResolvedModel {
            wire_id: entry.id.clone(),
            name,
        })
    }

    /// Async transcription implementation
    async fn transcribe_async(
        &self,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
        pass: TranscriptionPass,
    ) -> EngineResult {
        if audio_data.is_empty() {
            return Err(EngineError::AudioProcessingError(
                "Empty audio data provided".to_string(),
            ));
        }

        debug!(
            "Starting OpenAI transcription, model={}, language={}, size={} bytes",
            self.model,
            language,
            audio_data.len()
        );

        let mut wire_model = self.catalog_id().unwrap_or(&self.model);
        let mut response = self
            .submit(wire_model, audio_data, language, vocabulary, pass)
            .await?;
        // A running bridge from an earlier release accepts only its old ID.
        if self.framework.is_some() && response.status() == 404 && self.resolved.get().is_none() {
            if let Ok(verified) = self.catalog_model(None).await {
                if verified.wire_id != wire_model {
                    let _ = self.resolved.set(verified);
                    wire_model = self.catalog_id().unwrap_or(&self.model);
                    response = self
                        .submit(wire_model, audio_data, language, vocabulary, pass)
                        .await?;
                }
            }
        }
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            error!("OpenAI API error: {} - {}", status, text);

            return Err(if status == 429 {
                EngineError::RateLimited("Rate limit exceeded".to_string())
            } else {
                EngineError::NetworkError(format!("API error {}: {}", status, text))
            });
        }

        let is_json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
        let text = response
            .text()
            .await
            .map_err(|e| EngineError::InvalidResponse(format!("Failed to read response: {}", e)))?;

        let result = match self.framework {
            Some(framework) => match decode_framework_transcript(&text, framework.id, wire_model) {
                Ok(result) => result,
                Err(error) if self.resolved.get().is_none() => {
                    // A bridge may accept a configured alias and reply with the weight name.
                    // Accept this change only when its catalog ties both identifiers together.
                    let Ok(verified) = self.catalog_model(None).await else {
                        return Err(error);
                    };
                    let result =
                        decode_framework_transcript(&text, framework.id, &verified.wire_id)?;
                    let _ = self.resolved.set(verified);
                    result
                }
                Err(error) => return Err(error),
            },
            None => decode_transcript(&text, is_json)?,
        };
        debug!("Transcription complete: {} chars", result.len());
        Ok(result)
    }

    async fn submit(
        &self,
        wire_model: &str,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
        pass: TranscriptionPass,
    ) -> Result<reqwest::Response, EngineError> {
        let url = format!("{}/audio/transcriptions", self.base_url);

        // Create multipart form with audio file
        let part = multipart::Part::bytes(audio_data.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e: reqwest::Error| {
                EngineError::AudioProcessingError(format!("Failed to create multipart form: {}", e))
            })?;

        let mut form = multipart::Form::new()
            .part("file", part)
            .text("model", wire_model.to_owned())
            .text(
                "response_format",
                if self.framework.is_some() {
                    "json"
                } else {
                    "text"
                },
            );

        if self.use_vocabulary_prompt {
            let prompt = vocabulary_prompt(vocabulary);
            if !prompt.is_empty() {
                form = form.text("prompt", prompt);
            }
        }

        if self.local && pass == TranscriptionPass::Preview {
            form = form.text("preview", "true");
        }

        // Omit the language field for automatic detection. BCP 47 values from
        // older configs are converted at this provider boundary when present.
        if self.framework.is_some() {
            form = form.text(
                "language",
                if language.is_empty() {
                    "auto"
                } else {
                    language
                }
                .to_owned(),
            );
        } else if !language.is_empty() && language != "auto" {
            form = form.text(
                "language",
                language
                    .split(['-', '_'])
                    .next()
                    .unwrap_or("zh")
                    .to_string(),
            );
        }

        self.authenticate(self.client.post(&url))
            .multipart(form)
            .send()
            .await
            .map_err(|e| EngineError::NetworkError(format!("Request failed: {}", e)))
    }
}

pub fn loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn decode_framework_transcript(body: &str, framework: &str, model: &str) -> EngineResult {
    #[derive(Deserialize)]
    struct Response {
        text: String,
        framework: String,
        model: String,
    }
    let response: Response = serde_json::from_str(body).map_err(|_| {
        EngineError::InvalidResponse(format!(
            "{} / {} 的响应必须包含 text、framework、model。",
            framework, model
        ))
    })?;
    if response.framework != framework || response.model != model {
        return Err(EngineError::InvalidResponse(format!(
            "请求 {} / {}，服务实际返回 {} / {}。",
            framework, model, response.framework, response.model
        )));
    }
    Ok(response.text.trim().to_owned())
}

fn vocabulary_prompt(vocabulary: &[&str]) -> String {
    const MAX_PROMPT_BYTES: usize = 224;
    let mut prompt = String::new();
    for term in vocabulary {
        let term = term.trim();
        let separator = if prompt.is_empty() { "" } else { ", " };
        if term.is_empty() || prompt.len() + separator.len() + term.len() > MAX_PROMPT_BYTES {
            continue;
        }
        prompt.push_str(separator);
        prompt.push_str(term);
    }
    prompt
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_local_profiles_migrate_offline_without_changing_custom_models() {
        use crate::config::{Config, Framework, FrameworkProfile};
        let mut config = Config::default();
        for framework in Framework::ALL {
            let default = framework.service(&config);
            assert_ne!(default.model, framework.id);
            config.engine.frameworks.insert(
                framework.id.into(),
                FrameworkProfile {
                    model: Some(framework.id.into()),
                    base_url: Some(default.base_url),
                    ..Default::default()
                },
            );
            assert_eq!(framework.service(&config).model, framework.preset_name());
            config
                .engine
                .frameworks
                .get_mut(framework.id)
                .unwrap()
                .model = Some("custom/model".into());
            assert_eq!(framework.service(&config).model, "custom/model");
        }
        assert_eq!(Framework::ALL[0].preset_name(), "SenseVoiceSmall");
    }

    #[test]
    fn legacy_local_ids_and_declared_remote_aliases_keep_verified_model_identity() {
        use super::{CatalogModel, OpenAIEngine};
        use crate::config::{Config, Framework, FrameworkProfile};
        let f = Framework::ALL[0];
        let mut config = Config::default();
        let engine = OpenAIEngine::from_framework(f, &config).unwrap();
        let old: CatalogModel =
            serde_json::from_str(r#"{"id":"funasr","framework":"funasr"}"#).unwrap();
        let resolved = engine.match_catalog(&[old], None).unwrap();
        assert_eq!(resolved.wire_id, "funasr");
        assert_eq!(resolved.name, "SenseVoiceSmall");
        config.engine.frameworks.insert(
            f.id.into(),
            FrameworkProfile {
                model: Some("funasr".into()),
                base_url: Some("https://example.org/v1".into()),
                api_key: "key".into(),
            },
        );
        assert_eq!(f.service(&config).model, "funasr");
        let remote = OpenAIEngine::from_framework(f, &config).unwrap();
        let records = |s: &str| serde_json::from_str::<Vec<CatalogModel>>(s).unwrap();
        let good = records(r#"[{"id":"site/weights","framework":"funasr","aliases":["funasr"]}]"#);
        assert_eq!(
            remote.match_catalog(&good, None).unwrap().name,
            "site/weights"
        );
        let bad = records(r#"[{"id":"site/weights","framework":"vosk","aliases":["funasr"]}]"#);
        assert!(remote.match_catalog(&bad, None).is_err());
        let undeclared = records(r#"[{"id":"site/weights","framework":"funasr"}]"#);
        assert!(remote.match_catalog(&undeclared, None).is_err());
    }
    use super::{decode_transcript, vocabulary_prompt};

    #[test]
    fn decodes_compatible_transcription_responses() {
        assert_eq!(decode_transcript("  中文\n", false).unwrap(), "中文");
        assert_eq!(decode_transcript(r#""中文""#, true).unwrap(), "中文");
        assert_eq!(
            decode_transcript(r#"{"text":"中文"}"#, true).unwrap(),
            "中文"
        );
        assert_eq!(decode_transcript(r#""中文""#, false).unwrap(), r#""中文""#);
        assert!(decode_transcript(r#"{"error":"invalid"}"#, true).is_err());
    }

    #[test]
    fn builds_a_bounded_prompt_without_splitting_terms() {
        let oversized_term = "词".repeat(75);
        assert_eq!(
            vocabulary_prompt(&[" Speaky ", &oversized_term, "GPUI", ""]),
            "Speaky, GPUI"
        );
    }

    #[test]
    fn framework_transcripts_require_exact_framework_and_model() {
        let decode = |body| super::decode_framework_transcript(body, "kaldi", "mandarin");
        assert_eq!(
            decode(r#"{"framework":"kaldi","model":"mandarin","text":" 你好 "}"#).unwrap(),
            "你好"
        );
        for body in [
            r#"{"framework":"vosk","model":"mandarin","text":"你好"}"#,
            r#"{"framework":"kaldi","model":"other","text":"你好"}"#,
            r#"{"text":"你好"}"#,
            "你好",
        ] {
            assert!(decode(body).is_err());
        }
    }

    #[test]
    fn openai_engine_availability_allows_unauthenticated_loopback_with_model() {
        use super::{Engine, OpenAIEngine};

        assert!(OpenAIEngine::new("", "whisper-1", "http://127.0.0.1:8080/v1").is_available());
        assert!(OpenAIEngine::new("", "whisper-1", "http://localhost:8080/v1").is_available());
        assert!(!OpenAIEngine::new("", "whisper-1", "https://api.openai.com/v1").is_available());
        assert!(
            OpenAIEngine::new("sk-test", "whisper-1", "https://api.openai.com/v1").is_available()
        );
        assert!(!OpenAIEngine::new("", "   ", "http://127.0.0.1:8080/v1").is_available());
        assert!(
            !OpenAIEngine::new("sk-test", "   ", "https://api.openai.com/v1").is_available()
        );
    }
}

impl Engine for OpenAIEngine {
    fn name(&self) -> &str {
        self.resolved.get().map_or(self.name.as_str(), |m| &m.name)
    }

    fn is_available(&self) -> bool {
        !self.model.trim().is_empty()
            && (self.framework.is_some()
                || !self.api_key.is_empty()
                || reqwest::Url::parse(&self.base_url).is_ok_and(|url| loopback(&url)))
    }

    fn transcribe(&self, audio_data: &[u8], language: &str) -> EngineResult {
        self.transcribe_with_vocabulary(audio_data, language, &[], TranscriptionPass::Final)
    }

    fn transcribe_with_vocabulary(
        &self,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
        pass: TranscriptionPass,
    ) -> EngineResult {
        RUNTIME.block_on(self.transcribe_async(audio_data, language, vocabulary, pass))
    }

    fn supports_streaming(&self) -> bool {
        false
    }
}

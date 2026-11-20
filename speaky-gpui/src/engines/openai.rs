use super::{Engine, EngineError, EngineResult};
use log::{debug, error};
use once_cell::sync::Lazy;
use reqwest::multipart;
use serde::Deserialize;
use tokio::runtime::Runtime;

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
static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
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
}

impl OpenAIEngine {
    /// Create a new OpenAI engine instance
    ///
    /// # Arguments
    /// * `api_key` - OpenAI API key
    /// * `model` - Model name (e.g., "whisper-1", "gpt-4o-transcribe")
    /// * `base_url` - Base URL for API requests
    pub fn new(api_key: &str, model: &str, base_url: &str) -> Self {
        // Reusable HTTP client with timeout
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            use_vocabulary_prompt: false,
            client,
        }
    }

    pub fn with_vocabulary_prompt(mut self, enabled: bool) -> Self {
        self.use_vocabulary_prompt = enabled;
        self
    }

    #[cfg(target_os = "macos")]
    pub fn check_model(&self) -> Result<(), String> {
        #[derive(Deserialize)]
        struct ModelList {
            data: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            id: String,
        }

        RUNTIME.block_on(async {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(8))
                .build()
                .map_err(|_| "无法初始化服务检查。")?;
            let response = client
                .get(format!("{}/models", self.base_url))
                .bearer_auth(&self.api_key)
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
            if models.data.iter().any(|model| model.id == self.model) {
                Ok(())
            } else {
                Err("服务已连接，但模型列表里没有此模型 ID。".into())
            }
        })
    }

    /// Async transcription implementation
    async fn transcribe_async(
        &self,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
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
            .text("model", self.model.clone())
            .text("response_format", "text");

        if self.use_vocabulary_prompt {
            let prompt = vocabulary_prompt(vocabulary);
            if !prompt.is_empty() {
                form = form.text("prompt", prompt);
            }
        }

        // Omit the language field for automatic detection. BCP 47 values from
        // older configs are converted at this provider boundary when present.
        if !language.is_empty() && language != "auto" {
            form = form.text(
                "language",
                language
                    .split(['-', '_'])
                    .next()
                    .unwrap_or("zh")
                    .to_string(),
            );
        }

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| EngineError::NetworkError(format!("Request failed: {}", e)))?;

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

        let result = decode_transcript(&text, is_json)?;
        debug!("Transcription complete: {} chars", result.len());
        Ok(result)
    }
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
}

impl Engine for OpenAIEngine {
    fn name(&self) -> &str {
        "OpenAI Whisper"
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn transcribe(&self, audio_data: &[u8], language: &str) -> EngineResult {
        self.transcribe_with_vocabulary(audio_data, language, &[])
    }

    fn transcribe_with_vocabulary(
        &self,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
    ) -> EngineResult {
        RUNTIME.block_on(self.transcribe_async(audio_data, language, vocabulary))
    }

    fn supports_streaming(&self) -> bool {
        false
    }
}

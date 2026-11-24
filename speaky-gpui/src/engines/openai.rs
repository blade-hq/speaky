use super::{Engine, EngineError, EngineResult, TranscriptionPass};
use log::debug;
use once_cell::sync::Lazy;
use reqwest::multipart;
use serde::Deserialize;
use std::time::Duration;
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
pub(crate) static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("speaky-http")
        .worker_threads(2)
        .build()
        .expect("Failed to create Tokio runtime")
});

/// Client for the OpenAI-compatible transcription API.
#[derive(Debug, Clone)]
pub struct OpenAIEngine {
    api_key: String,
    model: String,
    endpoint: String,
    use_vocabulary_prompt: bool,
    client: reqwest::Client,
}

impl OpenAIEngine {
    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            request
        } else {
            request.bearer_auth(&self.api_key)
        }
    }

    pub fn from_service(service: &crate::config::OpenAIConfig) -> Self {
        Self::new(&service.api_key, &service.model, &service.base_url)
            .with_vocabulary_prompt(service.use_vocabulary_prompt)
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
        if reqwest::Url::parse(base_url).is_ok_and(|url| is_loopback(&url)) {
            builder = builder.no_proxy();
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());

        Self::at_endpoint(
            api_key,
            model,
            format!("{}/audio/transcriptions", base_url.trim_end_matches('/')),
            client,
        )
    }

    pub(super) fn at_endpoint(
        api_key: &str,
        model: &str,
        endpoint: String,
        client: reqwest::Client,
    ) -> Self {
        Self {
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            endpoint,
            use_vocabulary_prompt: false,
            client,
        }
    }

    pub fn with_vocabulary_prompt(mut self, enabled: bool) -> Self {
        self.use_vocabulary_prompt = enabled;
        self
    }

    /// Async transcription implementation
    pub(super) async fn transcribe_async(
        &self,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
        request_json: bool,
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

        let mut response = self
            .submit(audio_data, language, vocabulary, request_json)
            .await?;
        if !response.status().is_success() {
            return Err(super::http_status_error(response.status().as_u16()));
        }

        let is_json = request_json
            || response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| EngineError::InvalidResponse(e.to_string()))?
        {
            if body.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(EngineError::InvalidResponse("转写响应过大。".into()));
            }
            body.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&body);
        let result = decode_transcript(&text, is_json)?;
        debug!("Transcription complete: {} chars", result.len());
        Ok(result)
    }

    async fn submit(
        &self,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
        request_json: bool,
    ) -> Result<reqwest::Response, EngineError> {
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
            .text(
                "response_format",
                if request_json { "json" } else { "text" },
            );

        if self.use_vocabulary_prompt {
            let prompt = vocabulary_prompt(vocabulary);
            if !prompt.is_empty() {
                form = form.text("prompt", prompt);
            }
        }

        let language = language.split(['-', '_']).next().unwrap_or_default().trim();
        if !language.is_empty() && language != "auto" {
            form = form.text("language", language.to_owned());
        }

        self.authenticate(self.client.post(&self.endpoint))
            .multipart(form)
            .send()
            .await
            .map_err(|e| EngineError::NetworkError(format!("Request failed: {}", e)))
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
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
        &self.model
    }

    fn is_available(&self) -> bool {
        !self.model.trim().is_empty()
            && (!self.api_key.is_empty()
                || reqwest::Url::parse(&self.endpoint).is_ok_and(|url| is_loopback(&url)))
    }

    fn transcribe(&self, audio_data: &[u8], language: &str) -> EngineResult {
        self.transcribe_with_vocabulary(audio_data, language, &[], TranscriptionPass::Final)
    }

    fn transcribe_with_vocabulary(
        &self,
        audio_data: &[u8],
        language: &str,
        vocabulary: &[&str],
        _pass: TranscriptionPass,
    ) -> EngineResult {
        RUNTIME.block_on(self.transcribe_async(audio_data, language, vocabulary, false))
    }

    fn supports_streaming(&self) -> bool {
        false
    }
}

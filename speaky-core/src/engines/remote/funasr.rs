use super::{http_status_error, EngineError, EngineResult};
use futures_util::{SinkExt, StreamExt};
use reqwest::{header::HeaderValue, Url};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        error::{ProtocolError, SubProtocolError},
        http::{header, Request},
        Error, Message,
    },
};

/// Extract mono 16-bit PCM and its actual sample rate, skipping ancillary
/// chunks (including the FLLR chunk in the bundled microphone sample).
fn wav_pcm(wav: &[u8]) -> Option<(u32, &[u8])> {
    if wav.get(..4)? != b"RIFF" || wav.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut chunks = wav.get(12..)?;
    let mut rate = None;
    let mut data = None;
    while chunks.len() >= 8 {
        let size = u32::from_le_bytes(chunks[4..8].try_into().ok()?) as usize;
        let chunk = chunks.get(8..)?.get(..size)?;
        match &chunks[..4] {
            b"fmt " => {
                if chunk.get(..4)? != [1, 0, 1, 0] || chunk.get(14..16)? != [16, 0] {
                    return None;
                }
                rate = Some(u32::from_le_bytes(chunk.get(4..8)?.try_into().ok()?));
            }
            b"data" => data = Some(chunk),
            _ => {}
        }
        if let (Some(rate @ 8000..=192000), Some(data)) = (rate, data) {
            return (!data.is_empty() && data.len() % 2 == 0).then_some((rate, data));
        }
        chunks = chunks.get(8..)?.get(size.checked_add(size % 2)?..)?;
    }
    None
}

fn handshake(
    endpoint: &Url,
    authorization: Option<&HeaderValue>,
    binary: bool,
) -> Result<Request<()>, EngineError> {
    let mut request = endpoint.as_str().into_client_request().map_err(ws_error)?;
    if let Some(value) = authorization {
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, value.clone());
    }
    if binary {
        request.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("binary"),
        );
    }
    Ok(request)
}

fn ws_error(error: Error) -> EngineError {
    match error {
        Error::Http(response) => http_status_error(response.status().as_u16()),
        Error::Tls(_) => {
            EngineError::NetworkError("WebSocket TLS 连接失败，请检查证书和地址。".into())
        }
        _ => EngineError::NetworkError("WebSocket 连接失败或意外中断。".into()),
    }
}

#[derive(Deserialize)]
struct OfflineResult {
    mode: String,
    text: Option<String>,
    wav_name: Option<String>,
    error: Option<serde_json::Value>,
    #[serde(default)]
    is_end: bool,
    is_final: Option<bool>,
    // Python's segmented runtime includes this field (also when null) on
    // every utterance and follows them with a separate end acknowledgement.
    #[serde(default, rename = "spk_name", deserialize_with = "present")]
    speaker_segment: bool,
}

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer).map(|_| true)
}

pub(super) async fn transcribe(
    endpoint: &Url,
    authorization: Option<&HeaderValue>,
    wav: &[u8],
    language: &str,
) -> EngineResult {
    let (rate, pcm) = wav_pcm(wav).ok_or_else(|| {
        EngineError::AudioProcessingError("FunASR 需要单声道 16 位 PCM WAV 录音。".into())
    })?;
    let connect = async {
        let result = match connect_async(handshake(endpoint, authorization, true)?).await {
            // The Python service negotiates "binary". The native C++ server may
            // simply accept the upgrade without selecting a subprotocol.
            Err(Error::Protocol(ProtocolError::SecWebSocketSubProtocolError(
                SubProtocolError::NoSubProtocol,
            ))) => connect_async(handshake(endpoint, authorization, false)?).await,
            other => other,
        };
        result.map_err(ws_error)
    };
    let (mut socket, upgrade) = tokio::time::timeout(Duration::from_secs(5), connect)
        .await
        .map_err(|_| EngineError::NetworkError("WebSocket 握手超时。".into()))??;
    let binary_protocol = upgrade
        .headers()
        .contains_key(header::SEC_WEBSOCKET_PROTOCOL);
    let start = json!({
        "mode": "offline", "wav_name": "speaky", "wav_format": "pcm",
        "is_speaking": true, "audio_fs": rate, "chunk_size": [5,10,5],
        "chunk_interval": 10, "itn": true,
        "svs_lang": language.split(['-', '_']).next().filter(|s| !s.is_empty()).unwrap_or("auto")
    });
    socket
        .send(Message::Text(start.to_string()))
        .await
        .map_err(ws_error)?;
    // Send 60 ms of audio per message, as the native FunASR client does.
    for chunk in pcm.chunks(2 * (rate as usize * 60 / 1000)) {
        socket
            .send(Message::Binary(chunk.to_vec()))
            .await
            .map_err(ws_error)?;
    }
    // The Python server can emit several offline VAD segments for a file.
    // Mark the final control frame so pre-final segments can be accumulated.
    let end = json!({"is_speaking":false, "is_end":true, "wav_name":"speaky-end"});
    socket
        .send(Message::Text(end.to_string()))
        .await
        .map_err(ws_error)?;
    let mut text = String::new();
    let mut received_segment = false;
    while let Some(message) = socket.next().await {
        match message.map_err(ws_error)? {
            Message::Text(body) => {
                let result: OfflineResult = serde_json::from_str(&body).map_err(|_| {
                    EngineError::InvalidResponse("服务未返回 FunASR 转写结果。".into())
                })?;
                if result.mode != "offline" || result.error.is_some() {
                    return Err(EngineError::InvalidResponse(
                        "FunASR 未完成离线转写，请检查服务配置。".into(),
                    ));
                }
                if let Some(part) = result.text {
                    if text.len() + part.len() > 16 * 1024 * 1024 {
                        return Err(EngineError::InvalidResponse("FunASR 转写响应过大。".into()));
                    }
                    text.push_str(&part);
                    received_segment = true;
                } else if !result.is_end {
                    return Err(EngineError::InvalidResponse(
                        "FunASR 转写结果缺少 text。".into(),
                    ));
                }
                if result.is_end {
                    if result.is_final == Some(false) {
                        return Err(EngineError::InvalidResponse("FunASR 未能完成转写。".into()));
                    }
                    break;
                }
                // The C++ offline server also uses is_final:false for its
                // single completed response. The end-of-input name identifies it.
                if !result.speaker_segment
                    && (result.is_final == Some(true)
                        || !binary_protocol
                        || result.wav_name.as_deref() != Some("speaky"))
                {
                    break;
                }
            }
            Message::Ping(_) => socket.flush().await.map_err(ws_error)?,
            Message::Close(_) if received_segment => break,
            Message::Pong(_) => {}
            _ => {
                return Err(EngineError::InvalidResponse(
                    "FunASR 未返回有效文本。".into(),
                ))
            }
        }
    }
    if !received_segment {
        return Err(EngineError::InvalidResponse(
            "FunASR 未返回转写结果。".into(),
        ));
    }
    let _ = socket.close(None).await;
    Ok(text.trim().to_owned())
}

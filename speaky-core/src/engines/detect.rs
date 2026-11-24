use super::{openai::RUNTIME, Protocol, RemoteEngine};
use crate::config::RemoteService;
use reqwest::Url;
use serde::Deserialize;
use std::time::{Duration, Instant};

use crate::session::SAMPLE_AUDIO as SAMPLE;
const CHECK_TIMEOUT: Duration = Duration::from_secs(90);

fn candidates(address: &str) -> Result<Vec<(Protocol, Url)>, String> {
    let url =
        Url::parse(address.trim()).map_err(|_| "请输入包含 http(s):// 或 ws(s):// 的服务地址。")?;
    if !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("请填写服务地址，将凭据单独填在 API 密钥栏。".into());
    }
    let ws_scheme = match url.scheme() {
        "ws" | "wss" => return Ok(vec![(Protocol::FunAsr, url)]),
        "http" => "ws",
        "https" => "wss",
        _ => return Err("服务地址支持 HTTP、HTTPS、WS 和 WSS。".into()),
    };
    let prefix = url.path().trim_end_matches('/');
    let with_path = |path: &str| {
        let mut endpoint = url.clone();
        endpoint.set_path(path);
        endpoint
    };
    if prefix.ends_with("/audio/transcriptions") {
        return Ok(vec![(Protocol::OpenAI, with_path(prefix))]);
    }
    if prefix.ends_with("/inference") {
        return Ok(vec![(Protocol::WhisperCpp, with_path(prefix))]);
    }
    let mut routes = Vec::new();
    if !prefix.ends_with("/v1") {
        routes.push((
            Protocol::OpenAI,
            with_path(&format!("{prefix}/v1/audio/transcriptions")),
        ));
    }
    routes.push((
        Protocol::OpenAI,
        with_path(&format!("{prefix}/audio/transcriptions")),
    ));
    routes.push((
        Protocol::WhisperCpp,
        with_path(&format!("{prefix}/inference")),
    ));
    let mut websocket = url;
    websocket
        .set_scheme(ws_scheme)
        .map_err(|_| "WebSocket 地址无效。")?;
    routes.push((Protocol::FunAsr, websocket));
    Ok(routes)
}

// A catalog can supply a missing model ID. Only transcribing the sample establishes readiness.
fn single_model(endpoint: &Url, api_key: &str, remaining: Duration) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Model {
        id: String,
    }
    #[derive(Deserialize)]
    struct Catalog {
        data: Vec<Model>,
    }
    let mut catalog_url = endpoint.clone();
    let base = endpoint
        .path()
        .strip_suffix("/audio/transcriptions")
        .ok_or("转写地址缺少路径。")?;
    catalog_url.set_path(&format!("{base}/models"));
    RUNTIME.block_on(async {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(remaining.min(Duration::from_secs(4)))
            .build()
            .map_err(|e| e.to_string())?;
        let mut request = client.get(catalog_url);
        if !api_key.is_empty() {
            request = request.bearer_auth(api_key);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| "无法读取模型列表，可手动填写模型 ID。")?;
        if !response.status().is_success() {
            return Err(format!(
                "模型列表返回 HTTP {}，请核对地址、密钥或手动填写模型 ID。",
                response.status().as_u16()
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "读取模型列表失败。")? {
            if body.len() + chunk.len() > 1024 * 1024 {
                return Err("请手动填写模型 ID。".into());
            }
            body.extend_from_slice(&chunk);
        }
        let catalog: Catalog =
            serde_json::from_slice(&body).map_err(|_| "无法识别模型列表，请填写模型 ID。")?;
        match catalog.data.as_slice() {
            [model] if !model.id.trim().is_empty() => Ok(model.id.trim().into()),
            _ => Err("未找到唯一模型，请填写要使用的转写模型 ID。".into()),
        }
    })
}

pub fn detect_service(
    address: &str,
    model: &str,
    api_key: &str,
) -> Result<(RemoteService, String), String> {
    let address = address.trim();
    let api_key = api_key.trim();
    let mut failures = Vec::new();
    let started = Instant::now();
    for (protocol, endpoint) in candidates(address)? {
        let remaining = CHECK_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            failures.push("自动检测超时，请确认模型已就绪后重试。".into());
            break;
        }
        let result: Result<_, String> = (|| {
            let model = match protocol {
                Protocol::OpenAI if model.trim().is_empty() => {
                    single_model(&endpoint, api_key, remaining)?
                }
                Protocol::OpenAI => model.trim().to_owned(),
                _ => String::new(), // Native services select the loaded model server-side.
            };
            let service = RemoteService {
                address: address.into(),
                endpoint: endpoint.to_string(),
                protocol,
                model,
                api_key: api_key.into(),
            };
            let text = test_sample(&service, CHECK_TIMEOUT.saturating_sub(started.elapsed()))?;
            Ok((service, text))
        })();
        match result {
            Ok(verified) => return Ok(verified),
            Err(error) => failures.push(format!(
                "{} {}：{}",
                protocol.label(),
                endpoint.path(),
                error
            )),
        }
    }
    Err(format!("服务未通过试转写。{}", failures.join("；")))
}

pub fn check_service(service: &RemoteService) -> Result<String, String> {
    test_sample(service, CHECK_TIMEOUT)
}

fn test_sample(service: &RemoteService, timeout: Duration) -> Result<String, String> {
    let engine = RemoteEngine::new(
        service.protocol,
        &service.endpoint,
        &service.model,
        &service.api_key,
    )?;
    let text = engine.probe(SAMPLE, timeout).map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Err("内置语音样本未返回文字。".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    fn http_fixture(
        replies: Vec<(&'static str, &'static str)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (route, reply) in replies {
                let (mut connection, _) = listener.accept().unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut reader = BufReader::new(&mut connection);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                assert_eq!(first.trim(), format!("{route} HTTP/1.1"));
                let mut length = 0;
                let mut authorized = false;
                for header in reader.by_ref().lines() {
                    let line = header.unwrap().to_ascii_lowercase();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    authorized |= line == "authorization: bearer test-token";
                }
                assert!(authorized);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if route.starts_with("POST ") {
                    assert!(body.windows(12).any(|s| s == &SAMPLE[..12]));
                }
                write!(connection, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
            }
        });
        (address, server)
    }

    #[test]
    fn discovers_one_model_and_requires_real_transcription_to_activate() {
        for (reply, usable) in [
            (r#"{"text":"测试"}"#, true),
            (r#"{"text":" "}"#, false),
            ("<html>OK</html>", false),
        ] {
            let (server, handle) = http_fixture(vec![
                ("GET /v1/models", r#"{"data":[{"id":"asr-model"}]}"#),
                ("POST /v1/audio/transcriptions", reply),
            ]);
            let endpoint = format!("{server}/v1/audio/transcriptions");
            let detected = detect_service(&endpoint, "", "test-token");
            assert_eq!(detected.is_ok(), usable, "{detected:?}");
            if let Ok((service, text)) = detected {
                assert_eq!(service.model, "asr-model");
                assert_eq!(service.protocol, Protocol::OpenAI);
                assert_eq!(service.endpoint, endpoint);
                assert_eq!(text, "测试");
            }
            handle.join().unwrap();
        }
    }

    #[test]
    fn finds_native_inference_after_http_routes_return_unrelated_json() {
        let (server, handle) = http_fixture(vec![
            ("POST /v1/audio/transcriptions", r#"{"status":"ok"}"#),
            ("POST /audio/transcriptions", r#"{"status":"ok"}"#),
            ("POST /inference", r#"{"text":"你好"}"#),
        ]);
        let (service, _) = detect_service(&server, "test-model", "test-token").unwrap();
        assert_eq!(service.protocol, Protocol::WhisperCpp);
        assert_eq!(service.endpoint, format!("{server}/inference"));
        assert!(service.model.is_empty());
        handle.join().unwrap();
    }

    #[test]
    fn exact_paths_and_proxy_prefixes_stay_on_the_supplied_origin() {
        for (address, protocol) in [
            (
                "https://example.org/speech/v1/audio/transcriptions",
                Protocol::OpenAI,
            ),
            ("https://example.org/speech/inference", Protocol::WhisperCpp),
            ("wss://example.org/speech/", Protocol::FunAsr),
        ] {
            let routes = candidates(address).unwrap();
            assert_eq!(routes, vec![(protocol, Url::parse(address).unwrap())]);
        }
        let routes = candidates("https://example.org:8443/speech/").unwrap();
        let paths: Vec<_> = routes.iter().map(|(_, url)| url.as_str()).collect();
        assert_eq!(
            paths,
            [
                "https://example.org:8443/speech/v1/audio/transcriptions",
                "https://example.org:8443/speech/audio/transcriptions",
                "https://example.org:8443/speech/inference",
                "wss://example.org:8443/speech/",
            ]
        );
        assert_eq!(
            candidates("http://localhost/v1/").unwrap()[0].1.path(),
            "/v1/audio/transcriptions"
        );
    }

    #[test]
    fn invalid_addresses_are_rejected_before_network_io() {
        for address in [
            "",
            "file:///test",
            "http://user:pass@localhost",
            "http://localhost/?key=x",
            "http://localhost/#x",
        ] {
            assert!(candidates(address).is_err());
        }
    }
}

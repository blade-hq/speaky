use super::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;
use tokio_tungstenite::tungstenite::{
    handshake::server::{Request, Response},
    Message,
};

const SAMPLE: &[u8] = include_bytes!("../../../assets/settings-sample.wav");

fn http_fixture(
    status: u16,
    body: &'static str,
) -> (String, thread::JoinHandle<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/custom/path", listener.local_addr().unwrap());
    let exchange = thread::spawn(move || {
        let mut stream = RUNTIME.block_on(async {
            let incoming = tokio::net::TcpListener::from_std(listener).unwrap();
            let (stream, _) = tokio::time::timeout(Duration::from_secs(5), incoming.accept())
                .await
                .expect("client did not connect")
                .unwrap();
            stream.into_std().unwrap()
        });
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        let headers = headers.to_ascii_lowercase();
        let length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).unwrap();
        write!(
            stream,
            "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        (headers, payload)
    });
    (url, exchange)
}

#[test]
fn http_adapters_post_wav_with_protocol_fields_and_decode_json() {
    for protocol in [Protocol::OpenAI, Protocol::WhisperCpp] {
        let (endpoint, exchange) = http_fixture(200, r#"{"text":" 已识别 "}"#);
        let engine = RemoteEngine::new(protocol, &endpoint, "org/asr", "service-token").unwrap();
        assert_eq!(
            engine.probe(SAMPLE, Duration::from_secs(5)).unwrap(),
            "已识别"
        );
        let (headers, payload) = exchange.join().unwrap();
        assert!(headers.starts_with("post /custom/path "));
        assert!(headers.contains("authorization: bearer service-token"));
        assert!(payload.windows(SAMPLE.len()).any(|part| part == SAMPLE));
        let form = String::from_utf8_lossy(&payload);
        assert!(form.contains("name=\"file\""));
        assert!(form.contains("name=\"response_format\"\r\n\r\njson"));
        assert!(form.contains("name=\"language\"\r\n\r\nzh"));
        assert_eq!(
            form.contains("name=\"model\"\r\n\r\norg/asr"),
            protocol == Protocol::OpenAI
        );
    }
}

#[test]
fn http_errors_and_html_never_become_transcripts() {
    for (status, body) in [
        (200, "<html>login</html>"),
        (200, r#"{"detail":"oops"}"#),
        (403, "denied"),
    ] {
        let (endpoint, exchange) = http_fixture(status, body);
        let engine = RemoteEngine::new(Protocol::OpenAI, &endpoint, "asr", "").unwrap();
        assert!(engine.probe(SAMPLE, Duration::from_secs(5)).is_err());
        let (headers, _) = exchange.join().unwrap();
        assert!(!headers.contains("authorization:"));
    }
    let (endpoint, exchange) = http_fixture(200, r#"{"text":""}"#);
    assert_eq!(
        RemoteEngine::new(Protocol::WhisperCpp, &endpoint, "", "")
            .unwrap()
            .transcribe(SAMPLE, "auto")
            .unwrap(),
        ""
    );
    exchange.join().unwrap();
}

#[test]
fn funasr_sends_pcm_and_handles_both_final_results_and_segment_acknowledgements() {
    for segmented in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("ws://{}/asr", listener.local_addr().unwrap());
        let task = RUNTIME.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, |req: &Request, mut response: Response| {
                assert_eq!(req.uri().path(), "/asr");
                assert_eq!(req.headers()["authorization"], "Bearer secret");
                assert_eq!(req.headers()["sec-websocket-protocol"], "binary");
                response.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_static("binary"));
                Ok(response)
            }).await.unwrap();
            let start: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(start["mode"], "offline");
            assert_eq!(start["audio_fs"], 16000);
            assert_eq!(start["wav_format"], "pcm");
            let mut audio = Vec::new();
            let end = loop {
                match ws.next().await.unwrap().unwrap() {
                    Message::Binary(bytes) => audio.extend_from_slice(&bytes),
                    Message::Text(text) => break serde_json::from_str::<Value>(&text).unwrap(),
                    message => panic!("unexpected frame {message:?}"),
                }
            };
            assert_eq!(end["is_speaking"], false);
            let data = SAMPLE.windows(4).position(|chunk| chunk == b"data").unwrap() + 8;
            assert_eq!(audio, &SAMPLE[data..data+audio.len()]);
            assert!(audio.len() > 16000);
            let replies = if segmented {
                vec![json!({"mode":"offline","wav_name":start["wav_name"],"text":"测","spk_name":null,"is_final":true}),
                     json!({"mode":"offline","wav_name":end["wav_name"],"text":"试","spk_name":null,"is_final":true}),
                     json!({"mode":"offline","wav_name":end["wav_name"],"is_end":true,"is_final":true})]
            } else {
                vec![json!({"mode":"offline","wav_name":start["wav_name"],"text":"测试","is_final":true})]
            };
            for reply in replies {
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
            // Keep the socket open: a one-result server expects the client to finish.
            assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
        });
        let engine = RemoteEngine::new(Protocol::FunAsr, &endpoint, "", "secret").unwrap();
        assert_eq!(
            engine.probe(SAMPLE, Duration::from_secs(5)).unwrap(),
            "测试"
        );
        RUNTIME.block_on(task).unwrap();
    }
}

#[test]
fn funasr_native_handshake_retries_without_subprotocol_and_accepts_completed_offline_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("ws://{}/", listener.local_addr().unwrap());
    let task = RUNTIME.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        for offered_binary in [true, false] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, response: Response| {
                assert_eq!(req.headers().contains_key("sec-websocket-protocol"), offered_binary);
                // websocketpp upgrades without selecting the offered subprotocol.
                Ok(response)
            }).await.unwrap();
            if offered_binary {
                continue;
            }
            let start: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(start["mode"], "offline");
            let mut bytes = 0;
            loop {
                match ws.next().await.unwrap().unwrap() {
                    Message::Binary(chunk) => bytes += chunk.len(),
                    Message::Text(end) => {
                        assert_eq!(serde_json::from_str::<Value>(&end).unwrap()["is_speaking"], false);
                        break;
                    }
                    unexpected => panic!("unexpected frame {unexpected:?}"),
                }
            }
            assert!(bytes > 0);
            let reply = json!({"mode":"offline","text":"原生识别","wav_name":start["wav_name"],"is_final":false});
            ws.send(Message::Text(reply.to_string())).await.unwrap();
            assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
        }
    });
    let engine = RemoteEngine::new(Protocol::FunAsr, &endpoint, "", "").unwrap();
    assert_eq!(
        engine.probe(SAMPLE, Duration::from_secs(5)).unwrap(),
        "原生识别"
    );
    RUNTIME.block_on(task).unwrap();
}

//! The final ASR -> durable original -> optional rewrite sequence shared by both clients.
use crate::{config::Config, drafts, engines, history, polish, session, terms};
use std::path::Path;

pub const NO_RECOGNIZED_TEXT: &str = "未识别到文字。";

pub struct Transcript {
    pub text: String,
    pub notice: String,
}

pub fn transcribe_saved(config: &Config, path: &Path) -> Result<Transcript, String> {
    let source = transcribe_original(config, path)?;
    Ok(rewrite_saved(config, path, source))
}

/// Decode the complete recording and save its original before any optional rewrite.
pub fn transcribe_original(config: &Config, path: &Path) -> Result<String, String> {
    let wav = std::fs::read(path).map_err(|e| e.to_string())?;
    if session::is_silent_recording(&wav) {
        return Err(session::SILENT_INPUT.into());
    }
    let engine = engines::recognition_engine(config)?;
    let source = terms::transcribe(
        engine.as_ref(),
        &wav,
        &config.core.asr.language,
        engines::TranscriptionPass::Final,
    )
    .map_err(|e| e.to_string())?;
    if source.trim().is_empty() {
        return Err(NO_RECOGNIZED_TEXT.into());
    }
    // Do not call the LLM or inject text until the ASR original is durable.
    history::record(path, &source, engine.name()).map_err(|e| format!("保存转写结果失败：{e}"))?;
    Ok(source)
}

pub fn rewrite_saved(config: &Config, path: &Path, source: String) -> Transcript {
    if !config.llm.enabled {
        return Transcript {
            text: source,
            notice: String::new(),
        };
    }
    match organize_saved(config, path, &source) {
        Ok(organized) => Transcript {
            text: organized.text,
            notice: String::new(),
        },
        Err(error) => Transcript {
            text: source,
            notice: format!("整理未完成，已使用 ASR 原文：{error}"),
        },
    }
}

pub fn organize_saved(
    config: &Config,
    path: &Path,
    source: &str,
) -> Result<drafts::Organized, String> {
    let profile = config
        .llm
        .custom_prompt
        .trim()
        .is_empty()
        .then_some(polish::Profile::Balanced);
    let prompt = profile.map_or_else(|| config.llm.custom_prompt.clone(), polish::Profile::prompt);
    let settings = config.llm.organizer_service();
    let organized = drafts::Organized {
        source: source.into(),
        text: polish::organize_with_prompt(&settings, &prompt, source)?,
        base_url: settings.base_url.clone(),
        model: settings.model.clone(),
        profile,
    };
    drafts::save_organized(path, &organized).map_err(|e| format!("保存整理稿失败：{e}"))?;
    Ok(organized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_rewrite_preserves_audio_and_original_without_legacy_debug() {
        use std::{
            io::{BufRead, BufReader, Read, Write},
            net::TcpListener,
        };
        let dir = std::env::temp_dir().join(format!("speaky-rewrite-{}", uuid::Uuid::new_v4()));
        let path = dir.join("recording.wav");
        drafts::save(&path, b"untouched raw audio").unwrap();
        drafts::record(
            &path,
            "明天下午两点开会",
            std::time::SystemTime::now(),
            Some("test-asr"),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.learn_preferences = true;
        config.desktop.debug_mode = true;
        config.llm.openai.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        config.llm.openai.model = "test-rewriter".into();
        config.llm.openai.api_key = "test-secret".into();
        let asr = path.with_extension("json");
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "POST /v1/chat/completions HTTP/1.1");
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["model"], "test-rewriter");
            assert_eq!(
                request["messages"][0]["content"],
                polish::Profile::Balanced.prompt()
            );
            assert_eq!(request["messages"][1]["content"], "明天下午两点开会");
            assert!(std::fs::read_to_string(asr)
                .unwrap()
                .contains("明天下午两点开会"));
            let response = r#"{"choices":[{"finish_reason":"stop","message":{"content":"明天下午两点开会。"}}]}"#;
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        });
        let result = rewrite_saved(&config, &path, "明天下午两点开会".into());
        server.join().unwrap();
        assert_eq!(result.text, "明天下午两点开会。");
        assert!(result.notice.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"untouched raw audio");
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
        assert_eq!(original["text"], "明天下午两点开会");
        let organized: drafts::Organized =
            serde_json::from_slice(&std::fs::read(path.with_extension("polish.json")).unwrap())
                .unwrap();
        assert_eq!(organized.source, "明天下午两点开会");
        assert_eq!(organized.text, "明天下午两点开会。");
        assert!(!path.with_extension("debug.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabled_or_failed_rewrite_keeps_the_asr_original_and_audio() {
        let dir = std::env::temp_dir().join(format!("speaky-pipeline-{}", uuid::Uuid::new_v4()));
        let path = dir.join("recording.wav");
        drafts::save(&path, b"original audio").unwrap();
        drafts::record(
            &path,
            "原文",
            std::time::SystemTime::now(),
            Some("test-asr"),
        )
        .unwrap();
        let mut config = Config::default();
        assert_eq!(rewrite_saved(&config, &path, "原文".into()).text, "原文");
        config.llm.enabled = true;
        config.desktop.debug_mode = true;
        config.llm.openai.base_url = "invalid".into();
        config.llm.openai.api_key = "secret-key".into();
        config.llm.custom_prompt = "保留原意 secret-key".into();
        let result = rewrite_saved(&config, &path, "原文".into());
        assert_eq!(result.text, "原文");
        assert!(result.notice.contains("ASR 原文"));
        assert_eq!(std::fs::read(&path).unwrap(), b"original audio");
        assert!(!path.with_extension("debug.json").exists());
        assert!(!path.with_extension("polish.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

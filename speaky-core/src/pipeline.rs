//! The final ASR -> durable original -> optional rewrite sequence shared by both clients.
use crate::{config::Config, drafts, engines, history, polish, polish_preferences, session, terms};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub struct Transcript {
    pub text: String,
    pub notice: String,
}

#[derive(Deserialize, Serialize)]
pub struct RewriteDebug {
    pub model: String,
    pub system: String,
    pub input: String,
    pub error: Option<String>,
}

pub fn transcribe_saved(config: &Config, path: &Path) -> Result<Transcript, String> {
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
        return Err("未识别到文字。".into());
    }
    // Do not call the LLM or inject text until the ASR original is durable.
    history::record(path, &source, engine.name()).map_err(|e| format!("保存转写结果失败：{e}"))?;
    Ok(rewrite_saved(config, path, source))
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
    let profile = if !config.llm.custom_prompt.trim().is_empty() {
        None
    } else if config.llm.learns_preferences() {
        Some(polish_preferences::load(&config.llm.openai)?.next_candidate())
    } else {
        Some(polish::Profile::Balanced)
    };
    let prompt = profile.map_or_else(|| config.llm.custom_prompt.clone(), polish::Profile::prompt);
    let settings = &config.llm.openai;
    let result = polish::organize_with_prompt(settings, &prompt, source);
    if config.desktop.debug_mode {
        // Only these request fields are persisted; never serialize the service settings/headers.
        let redact = |value: &str| {
            let key = settings.api_key.trim();
            if key.is_empty() {
                value.to_owned()
            } else {
                value.replace(key, "[REDACTED]")
            }
        };
        let debug = RewriteDebug {
            model: redact(settings.model.trim()),
            system: redact(&prompt),
            input: redact(source),
            error: result.as_ref().err().map(|e| redact(e)),
        };
        let bytes = serde_json::to_vec_pretty(&debug).map_err(|e| e.to_string())?;
        drafts::write_atomically(&path.with_extension("debug.json"), &bytes)
            .map_err(|e| format!("保存调试记录失败：{e}"))?;
    }
    let organized = drafts::Organized {
        source: source.into(),
        text: result?,
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
    fn successful_rewrite_saves_three_independent_artifacts_and_redacted_debug() {
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
        let debug = std::fs::read_to_string(path.with_extension("debug.json")).unwrap();
        assert!(!debug.contains("test-secret"));
        let debug: RewriteDebug = serde_json::from_str(&debug).unwrap();
        assert_eq!(debug.input, organized.source);
        assert_eq!(debug.model, "test-rewriter");
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
        let debug = std::fs::read_to_string(path.with_extension("debug.json")).unwrap();
        assert!(!debug.contains("secret-key"));
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains("原文"));
        assert!(!path.with_extension("polish.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

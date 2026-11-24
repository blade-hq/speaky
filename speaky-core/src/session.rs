use std::time::Instant;

pub const SAMPLE_AUDIO: &[u8] = include_bytes!("../assets/settings-sample.wav");

#[derive(Default)]
pub struct LivePreview {
    pub phrase_start: usize,
    pub last_decoded: usize,
    pub text: String,
    committed_bytes: usize,
    pub error: Option<String>,
    pub retry_after: Option<Instant>,
}

impl LivePreview {
    pub fn new(cursor: usize) -> Self {
        Self {
            phrase_start: cursor,
            last_decoded: cursor,
            ..Self::default()
        }
    }

    pub fn accept(
        &mut self,
        end: usize,
        complete: bool,
        result: Result<String, String>,
    ) -> Result<(), String> {
        // A failed request keeps its audio range and last visible hypothesis.
        let hypothesis = result?;
        let hypothesis = hypothesis.trim();
        self.last_decoded = end;
        if !hypothesis.is_empty() {
            self.text.truncate(self.committed_bytes);
            if self
                .text
                .ends_with(|c: char| c.is_ascii() && !c.is_whitespace())
                && hypothesis.starts_with(|c: char| c.is_ascii_alphanumeric())
            {
                self.text.push(' ');
            }
            self.text.push_str(hypothesis);
        }
        if complete {
            self.phrase_start = end;
            self.committed_bytes = self.text.len();
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub const SILENT_INPUT: &str =
    "录音没有收到声音。请检查 macOS「声音 → 输入」的电平；在虚拟机中还需检查宿主机的麦克风共享。";
#[cfg(windows)]
pub const SILENT_INPUT: &str =
    "录音没有收到声音。请检查 Windows 的麦克风隐私权限，并在 Speaky 设置中选择有输入电平的麦克风。";

pub fn is_silent_recording(wav: &[u8]) -> bool {
    wav.starts_with(b"RIFF")
        && wav.get(8..12) == Some(b"WAVE")
        && wav.get(36..40) == Some(b"data")
        && wav
            .get(44..)
            .is_some_and(|samples| samples.iter().all(|&byte| byte == 0))
}

#[cfg(test)]
mod tests {
    use super::is_silent_recording;

    #[test]
    fn preview_replaces_provisional_words_and_retries_failed_phrases() {
        let mut preview = super::LivePreview::default();
        preview.accept(16000, false, Ok("A值".into())).unwrap();
        assert!(preview.accept(32000, true, Err("服务忙".into())).is_err());
        assert_eq!(
            (
                preview.phrase_start,
                preview.last_decoded,
                preview.text.as_str()
            ),
            (0, 16000, "A值")
        );
        preview.accept(32000, true, Ok("Agent".into())).unwrap();
        preview.accept(48000, false, Ok("red".into())).unwrap();
        preview.accept(64000, true, Ok("RAG".into())).unwrap();
        assert_eq!(
            (preview.phrase_start, preview.text.as_str()),
            (64000, "Agent RAG")
        );
    }

    #[test]
    fn preview_preserves_visible_words_during_an_empty_hypothesis() {
        let mut preview = super::LivePreview::default();
        preview.accept(13000, false, Ok("正在记录".into())).unwrap();
        preview.accept(26000, false, Ok(String::new())).unwrap();
        assert_eq!(preview.text, "正在记录");
        preview.accept(39000, false, Ok("正在录音".into())).unwrap();
        preview.accept(52000, true, Ok(String::new())).unwrap();
        preview.accept(65000, false, Ok("下一句".into())).unwrap();
        assert_eq!(preview.text, "正在录音下一句");
    }

    #[test]
    fn rejects_zero_pcm_without_rejecting_quiet_pcm_or_unknown_formats() {
        let mut wav = [0; 48];
        wav[..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        wav[36..40].copy_from_slice(b"data");
        assert!(is_silent_recording(&wav));
        wav[46] = 1;
        assert!(!is_silent_recording(&wav));
        assert!(!is_silent_recording(&[0; 48]));
    }
}

pub const LANGUAGES: [(&str, &str); 6] = [
    ("auto", "自动检测"),
    ("zh", "中文（普通话）"),
    ("yue", "粤语"),
    ("en", "英语"),
    ("ja", "日语"),
    ("ko", "韩语"),
];

pub const SAMPLE_REFERENCE: &str = "你好，语音识别测试。";

pub fn sample_comparison(text: &str) -> &'static str {
    let recognized = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            '語' => '语',
            '識' => '识',
            '別' => '别',
            '測' => '测',
            '試' => '试',
            other => other,
        });
    if recognized.eq(SAMPLE_REFERENCE.chars().filter(|c| c.is_alphanumeric())) {
        "样本与参考一致"
    } else {
        "样本与参考有差异"
    }
}

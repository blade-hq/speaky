use super::*;
use speaky_core::benchmark_sets::{self, Sample};

const SAMPLE: usize = 300;
const RUN: usize = 301;
const RESULT: usize = 302;
const PLAY_SAMPLE: usize = 303;
const RELOAD: usize = 304;

#[derive(Default)]
pub(super) struct BenchmarkState {
    samples: Vec<(String, Sample)>,
    loaded: bool,
    load_error: Option<String>,
    result: String,
    running: bool,
}

impl BenchmarkState {
    fn load(&mut self) {
        self.samples.clear();
        self.load_error = None;
        match benchmark_sets::load() {
            Ok(datasets) => {
                for dataset in datasets {
                    for sample in dataset.cases {
                        let label = format!("{} · {}", dataset.name, sample.id);
                        self.samples.push((label, sample));
                    }
                }
            }
            Err(error) => self.load_error = Some(error),
        }
        self.loaded = true;
    }
}

fn run_sample(config: &Config, sample: Option<Sample>) -> Result<String, String> {
    let engine = engines::recognition_engine(config)?;
    let (audio, reference) = match sample {
        Some(sample) => (
            std::fs::read(&sample.audio)
                .map_err(|error| format!("无法读取录音 {}：{error}", sample.id))?,
            Some(sample.expected),
        ),
        None => (SAMPLE_AUDIO.to_vec(), None),
    };
    let started = Instant::now();
    let text = engine
        .transcribe(&audio, "auto")
        .map_err(|error| format!("识别失败：{error}"))?;
    if text.trim().is_empty() {
        return Err("识别返回空白文本。".into());
    }
    let elapsed = started.elapsed().as_secs_f64();
    let mut result = format!(
        "识别引擎：{}\n耗时：{elapsed:.2} 秒\n\n识别结果\n{text}",
        engine.name()
    );
    if let Some(reference) = reference {
        result.push_str("\n\n参考文本\n");
        result.push_str(&reference);
    }
    Ok(result)
}

impl Window {
    pub(super) fn benchmark_layout(&mut self) {
        if !self.benchmark.loaded {
            self.benchmark.load();
        }
        let mut labels = vec!["内置测试样本".to_owned()];
        labels.extend(
            self.benchmark
                .samples
                .iter()
                .map(|(label, _)| label.clone()),
        );
        let labels: Vec<_> = labels.iter().map(String::as_str).collect();
        self.combo(SAMPLE, "测试样本", 66, &labels, 0);
        self.button(RUN, "开始识别", 14, 110, 115);
        self.button(PLAY_SAMPLE, "播放样本", 141, 110, 115);
        self.button(RELOAD, "重新读取测试集", 268, 110, 140);
        let report = if self.benchmark.result.is_empty() {
            self.benchmark
                .load_error
                .as_deref()
                .unwrap_or("请选择样本，然后开始识别。")
        } else {
            &self.benchmark.result
        };
        self.control(
            RESULT,
            "EDIT",
            &report.replace('\n', "\r\n"),
            (14, 152, 620, 255),
            WS_TABSTOP | WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
        );
    }

    pub(super) fn benchmark_command(&mut self, id: usize) -> bool {
        if self.panel != Panel::Benchmark || !matches!(id, RUN | PLAY_SAMPLE | RELOAD) {
            return false;
        }
        if self.audio_busy() || self.auxiliary_busy {
            self.status("请先完成当前操作。");
            return true;
        }
        let index = self.send(SAMPLE, CB_GETCURSEL, 0, 0);
        let sample = usize::try_from(index - 1)
            .ok()
            .and_then(|index| self.benchmark.samples.get(index))
            .map(|(_, sample)| sample.clone());
        match id {
            RELOAD => {
                self.benchmark.load();
                self.benchmark.result.clear();
                self.layout();
            }
            PLAY_SAMPLE => {
                if self.play.take().is_some() {
                    self.status("已停止播放。");
                } else if let Some(sample) = sample {
                    match Playback::start(&sample.audio) {
                        Ok(play) => {
                            self.play = Some(play);
                            self.status("正在播放测试样本。");
                        }
                        Err(error) => self.status(format!("播放失败：{error}")),
                    }
                } else {
                    self.status("请选择测试集中的样本播放。");
                }
            }
            RUN => {
                self.play = None;
                self.benchmark.running = true;
                self.auxiliary_busy = true;
                self.status("正在识别测试样本…");
                self.set(RESULT, "");
                let config = self.config.clone();
                let updates = self.updates.clone();
                thread::spawn(move || {
                    let report = run_sample(&config, sample).unwrap_or_else(|error| error);
                    updates.send(Event::Aux(report));
                });
            }
            _ => unreachable!(),
        }
        true
    }

    // Event::Aux is also used by diagnostics. Consume only our pending result.
    pub(super) fn benchmark_report(&mut self, report: &str) -> bool {
        if !self.benchmark.running {
            return false;
        }
        self.benchmark.running = false;
        self.benchmark.result = report.to_owned();
        if self.panel == Panel::Benchmark {
            self.set(RESULT, &report.replace('\n', "\r\n"));
        }
        self.status("测试已完成，请查看结果。");
        true
    }
}

use crate::{
    benchmark, button, card, page_heading, playback::Playback, Speaky, ACCENT, BORDER, INK, MUTED,
    SIDEBAR,
};
use gpui::{div, prelude::*, px, rgb, rgba, Context, Div};
use std::collections::HashMap;

// Keep a result with the exact model and recording that produced it, even if
// the listener changes the selection before transcription finishes.
#[derive(Clone, Hash, PartialEq, Eq)]
struct RunKey {
    dataset: usize,
    sample: usize,
    model: benchmark::BenchmarkModel,
}

#[derive(Default)]
pub(super) struct BenchmarkPage {
    datasets: Vec<benchmark::Dataset>,
    dataset: usize,
    sample: usize,
    loading: bool,
    load_error: Option<String>,
    show_samples: bool,
    pub(super) playing: bool,
    playback_error: Option<String>,
    pending: Option<RunKey>,
    results: HashMap<RunKey, Result<String, String>>,
}

impl BenchmarkPage {
    pub(super) fn is_running(&self) -> bool {
        self.pending.is_some()
    }

    fn selected(&self) -> Option<(&benchmark::Dataset, &benchmark::Sample)> {
        let set = self.datasets.get(self.dataset)?;
        Some((set, set.cases.get(self.sample)?))
    }

    fn key(&self, model: &benchmark::BenchmarkModel) -> RunKey {
        RunKey {
            dataset: self.dataset,
            sample: self.sample,
            model: model.clone(),
        }
    }
}

fn section_label(label: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(label.into())
}

fn dataset_label(name: &str) -> String {
    match name {
        "ASCEND" => "ASCEND · 中英混说".into(),
        "Google FLEURS" => "FLEURS · 普通话朗读".into(),
        other => other.to_owned(),
    }
}

impl Speaky {
    pub(super) fn load_benchmark(&mut self, cx: &mut Context<Self>) {
        if self.benchmark.loading || !self.benchmark.datasets.is_empty() {
            return;
        }
        self.benchmark.loading = true;
        self.benchmark.load_error = None;
        let task = cx.background_spawn(async { benchmark::load() });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                let page = &mut view.benchmark;
                page.loading = false;
                match result {
                    Ok(sets) if sets.iter().any(|set| !set.cases.is_empty()) => {
                        page.datasets = sets
                            .into_iter()
                            .filter(|set| !set.cases.is_empty())
                            .collect();
                        page.dataset = 0;
                        page.sample = 0;
                    }
                    Ok(_) => page.load_error = Some("测试集中没有可用录音。".into()),
                    Err(error) => page.load_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn select_benchmark_sample(&mut self, dataset: usize, sample: usize, cx: &mut Context<Self>) {
        if self
            .benchmark
            .datasets
            .get(dataset)
            .and_then(|set| set.cases.get(sample))
            .is_none()
        {
            return;
        }
        let changed = (dataset, sample) != (self.benchmark.dataset, self.benchmark.sample);
        if changed && self.benchmark.playing {
            self.stop_playback();
        }
        self.benchmark.dataset = dataset;
        self.benchmark.sample = sample;
        self.benchmark.show_samples = false;
        self.benchmark.playback_error = None;
        cx.notify();
    }

    pub(super) fn play_benchmark_sample(&mut self, cx: &mut Context<Self>) {
        if self.benchmark.playing {
            self.stop_playback();
        } else if self.recording.is_none() {
            let Some((_, sample)) = self.benchmark.selected() else {
                return;
            };
            let path = sample.audio.clone();
            self.stop_playback();
            self.benchmark.playback_error = None;
            match Playback::start(&path) {
                Ok(playback) => {
                    self.playback = Some(playback);
                    self.benchmark.playing = true;
                }
                Err(error) => self.benchmark.playback_error = Some(format!("播放失败：{error}")),
            }
        }
        cx.notify();
    }

    fn recognize_benchmark_sample(&mut self, cx: &mut Context<Self>) {
        if self.benchmark.is_running()
            || self.local_model_switching()
            || self.model_switch_blocker().is_some()
        {
            return;
        }
        let Some(model) = self.model_picker.benchmark_model() else {
            return;
        };
        let Some((_, sample)) = self.benchmark.selected() else {
            return;
        };
        let sample = sample.clone();
        let key = self.benchmark.key(model);
        let model = key.model.clone();
        self.benchmark.pending = Some(key.clone());
        self.model_picker.close();
        let task = cx.background_spawn(async move { benchmark::recognize(&model, &sample) });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.benchmark.pending = None;
                view.benchmark.results.insert(key, result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn benchmark_page(&self, cx: &mut Context<Self>) -> Div {
        let state = &self.benchmark;
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(page_heading(
                        "模型评测",
                        "试听样本，对照参考文字评估识别效果。",
                    ))
                    .child(self.model_picker_button(cx)),
            )
            .when_some(self.model_picker_panel(cx), |page, panel| page.child(panel));
        if let Some(error) = &state.load_error {
            return page.child(
                card()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .text_color(rgb(crate::ERROR))
                            .child(error.clone()),
                    )
                    .child(
                        button("重新读取", false)
                            .id("benchmark-retry")
                            .on_click(cx.listener(|view, _, _, cx| view.load_benchmark(cx))),
                    ),
            );
        }
        let Some((set, sample)) = state.selected() else {
            return page.child(section_label("正在读取本地测试集…"));
        };
        let (dataset, index, count) = (state.dataset, state.sample, set.cases.len());
        let mut pick = card()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_label("数据集"))
            .child(div().flex().flex_wrap().gap_2().children(
                state.datasets.iter().enumerate().map(|(i, s)| {
                    button(dataset_label(&s.name), dataset == i)
                        .id(("benchmark-dataset", i))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if view.benchmark.dataset != i {
                                view.select_benchmark_sample(i, 0, cx)
                            }
                        }))
                }),
            ))
            .child(div().mt_1().child(section_label("录音")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("benchmark-select-sample")
                            .flex_1()
                            .min_w(px(0.))
                            .h(px(32.))
                            .px_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(if state.show_samples { ACCENT } else { BORDER }))
                            .bg(rgb(SIDEBAR))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|s| s.border_color(rgb(ACCENT)))
                            .child(div().flex_none().child(format!(
                                "第 {} / {} 条",
                                index + 1,
                                count
                            )))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_color(rgb(MUTED))
                                    .child(sample.expected.clone()),
                            )
                            .child(if state.show_samples { "▴" } else { "▾" })
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.benchmark.show_samples = !view.benchmark.show_samples;
                                view.model_picker.close();
                                cx.notify();
                            })),
                    )
                    .child(
                        button("上一条", false)
                            .id("benchmark-previous")
                            .when(index == 0, |b| b.opacity(0.45))
                            .when(index > 0, |b| {
                                b.on_click(cx.listener(move |view, _, _, cx| {
                                    view.select_benchmark_sample(dataset, index - 1, cx)
                                }))
                            }),
                    )
                    .child(
                        button("下一条", false)
                            .id("benchmark-next")
                            .when(index + 1 == count, |b| b.opacity(0.45))
                            .when(index + 1 < count, |b| {
                                b.on_click(cx.listener(move |view, _, _, cx| {
                                    view.select_benchmark_sample(dataset, index + 1, cx)
                                }))
                            }),
                    ),
            );
        if state.show_samples {
            pick = pick.child(
                div()
                    .id("benchmark-sample-list")
                    .max_h(px(174.))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded_md()
                    .bg(rgb(SIDEBAR))
                    .children(set.cases.iter().enumerate().map(|(i, s)| {
                        div()
                            .id(("benchmark-sample", i))
                            .h(px(28.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .cursor_pointer()
                            .when(i == index, |row| row.bg(rgba(0x28584828)))
                            .hover(|style| style.bg(rgba(0x28584835)))
                            .child(
                                div()
                                    .w(px(33.))
                                    .text_color(rgb(ACCENT))
                                    .child(format!("#{}", i + 1)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(s.expected.clone()),
                            )
                            .child(section_label(format!("{:.1} 秒", s.duration)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.select_benchmark_sample(dataset, i, cx)
                            }))
                    })),
            );
        }
        page = page.child(pick);
        let key = self
            .model_picker
            .benchmark_model()
            .map(|model| state.key(model));
        let pending = state.pending.is_some() && state.pending == key;
        let result = key.as_ref().and_then(|k| state.results.get(k));
        let blocked = if self.local_model_switching() {
            Some("正在切换模型…")
        } else if pending {
            Some("正在识别这条录音…")
        } else if state.is_running() {
            Some("正在识别其他录音…")
        } else if key.is_none() {
            Some("请先选择本地模型或配置识别服务")
        } else {
            self.model_switch_blocker()
                .map(|_| "请等待当前录音或识别任务结束")
        };
        let actions = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .child(
                button(
                    if state.playing {
                        "停止原声"
                    } else {
                        "播放原声"
                    },
                    false,
                )
                .id("benchmark-play")
                .when(self.recording.is_some(), |b| b.opacity(0.45))
                .when(self.recording.is_none(), |b| {
                    b.on_click(cx.listener(|view, _, _, cx| view.play_benchmark_sample(cx)))
                }),
            )
            .child(section_label(format!("{:.1} 秒", sample.duration)))
            .child(
                button(
                    if pending {
                        "识别中…"
                    } else if result.is_some() {
                        "重新识别"
                    } else {
                        "识别此条"
                    },
                    true,
                )
                .id("benchmark-recognize")
                .when(blocked.is_some(), |b| b.opacity(0.5))
                .when(blocked.is_none(), |b| {
                    b.on_click(cx.listener(|view, _, _, cx| view.recognize_benchmark_sample(cx)))
                }),
            )
            .when_some(blocked, |row, reason| row.child(section_label(reason)));
        page.child(
            card()
                .flex()
                .flex_col()
                .gap_2()
                .child(actions)
                .when_some(state.playback_error.as_ref(), |c, e| {
                    c.child(div().text_color(rgb(crate::ERROR)).child(e.clone()))
                })
                .child(div().mt_1().child(section_label("人工参考")))
                .child(div().text_color(rgb(INK)).child(sample.expected.clone()))
                .child(
                    div()
                        .mt_2()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .pt_2()
                        .child(section_label(match key {
                            Some(k) => format!("模型输出 · {}", k.model.label()),
                            None => "模型输出".into(),
                        })),
                )
                .child(match (pending, result) {
                    (true, _) => section_label("识别中…"),
                    (_, Some(Ok(text))) if text.is_empty() => section_label("模型返回了空文本。"),
                    (_, Some(Ok(text))) => div().text_color(rgb(INK)).child(text.clone()),
                    (_, Some(Err(error))) => {
                        div().text_color(rgb(crate::ERROR)).child(error.clone())
                    }
                    (_, None) => section_label("尚未识别"),
                }),
        )
    }
}

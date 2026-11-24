use crate::{
    button,
    config::Config,
    drafts, polish, polish_benchmark, polish_preferences,
    settings::detail,
    Speaky,
};
use gpui::{div, prelude::*, px, ClipboardItem, Context, Div};
use std::path::PathBuf;

#[derive(Default)]
pub struct Organizer {
    pending: Option<PathBuf>,
    notice: Option<(PathBuf, String)>,
}

pub(crate) struct BenchmarkResult {
    model: String,
    profile: polish::Profile,
    report: polish_benchmark::Report,
}

impl Speaky {
    pub(crate) fn rate_organized(
        &mut self,
        path: &std::path::Path,
        good: bool,
        cx: &mut Context<Self>,
    ) {
        if !Config::load().is_ok_and(|config| config.llm.learns_preferences()) {
            return;
        }
        let result = (|| {
            let entry = self.archive.iter().find(|entry| entry.path == path)?;
            let organized = entry.organized.as_ref()?;
            let drafts::State::Transcribed { text } = &entry.state else {
                return None;
            };
            if text != &organized.source {
                return None;
            }
            let sample = path.file_stem()?.to_str()?;
            let profile = organized.profile?;
            Some(
                polish_preferences::load(&organized.model_settings())
                    .and_then(|mut preferences| preferences.record(sample, profile, good)),
            )
        })();
        if let Some(result) = result {
            self.organizer.notice = result.err().map(|error| (path.to_owned(), error));
            cx.notify();
        }
    }

    pub(crate) fn organize_transcript(
        &mut self,
        path: PathBuf,
        source: String,
        cx: &mut Context<Self>,
    ) {
        if self.organizer.pending.is_some() {
            return;
        }
        let setup = (|| {
            let config = Config::load().map_err(|_| "无法读取整理配置。")?;
            if !config.llm.enabled {
                return Err("请在设置中启用语音整理。".to_owned());
            }
            let profile = if !config.llm.custom_prompt.trim().is_empty() {
                None
            } else if config.llm.learns_preferences() {
                Some(polish_preferences::load(&config.llm.openai)?.next_candidate())
            } else {
                Some(polish::Profile::Balanced)
            };
            let prompt = profile.map_or(config.llm.custom_prompt, polish::Profile::prompt);
            Ok((config.llm.openai, profile, prompt))
        })();
        let (service, profile, prompt) = match setup {
            Ok(setup) => setup,
            Err(error) => {
                self.organizer.notice = Some((path, error));
                cx.notify();
                return;
            }
        };
        self.organizer.pending = Some(path.clone());
        self.organizer.notice = None;
        let task = cx.background_spawn(async move {
            let text = polish::organize_with_prompt(&service, &prompt, &source)?;
            Ok::<_, String>(drafts::Organized {
                source,
                text,
                base_url: service.base_url,
                model: service.model,
                profile,
            })
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.organizer.pending = None;
                let result = result.and_then(|result| {
                    drafts::save_organized(&path, &result)
                        .map_err(|_| "无法保存整理稿，请重试。".to_owned())
                });
                match result {
                    Ok(()) => view.archive = drafts::list(),
                    Err(error) => view.organizer.notice = Some((path, error)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn organized_result(
        &self,
        entry: &drafts::Entry,
        source: &str,
        cx: &mut Context<Self>,
    ) -> Div {
        let pending = self.organizer.pending.as_ref() == Some(&entry.path);
        let notice = self
            .organizer
            .notice
            .as_ref()
            .filter(|(path, _)| path == &entry.path);
        div()
            .when(pending, |body| body.child(detail("正在整理…")))
            .when_some(notice, |body, (_, message)| {
                body.child(detail(message.clone()))
            })
            .when_some(entry.organized.as_ref(), |body, result| {
                let text = result.text.clone();
                let profile = result.profile;
                let sample = entry
                    .path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned);
                let path = entry.path.clone();
                let mut actions = div().flex().gap_1().flex_wrap().child(
                    button("复制整理稿", false)
                        .id("copy-organized")
                        .invisible()
                        .group_hover("history-row", |style| style.visible())
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                        })),
                );
                let current = result.source == source;
                if let (Some(sample), Some(profile)) = (
                    sample.filter(|_| current && self.organizer_learning),
                    profile,
                ) {
                    let vote = polish_preferences::load(&result.model_settings())
                        .ok()
                        .and_then(|preferences| preferences.vote(&sample, profile));
                    actions =
                        actions.children([("好", true), ("不好", false)].map(|(label, good)| {
                            let path = path.clone();
                            button(label, vote == Some(good))
                                .id(label)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.rate_organized(&path, good, cx);
                                }))
                        }));
                }
                body.child(
                    div()
                        .mt_1()
                        .pl_2()
                        .border_l_1()
                        .border_color(gpui::rgb(crate::BORDER))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(detail(format!(
                            "整理稿 · {} · {}",
                            profile.map_or("自定义提示词", polish::Profile::label),
                            result.model
                        )))
                        .when(!current, |body| body.child(detail("来自上一版转写")))
                        .child(result.text.clone())
                        .child(actions),
                )
            })
    }

    pub(crate) fn benchmark(&mut self, cx: &mut Context<Self>) {
        if self.settings.polish_checking {
            return;
        }
        let setup = (|| {
            let config = Config::load().map_err(|_| "无法读取整理配置。")?;
            if !config.llm.enabled {
                return Err("请在设置中启用语音整理。".to_owned());
            }
            Ok(config.llm.openai)
        })();
        let service = match setup {
            Ok(setup) => setup,
            Err(error) => {
                self.settings.polish_report = Some(Err(error));
                cx.notify();
                return;
            }
        };
        self.settings.polish_checking = true;
        self.settings.polish_report = None;
        let model = service.model.clone();
        let profile = polish::Profile::Balanced;
        let task = cx.background_spawn(async move { polish_benchmark::run(&service, profile) });
        cx.spawn(async move |entity, cx| {
            let result = task.await.map(|report| BenchmarkResult {
                model,
                profile,
                report,
            });
            let _ = entity.update(cx, |view, cx| {
                view.settings.polish_checking = false;
                view.settings.polish_report = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn benchmark_result(&self) -> Div {
        let Some(report) = &self.settings.polish_report else {
            return div();
        };
        let body = div()
            .p_2()
            .px_3()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(12.));
        match report {
            Err(error) => body.child(detail(error.clone())),
            Ok(BenchmarkResult {
                model,
                profile,
                report,
            }) => body
                .child(format!(
                    "{model} · {} · {}/100 · {}",
                    profile.label(),
                    report.score,
                    if report.passed { "通过" } else { "未通过" }
                ))
                .child(detail("使用固定提示词检查合成样本中的事实保留。"))
                .children(report.cases.iter().map(|case| {
                    div()
                        .border_t_1()
                        .border_color(gpui::rgb(crate::BORDER))
                        .pt_2()
                        .child(format!(
                            "{} · {:.2} 秒",
                            case.name,
                            case.elapsed_ms as f64 / 1000.
                        ))
                        .child(detail(case.input))
                        .child(case.output.clone())
                        .children(case.issues.iter().cloned().map(detail))
                })),
        }
    }
}

use crate::{button, config::Config, drafts, polish, polish_benchmark, settings::detail, Speaky};
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
            Ok(config)
        })();
        let config = match setup {
            Ok(setup) => setup,
            Err(error) => {
                self.organizer.notice = Some((path, error));
                cx.notify();
                return;
            }
        };
        self.organizer.pending = Some(path.clone());
        self.organizer.notice = None;
        let request_path = path.clone();
        let task = cx.background_spawn(async move {
            crate::pipeline::organize_saved(&config, &request_path, &source)
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
                let copy = button("复制整理稿", false)
                    .id("copy-organized")
                    .invisible()
                    .group_hover("history-row", |style| style.visible())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                    }));
                let current = result.source == source;
                body.child(
                    div()
                        .mt_1()
                        .pl_2()
                        .border_l_1()
                        .border_color(gpui::rgb(crate::BORDER()))
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
                        .child(div().flex().child(copy)),
                )
            })
    }

    pub(crate) fn benchmark(&mut self, cx: &mut Context<Self>) {
        if self.settings.polish_checking {
            return;
        }
        let setup = (|| {
            let config = Config::load().map_err(|_| "无法读取整理配置。")?;
            if !crate::settings::rewrite_configured(&config) {
                return Err("请先配置整理服务。".to_owned());
            }
            Ok(config.llm.organizer_service().into_owned())
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
                        .border_color(gpui::rgb(crate::BORDER()))
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

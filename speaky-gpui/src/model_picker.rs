use super::{
    config::Config, local_models, ActionTooltip, Page, Speaky, ACCENT, BORDER, INK, MUTED, SIDEBAR,
};
use gpui::{div, prelude::*, px, rgb, rgba, Context, Div, Stateful};

#[derive(Default)]
pub(super) struct ModelPicker {
    label: String,
    selected: Option<String>,
    benchmark_model: Option<crate::benchmark::BenchmarkModel>,
    expanded: bool,
    listing: bool,
    models: Vec<local_models::Model>,
    pub(super) switching: Option<String>,
    pub(super) preloading: bool,
    preload_error: Option<String>,
    error: Option<String>,
    pub(super) pending_previews: usize,
}

impl ModelPicker {
    pub(super) fn new(config: Option<&Config>) -> Self {
        let mut picker = Self::default();
        picker.read_selection(config);
        picker
    }

    fn read_selection(&mut self, config: Option<&Config>) {
        self.label = config
            .map(local_models::selected_label)
            .unwrap_or_else(|| "配置读取失败".into());
        self.selected = config
            .and_then(local_models::selected_id)
            .map(str::to_owned);
        self.benchmark_model = config.and_then(crate::benchmark::BenchmarkModel::selected);
    }

    pub(super) fn close(&mut self) {
        self.expanded = false;
        self.error = None;
    }

    pub(super) fn switching_notice(&self) -> Option<&str> {
        self.switching.as_deref()
    }

    pub(super) fn preload_notice(&self) -> Option<&str> {
        if self.preloading {
            Some("正在预加载本机识别模型，可以先录音。")
        } else {
            self.preload_error.as_deref()
        }
    }

    pub(super) fn benchmark_model(&self) -> Option<&crate::benchmark::BenchmarkModel> {
        self.benchmark_model.as_ref()
    }

    pub(super) fn guide_is_open(&self) -> bool {
        self.expanded
    }

    pub(super) fn guide_has_no_models(&self) -> bool {
        self.expanded && !self.listing && self.models.is_empty()
    }
}

impl Speaky {
    pub(crate) fn local_model_switching(&self) -> bool {
        self.model_picker.switching.is_some()
    }

    pub(crate) fn refresh_model_selection(&mut self) {
        let previously_selected = self.model_picker.selected.clone();
        self.model_picker
            .read_selection(Config::load().ok().as_ref());
        if self.model_picker.selected != previously_selected {
            self.model_picker.preload_error = None;
        }
        self.model_picker.expanded = false;
    }

    pub(super) fn preload_selected_model(&mut self, config: Config, cx: &mut Context<Self>) {
        if !config.desktop.preload_local_model
            || self.model_picker.preloading
            || local_models::selected_id(&config).is_none_or(|id| id.trim().is_empty())
        {
            return;
        }
        self.model_picker.preloading = true;
        self.model_picker.preload_error = None;
        let task =
            cx.background_spawn(async move { local_models::ready_engine(&config).map(|_| ()) });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.model_picker.preloading = false;
                view.model_picker.preload_error = result
                    .err()
                    .map(|error| format!("预加载失败，首次听写时会重试。{error}"));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn model_switch_blocker(&self) -> Option<&'static str> {
        if self.recording.is_some() || self.transcribing || self.model_picker.pending_previews != 0
        {
            Some("录音或转写结束后可切换模型")
        } else if self.benchmark.is_running() {
            Some("本条录音识别结束后可切换模型")
        } else if self.settings.recognition_checking() {
            Some("识别服务检查结束后可切换模型")
        } else if self.model_picker.preloading {
            Some("预加载结束后可切换模型")
        } else {
            None
        }
    }

    fn model_picker_busy(&self) -> bool {
        self.local_model_switching() || self.model_switch_blocker().is_some()
    }

    pub(super) fn toggle_model_picker(&mut self, cx: &mut Context<Self>) {
        if self.model_picker_busy() {
            return;
        }
        if self.model_picker.expanded {
            self.model_picker.close();
            cx.notify();
            return;
        }
        self.refresh_model_selection();
        self.model_picker.expanded = true;
        self.model_picker.error = None;
        if self.tour.at(crate::onboarding::Step::Model) {
            self.tour.request_reveal();
        }
        if !self.model_picker.listing {
            self.model_picker.listing = true;
            self.model_picker.models.clear();
            let scan = cx.background_spawn(async { local_models::list() });
            cx.spawn(async move |entity, cx| {
                let result = scan.await;
                let _ = entity.update(cx, |view, cx| {
                    let picker = &mut view.model_picker;
                    picker.listing = false;
                    match result {
                        Ok(models) => picker.models = models,
                        Err(error) if picker.expanded => picker.error = Some(error),
                        Err(_) => {}
                    }
                    if view.tour.at(crate::onboarding::Step::Model) {
                        view.tour.model_rows_arrived();
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn choose_model(&mut self, model: local_models::Model, cx: &mut Context<Self>) {
        if self.model_picker_busy() || !self.model_picker.expanded || self.model_picker.listing {
            return;
        }
        self.model_picker.close();
        self.model_picker.switching = Some(
            if self.model_picker.selected.as_deref() == Some(model.id.as_str()) {
                format!("正在准备 GPU 模型 {}…", model.label)
            } else {
                format!("正在释放上一模型并加载 {} 到 GPU…", model.label)
            },
        );
        let task = cx.background_spawn(async move { local_models::activate(&model.id) });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                let result = result.and_then(|endpoint| {
                    if let Some(reason) = view.model_switch_blocker() {
                        return Err(reason.into());
                    }
                    local_models::select(&endpoint)
                });
                view.model_picker.switching = None;
                view.refresh_model_selection();
                if let Err(error) = result {
                    view.model_picker.error = Some(format!("切换失败：{error}"));
                    view.model_picker.expanded =
                        matches!(view.page, Page::Home | Page::Benchmark | Page::History);
                } else if view.page == Page::Settings {
                    view.reload_settings();
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn model_picker_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let picker = &self.model_picker;
        let busy = self.model_picker_busy();
        let tooltip = self
            .model_switch_blocker()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("当前识别模型：{}", picker.label));
        div()
            .id("home-model-picker")
            .min_w(px(0.))
            .max_w(px(255.))
            .h(px(34.))
            .px_3()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(if picker.expanded { ACCENT } else { BORDER }))
            .bg(rgb(SIDEBAR))
            .text_size(px(12.))
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(MUTED))
                    .child(if picker.selected.is_some() {
                        "模型 · GPU"
                    } else {
                        "模型"
                    }),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .text_color(rgb(INK))
                    .child(picker.label.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(ACCENT))
                    .child(if picker.expanded { "▴" } else { "▾" }),
            )
            .tooltip(move |_, cx| cx.new(|_| ActionTooltip(tooltip.clone().into())).into())
            .when(busy, |button| button.opacity(0.65))
            .when(!busy, |button| {
                button
                    .when(
                        self.studio_controls_active()
                            || self.tour.at(crate::onboarding::Step::Model),
                        |button| {
                            button
                                .focusable()
                                .tab_stop(true)
                                .focus(|style| style.border_color(rgb(ACCENT)))
                        },
                    )
                    .cursor_pointer()
                    .hover(|style| style.border_color(rgb(ACCENT)))
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_model_picker(cx)))
            })
    }

    pub(super) fn model_picker_panel(&self, cx: &mut Context<Self>) -> Option<Div> {
        let picker = &self.model_picker;
        if !picker.expanded && picker.error.is_none() {
            return None;
        }
        let mut panel = div()
            .flex()
            .flex_col()
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SIDEBAR));
        panel = panel.child(
            super::button("配置识别服务…", false)
                .id("open-service-settings")
                .when(self.tour.at(crate::onboarding::Step::Model), |button| {
                    button
                        .focusable()
                        .tab_stop(true)
                        .focus(|button| button.border_1().border_color(rgb(ACCENT)))
                })
                .on_click(
                    cx.listener(|view, _, window, cx| view.open_service_settings(window, cx)),
                ),
        );
        if let Some(error) = &picker.error {
            panel = panel.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(crate::ERROR))
                    .child(error.clone()),
            );
        }
        if !picker.expanded {
            return Some(panel);
        }
        if picker.listing {
            return Some(
                panel.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(rgb(MUTED))
                        .child("正在查找本地语音模型…"),
                ),
            );
        }
        if picker.models.is_empty() && picker.error.is_none() {
            return Some(
                panel.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(rgb(MUTED))
                        .child("未找到本地语音识别模型。"),
                ),
            );
        }
        Some(
            panel.child(
                div()
                    .id("home-model-options")
                    .max_h(px(212.))
                    .overflow_y_scroll()
                    .children(picker.models.iter().enumerate().map(|(index, model)| {
                        let selected = picker.selected.as_deref() == Some(model.id.as_str());
                        let model = model.clone();
                        div()
                            .id(("home-model-option", index))
                            .when_some(
                                self.tour.model_row_motion(index),
                                |row, (opacity, offset)| {
                                    row.relative().top(offset).opacity(opacity)
                                },
                            )
                            .h(px(34.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .cursor_pointer()
                            .when(self.tour.at(crate::onboarding::Step::Model), |row| {
                                row.focusable()
                                    .tab_stop(true)
                                    .focus(|row| row.border_1().border_color(rgb(ACCENT)))
                            })
                            .when(selected, |row| row.bg(rgba(0x28584825)))
                            .hover(|style| style.bg(rgba(0x28584830)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(model.label.clone()),
                            )
                            .when(selected, |row| {
                                row.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(ACCENT))
                                        .child("已选择"),
                                )
                            })
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.choose_model(model.clone(), cx)
                            }))
                    })),
            ),
        )
    }
}

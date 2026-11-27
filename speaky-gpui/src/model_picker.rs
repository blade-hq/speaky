#[cfg(windows)]
use super::local_models;
use super::{
    config::Config, model_packs, ActionTooltip, Page, Speaky, ACCENT, BORDER, INK, MUTED, SIDEBAR,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{div, prelude::*, px, rgb, rgba, Context, Div, Stateful};
use speaky_core::model_load::ModelLoad;

use speaky_core::model_load::CANCELLED_MESSAGE;

pub(super) fn stopped_model_load_message<T>(result: &Result<T, String>) -> String {
    match result {
        Err(error) if error != CANCELLED_MESSAGE => format!("停止加载时发生错误：{error}"),
        _ => CANCELLED_MESSAGE.into(),
    }
}

#[derive(Clone)]
struct Candidate {
    id: String,
    label: String,
    pack: Option<model_packs::Pack>,
}

struct ModelLoading {
    id: String,
    motion_started: Option<Instant>,
    request: Arc<ModelLoad>,
}

pub(super) fn selected_id(config: &Config) -> Option<&str> {
    #[cfg(windows)]
    if let Some(id) = local_models::selected_id(config) {
        if model_packs::selected_id(config).is_none() {
            return Some(id);
        }
    }
    model_packs::selected_id(config)
}

pub(super) fn selected_label(config: &Config) -> String {
    #[cfg(windows)]
    if model_packs::selected_id(config).is_none() {
        return local_models::selected_label(config);
    }
    model_packs::selected_label(config)
}

fn available_models() -> Result<Vec<Candidate>, String> {
    let packs = model_packs::list();
    #[cfg(windows)]
    let gpu = local_models::list();
    #[cfg(not(windows))]
    let gpu: Result<Vec<Candidate>, String> = Ok(Vec::new());
    #[cfg(windows)]
    let gpu = gpu.map(|models| {
        models
            .into_iter()
            .map(|model| Candidate {
                id: model.id,
                label: model.label,
                pack: None,
            })
            .collect::<Vec<_>>()
    });
    let mut models = Vec::new();
    match (packs, gpu) {
        (Err(pack_error), Err(gpu_error)) => return Err(format!("{pack_error} {gpu_error}")),
        (packs, gpu) => {
            models.extend(packs.unwrap_or_default().into_iter().map(|pack| Candidate {
                id: pack.id.clone(),
                label: pack.label.clone(),
                pack: Some(pack),
            }));
            models.extend(gpu.unwrap_or_default());
        }
    }
    Ok(models)
}

fn activate(model: Candidate, load: &ModelLoad) -> Result<(), String> {
    if let Some(pack) = model.pack {
        return activate_pack(&pack, Some(load));
    }
    #[cfg(windows)]
    {
        load.check()?;
        model_packs::release()?;
        let endpoint = local_models::activate_cancellable(&model.id, load)?;
        if let Err(error) = local_models::select(&endpoint) {
            if let Err(release_error) = local_models::release() {
                log::warn!("释放本地 GPU 模型失败：{release_error}");
            }
            return Err(error);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    Err("找不到本地模型包。".into())
}

fn activate_pack(pack: &model_packs::Pack, load: Option<&ModelLoad>) -> Result<(), String> {
    if let Some(load) = load {
        load.check()?;
    }
    #[cfg(windows)]
    if local_models::has_owned_manager() {
        local_models::release()?;
    }
    match load {
        Some(load) => model_packs::activate_cancellable(pack, load)?,
        None => model_packs::activate(pack)?,
    };
    if let Err(error) = crate::settings::save_local_pack(&pack.id) {
        if let Err(release_error) = model_packs::release() {
            log::warn!("释放本地模型失败：{release_error}");
        }
        return Err(error);
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct ModelPicker {
    label: String,
    selected: Option<String>,
    #[cfg(windows)]
    benchmark_model: Option<crate::benchmark::BenchmarkModel>,
    expanded: bool,
    listing: bool,
    models: Vec<Candidate>,
    pub(super) switching: Option<String>,
    pub(super) switching_model: Option<String>,
    loading: Option<ModelLoading>,
    pub(super) load_feedback: Option<String>,
    pub(super) unload_feedback: Option<Result<String, String>>,
    pub(super) preloading: bool,
    preload_error: Option<String>,
    loaded_model: Option<String>,
    error: Option<String>,
    download: Option<Arc<model_packs::WhisperDownload>>,
    can_download_whisper: bool,
    pub(super) pending_previews: usize,
}

impl ModelPicker {
    pub(super) fn finish_loading(&mut self) {
        self.loading = None;
    }

    pub(super) fn animates_loading(&self) -> bool {
        self.loading.as_ref().is_some_and(|load| {
            load.request.is_loading() && load.motion_started.is_some() && self.has_model(&load.id)
        })
    }

    fn has_loading_row(&self) -> bool {
        self.loading
            .as_ref()
            .is_some_and(|load| self.has_model(&load.id))
    }

    fn loading_label(&self) -> Option<&'static str> {
        let load = self.loading.as_ref()?;
        Some(if load.request.is_cancelled() {
            "正在停止加载…"
        } else if self.preloading {
            "正在预加载模型…"
        } else {
            "正在加载模型…"
        })
    }

    pub(super) fn new(config: Option<&Config>) -> Self {
        let mut picker = Self::default();
        picker.read_selection(config);
        picker
    }

    fn read_selection(&mut self, config: Option<&Config>) {
        self.label = config
            .map(selected_label)
            .unwrap_or_else(|| "配置读取失败".into());
        self.selected = config.and_then(selected_id).map(str::to_owned);
        #[cfg(windows)]
        {
            self.benchmark_model = config.and_then(crate::benchmark::BenchmarkModel::selected);
        }
    }

    pub(super) fn close(&mut self) {
        self.expanded = false;
        self.error = None;
    }

    pub(super) fn switching_notice(&self, inline: bool) -> Option<&str> {
        if inline
            && (self.download.is_some()
                || self
                    .switching_model
                    .as_deref()
                    .is_some_and(|id| self.has_model(id)))
        {
            return None;
        }
        if !inline
            && self
                .loading
                .as_ref()
                .is_some_and(|load| load.request.is_cancelled())
        {
            return self.loading_label();
        }
        self.switching.as_deref()
    }

    fn has_model(&self, id: &str) -> bool {
        self.models.iter().any(|model| model.id == id)
    }

    pub(super) fn has_selected_row(&self) -> bool {
        self.selected_model().is_some_and(|id| self.has_model(id))
    }

    pub(super) fn selected_model(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub(super) fn model_label(&self) -> &str {
        &self.label
    }

    pub(super) fn selected_model_loaded(&self) -> bool {
        self.selected_model()
            .is_some_and(|id| self.loaded_model.as_deref() == Some(id))
    }

    pub(super) fn note_model_loaded(&mut self) {
        self.loaded_model.clone_from(&self.selected);
    }

    pub(super) fn note_model_unloaded(&mut self) {
        self.loaded_model = None;
    }

    pub(super) fn guide_local_model_status(&self) -> &'static str {
        if self.preload_error.is_some() || self.error.is_some() {
            "加载失败，可重试"
        } else if self.selected_model_loaded() {
            "已加载"
        } else if self.has_selected_row() {
            "已安装，使用时加载"
        } else if self.listing {
            "正在查找本地模型"
        } else if self.selected.is_some() {
            "未找到所选模型"
        } else {
            "待选择"
        }
    }

    pub(super) fn download_notice(&self) -> Option<String> {
        let download = self.download.as_ref()?;
        let percent = download.percent();
        Some(if download.is_cancelled() {
            "正在取消下载…".into()
        } else if percent == 100 {
            "正在校验并启动 Whisper Base…".into()
        } else {
            format!("正在下载 Whisper Base · {percent}%")
        })
    }

    pub(super) fn preload_notice(&self) -> Option<&str> {
        if self.preloading {
            Some("正在预加载本机识别模型，可以先录音。")
        } else {
            self.preload_error.as_deref()
        }
    }

    #[cfg(windows)]
    pub(super) fn benchmark_model(&self) -> Option<&crate::benchmark::BenchmarkModel> {
        self.benchmark_model.as_ref()
    }
}

impl Speaky {
    pub(super) fn begin_model_loading(&mut self, id: String) -> Arc<ModelLoad> {
        self.model_picker.load_feedback = None;
        let request = Arc::new(ModelLoad::default());
        self.model_picker.loading = Some(ModelLoading {
            id,
            motion_started: crate::onboarding::motion_allowed().then(Instant::now),
            request: request.clone(),
        });
        request
    }

    pub(super) fn stop_model_loading_button(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let load = self.model_picker.loading.as_ref()?;
        if !load.request.is_loading() {
            return None;
        }
        let request = load.request.clone();
        Some(
            crate::redesign_config::action("停止加载")
                .id("stop-model-loading")
                .focusable()
                .tab_stop(true)
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    if request.cancel() {
                        cx.notify();
                    }
                })),
        )
    }

    pub(crate) fn local_model_switching(&self) -> bool {
        self.model_picker.switching.is_some()
    }

    pub(crate) fn refresh_model_selection(&mut self) {
        let previously_selected = self.model_picker.selected.clone();
        self.model_picker
            .read_selection(Config::load().ok().as_ref());
        if self.model_picker.selected != previously_selected {
            self.model_picker.preload_error = None;
            self.model_picker.unload_feedback = None;
            self.model_picker.load_feedback = None;
            self.model_picker.note_model_unloaded();
        }
        self.model_picker.expanded = false;
    }

    pub(super) fn preload_selected_model(&mut self, config: Config, cx: &mut Context<Self>) {
        let Some(id) = selected_id(&config).filter(|id| !id.trim().is_empty()) else {
            return;
        };
        if !config.desktop.preload_local_model
            || self.model_picker.preloading
            || self.settings.recognition_checking()
            || self.local_model_switching()
        {
            return;
        }
        let expected_model = id.to_owned();
        let load = self.begin_model_loading(expected_model.clone());
        self.model_picker.preloading = true;
        self.model_picker.preload_error = None;
        self.model_picker.unload_feedback = None;
        let worker = load.clone();
        let task = cx.background_spawn(async move {
            crate::engines::recognition_engine_cancellable(&config, &worker).map(|_| ())
        });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.model_picker.preloading = false;
                view.model_picker.finish_loading();
                if load.is_cancelled() {
                    view.model_picker.load_feedback = Some(stopped_model_load_message(&result));
                    cx.notify();
                    return;
                }
                if result.is_ok() && view.model_picker.selected_model() == Some(&expected_model) {
                    view.model_picker.note_model_loaded();
                }
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
        if self.starting
            || self.recording.is_some()
            || self.transcribing
            || self.model_picker.pending_previews != 0
        {
            Some("录音或转写结束后可切换模型")
        } else if self.model_benchmark_running() {
            Some("本条录音识别结束后可切换模型")
        } else if self.settings.recognition_checking() {
            Some("识别服务检查结束后可切换模型")
        } else if self.model_picker.preloading {
            Some("预加载结束后可切换模型")
        } else {
            None
        }
    }

    fn model_benchmark_running(&self) -> bool {
        #[cfg(windows)]
        {
            self.benchmark.is_running()
        }
        #[cfg(not(windows))]
        {
            false
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
        self.scan_models(cx);
    }

    pub(super) fn scan_models(&mut self, cx: &mut Context<Self>) {
        if !self.model_picker.listing {
            self.model_picker.listing = true;
            self.model_picker.error = None;
            let scan = cx.background_spawn(async {
                (available_models(), model_packs::whisper_runtime().is_some())
            });
            cx.spawn(async move |entity, cx| {
                let (result, can_download_whisper) = scan.await;
                let _ = entity.update(cx, |view, cx| {
                    let picker = &mut view.model_picker;
                    picker.listing = false;
                    picker.can_download_whisper = can_download_whisper;
                    match result {
                        Ok(models) => picker.models = models,
                        Err(error) if picker.expanded || view.page == Page::Models => {
                            picker.models.clear();
                            picker.error = Some(error);
                        }
                        Err(_) => {}
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn choose_model(&mut self, model: Candidate, cx: &mut Context<Self>) {
        if self.model_picker_busy()
            || (!self.model_picker.expanded && self.page != Page::Models)
            || self.model_picker.listing
        {
            return;
        }
        let notice = format!("正在加载 {}…", model.label);
        self.settings.clear_model_test_feedback();
        self.model_picker.unload_feedback = None;
        let load = self.begin_model_loading(model.id.clone());
        self.model_picker.switching_model = Some(model.id.clone());
        self.activate_model(notice, None, move || activate(model, &load), cx);
    }

    fn download_whisper(&mut self, cx: &mut Context<Self>) {
        if self.model_picker_busy()
            || (!self.model_picker.expanded && self.page != Page::Models)
            || self.model_picker.listing
        {
            return;
        }
        let download = Arc::new(model_packs::WhisperDownload::default());
        self.settings.clear_model_test_feedback();
        self.model_picker.unload_feedback = None;
        let worker = download.clone();
        self.activate_model(
            "正在下载 Whisper Base…".into(),
            Some(download.clone()),
            move || {
                let pack = model_packs::download_whisper(&worker)?;
                activate_pack(&pack, None)
            },
            cx,
        );
        let executor = cx.background_executor().clone();
        cx.spawn(async move |entity, cx| loop {
            executor.timer(Duration::from_millis(250)).await;
            let active = entity.update(cx, |view, cx| {
                if !view
                    .model_picker
                    .download
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &download))
                {
                    return false;
                }
                cx.notify();
                true
            });
            if !matches!(active, Ok(true)) {
                break;
            }
        })
        .detach();
    }

    fn activate_model(
        &mut self,
        notice: String,
        download: Option<Arc<model_packs::WhisperDownload>>,
        prepare: impl FnOnce() -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if download.is_none() {
            self.model_picker.close();
        }
        self.redesign_config.try_download = false;
        self.model_picker.error = None;
        self.model_picker.note_model_unloaded();
        self.model_picker.switching = Some(notice);
        self.model_picker.download = download.clone();
        let task = cx.background_spawn(async move { prepare() });
        cx.spawn(async move |entity, cx| {
            let result = task.await;
            let _ = entity.update(cx, |view, cx| {
                let stopped = view
                    .model_picker
                    .loading
                    .as_ref()
                    .is_some_and(|load| load.request.is_cancelled());
                view.model_picker.switching = None;
                view.model_picker.switching_model = None;
                view.model_picker.finish_loading();
                view.model_picker.download = None;
                view.refresh_model_selection();
                if stopped {
                    view.model_picker.load_feedback = Some(stopped_model_load_message(&result));
                } else if let Err(error) = result {
                    view.model_picker.error =
                        Some(if download.as_ref().is_some_and(|job| job.is_cancelled()) {
                            "已取消下载。".into()
                        } else if download.is_some() {
                            format!("Whisper Base 安装失败：{error}")
                        } else {
                            format!("切换失败：{error}")
                        });
                    view.model_picker.expanded = matches!(view.page, Page::Home | Page::History);
                    #[cfg(windows)]
                    if view.page == Page::Benchmark {
                        view.model_picker.expanded = true;
                    }
                } else {
                    view.model_picker.note_model_loaded();
                    if download.is_some() {
                        view.redesign_config.try_download = true;
                    }
                    if matches!(view.page, Page::Models | Page::Settings) {
                        view.reload_settings();
                    }
                    if view.page == Page::Models && download.is_some() {
                        view.scan_models(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn model_load_notice(&self, inline: bool, cx: &mut Context<Self>) -> Option<Div> {
        let picker = &self.model_picker;
        if picker.download.is_some() || (inline && picker.has_loading_row()) {
            return None;
        }
        let current = if picker
            .loading
            .as_ref()
            .is_some_and(|load| load.request.is_cancelled())
        {
            picker.loading_label()
        } else {
            picker
                .switching_notice(inline)
                .or_else(|| picker.loading_label())
        };
        let label = current.or(picker.load_feedback.as_deref())?;
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .child(crate::settings::detail(label.to_owned()))
                .when(current.is_some(), |row| {
                    row.children(self.stop_model_loading_button(cx))
                }),
        )
    }

    pub(super) fn v30_local_rows(&self, cx: &mut Context<Self>) -> Div {
        use crate::{
            redesign_config::{action, loading_notice, model_row},
            settings::{detail, ModelTestTarget},
        };
        let picker = &self.model_picker;
        let busy = self.model_picker_busy() || picker.listing;
        let mut rows = div().children(self.model_load_notice(true, cx));
        if picker.listing && picker.models.is_empty() {
            rows = rows.child(div().p_3().child(detail("正在查找本机语音模型…")));
        } else if picker.models.is_empty() && picker.error.is_none() {
            rows = rows.child(div().p_3().child(detail("尚未发现已安装的本地模型。")));
        }
        if let Some(err) = &picker.error {
            rows = rows.child(
                div()
                    .p_3()
                    .text_color(rgb(crate::ERROR()))
                    .child(err.clone()),
            );
        }
        for (index, model) in picker.models.iter().enumerate() {
            let selected = picker.selected.as_deref() == Some(&model.id);
            let worker = if model.pack.is_some() {
                "离线模型包"
            } else {
                "本机 GPU"
            };
            let mut actions = div().flex().flex_col().gap_1();
            if selected {
                let target = model.id.clone();
                let can_unload = !busy && self.can_release_local_model();
                let testing = self.settings.testing_local_model(&model.id);
                let unloading = picker.switching_model.as_deref() == Some(model.id.as_str())
                    && picker.loading.is_none();
                let unload_label = match (unloading, &picker.unload_feedback) {
                    (true, _) => "卸载中…",
                    (false, Some(Ok(_))) => "已卸载",
                    (false, Some(Err(_))) => "卸载失败，重试",
                    _ => "卸载模型",
                };
                actions = actions.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            action(if testing { "测试中…" } else { "测试" })
                                .id("catalog-test-local")
                                .when(!busy, |b| {
                                    b.on_click(cx.listener(|v, _, _, cx| {
                                        cx.stop_propagation();
                                        v.check_recognition_service(cx);
                                    }))
                                }),
                        )
                        .child(
                            action(unload_label)
                                .id("catalog-unload-local")
                                .when_some(picker.unload_feedback.clone(), |button, result| {
                                    let message = match result {
                                        Ok(message) | Err(message) => message,
                                    };
                                    button.tooltip(move |_, cx| {
                                        cx.new(|_| ActionTooltip(message.clone().into())).into()
                                    })
                                })
                                .when(!can_unload, |b| b.opacity(0.55))
                                .when(can_unload, |b| {
                                    b.on_click(cx.listener(move |v, _, _, cx| {
                                        cx.stop_propagation();
                                        v.release_local_model(Some(target.clone()), cx);
                                    }))
                                }),
                        ),
                );
            }
            if let Some(load) = picker.loading.as_ref().filter(|load| load.id == model.id) {
                let motion = load.motion_started.filter(|_| load.request.is_loading());
                let pulse = motion.map_or(0., |start| {
                    let phase = (start.elapsed().as_secs_f32() % 0.95) / 0.95;
                    (phase * std::f32::consts::PI).cos().powi(2)
                });
                actions = actions.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .children(
                            picker
                                .loading_label()
                                .map(|label| loading_notice(label, pulse)),
                        )
                        .children(self.stop_model_loading_button(cx)),
                );
            }
            let choice = model.clone();
            rows = rows.child(
                model_row(
                    model.label.clone(),
                    true,
                    selected,
                    if selected { "已选择" } else { "已安装" },
                    worker,
                    "离线听写",
                    self.settings
                        .model_tests
                        .get(&ModelTestTarget::Local(Some(model.id.clone())))
                        .filter(|_| selected),
                    actions,
                )
                .id(("catalog-local", index))
                .relative()
                .when(
                    selected || (index == 0 && !picker.has_selected_row()),
                    |row| row.children(self.tour.mark(crate::onboarding::Anchor::ModelChoice)),
                )
                .when(!busy && !selected, |row| {
                    row.cursor_pointer()
                        .focusable()
                        .tab_stop(true)
                        .hover(|style| style.bg(rgb(SIDEBAR())))
                        .on_click(
                            cx.listener(move |v, _, _, cx| v.choose_model(choice.clone(), cx)),
                        )
                }),
            );
        }
        if picker.download.is_some() {
            rows = rows.children(self.model_picker_panel(cx));
        } else if !picker.listing
            && picker.can_download_whisper
            && !picker
                .models
                .iter()
                .any(|m| m.pack.as_ref().is_some_and(model_packs::is_managed_whisper))
        {
            rows = rows.child(model_row(
                "Whisper Base".into(),
                true,
                false,
                "可下载",
                "147.95 MB · 下载后自动校验",
                "多语听写",
                None,
                div().child(
                    action("↓ 下载")
                        .id("catalog-download-whisper")
                        .relative()
                        .when(picker.models.is_empty(), |button| {
                            button.children(self.tour.mark(crate::onboarding::Anchor::ModelChoice))
                        })
                        .when(!busy, |b| {
                            b.on_click(cx.listener(|v, _, _, cx| v.download_whisper(cx)))
                        }),
                ),
            ));
        }
        rows
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
            .border_color(rgb(if picker.expanded { ACCENT() } else { BORDER() }))
            .bg(rgb(SIDEBAR()))
            .text_size(px(12.))
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(MUTED()))
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
                    .text_color(rgb(INK()))
                    .child(picker.label.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(ACCENT()))
                    .child(if picker.expanded { "▴" } else { "▾" }),
            )
            .tooltip(move |_, cx| cx.new(|_| ActionTooltip(tooltip.clone().into())).into())
            .when(busy, |button| button.opacity(0.65))
            .when(!busy, |button| {
                button
                    .when(self.studio_controls_active(), |button| {
                        button
                            .focusable()
                            .tab_stop(true)
                            .focus(|style| style.border_color(rgb(ACCENT())))
                    })
                    .cursor_pointer()
                    .hover(|style| style.border_color(rgb(ACCENT())))
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_model_picker(cx)))
            })
    }

    pub(super) fn model_picker_panel(&self, cx: &mut Context<Self>) -> Option<Div> {
        let picker = &self.model_picker;
        if !picker.expanded && picker.error.is_none() && picker.download.is_none() {
            return None;
        }
        let mut panel = div()
            .flex()
            .flex_col()
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER()))
            .bg(rgb(SIDEBAR()));
        if let Some(download) = &picker.download {
            let percent = download.percent();
            panel = panel.child(div().px_2().py_1().children(picker.download_notice()));
            if percent < 100 && !download.is_cancelled() {
                let download = download.clone();
                panel = panel.child(
                    super::button("取消下载", false)
                        .id("cancel-whisper-download")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            download.cancel();
                            cx.notify();
                        })),
                );
            }
            return Some(panel);
        }
        panel = panel.child(
            super::button("配置识别服务…", false)
                .id("open-service-settings")
                .on_click(
                    cx.listener(|view, _, window, cx| view.open_service_settings(window, cx)),
                ),
        );
        if let Some(error) = &picker.error {
            panel = panel.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(crate::ERROR()))
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
                        .text_color(rgb(MUTED()))
                        .child("正在查找本地语音模型…"),
                ),
            );
        }
        if !picker.models.iter().any(|model| {
            model
                .pack
                .as_ref()
                .is_some_and(model_packs::is_managed_whisper)
        }) {
            if picker.can_download_whisper {
                panel = panel.child(
                    super::button("下载并启动 Whisper Base（148 MB）", false)
                        .id("download-whisper")
                        .focusable()
                        .tab_stop(true)
                        .on_click(cx.listener(|view, _, _, cx| view.download_whisper(cx))),
                );
            } else if picker.models.is_empty() {
                panel = panel.child(div().px_2().py_1().text_color(rgb(MUTED())).child(
                    "需要包含 Whisper 推理程序的 Speaky 发布包，或本机已安装 whisper-server。",
                ));
            }
        }
        if picker.models.is_empty() && picker.error.is_none() && !picker.can_download_whisper {
            return Some(
                panel.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(rgb(MUTED()))
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
                            .h(px(34.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .cursor_pointer()
                            .when(selected, |row| {
                                row.bg(rgba(crate::theme::rgba(ACCENT(), 0x25)))
                            })
                            .hover(|style| style.bg(rgba(crate::theme::rgba(ACCENT(), 0x30))))
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
                                        .text_color(rgb(ACCENT()))
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

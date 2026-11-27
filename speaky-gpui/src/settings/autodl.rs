use super::{detail, Config, Preference, Speaky};
use crate::{
    button,
    redesign_config::{action, model_row},
};
use gpui::{div, prelude::*, px, Context, Div, Window};
use speaky_core::{autodl, config::LlmOpenAIConfig, polish};
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct RewriteCatalog {
    models: Vec<String>,
    listing: Option<uuid::Uuid>,
    notice: Option<String>,
    testing: Option<String>,
}

impl RewriteCatalog {
    pub(super) fn clear_listing(&mut self) {
        self.models.clear();
        self.listing = None;
        self.notice = None;
    }
}

impl Speaky {
    pub(super) fn edit_autodl_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_key = self
            .settings
            .config
            .as_ref()
            .is_some_and(|config| config.llm.autodl_key().is_some());
        match crate::service_ui::edit_autodl_key(window, has_key) {
            Ok(receiver) => cx
                .spawn(async move |entity, cx| {
                    let Ok(result) = receiver.await else { return };
                    let _ = entity.update(cx, |view, cx| match result {
                        Ok(Some(key)) if key.is_empty() && has_key => {}
                        Ok(Some(key)) => {
                            if let Err(error) =
                                view.update_preference(Preference::AutodlKey(key), cx)
                            {
                                view.settings.notice = Some(Err(error));
                            } else {
                                view.settings.notice = Some(Ok("已保存 AutoDL API Key。".into()));
                                if view.redesign_config.rewrite {
                                    view.refresh_autodl_rewrite_models(cx);
                                }
                            }
                            cx.notify();
                        }
                        Ok(None) => {}
                        Err(error) => {
                            view.settings.notice = Some(Err(error));
                            cx.notify();
                        }
                    });
                })
                .detach(),
            Err(error) => {
                self.settings.notice = Some(Err(error));
                cx.notify();
            }
        }
    }

    fn refresh_autodl_rewrite_models(&mut self, cx: &mut Context<Self>) {
        if self.settings.autodl_catalog.listing.is_some() {
            return;
        }
        let Some(key) = self
            .settings
            .config
            .as_ref()
            .and_then(|c| c.llm.autodl_key())
            .map(str::to_owned)
        else {
            return;
        };
        let request = uuid::Uuid::new_v4();
        self.settings.autodl_catalog.listing = Some(request);
        self.settings.autodl_catalog.notice = None;
        let task = cx.background_spawn(async move {
            let models = autodl::list_models(&key);
            (key, models)
        });
        cx.spawn(async move |entity, cx| {
            let (key, result) = task.await;
            let _ = entity.update(cx, |view, cx| {
                let catalog = &mut view.settings.autodl_catalog;
                if catalog.listing != Some(request) {
                    return;
                }
                catalog.listing = None;
                if view
                    .settings
                    .config
                    .as_ref()
                    .and_then(|c| c.llm.autodl_key())
                    == Some(key.as_str())
                {
                    match result {
                        Ok(models) => catalog.models = models,
                        Err(error) => {
                            catalog.notice = Some(format!("无法刷新 AutoDL 模型：{error}"))
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn test_autodl_rewrite(&mut self, model: String, activate: bool, cx: &mut Context<Self>) {
        if self.settings.polish_checking
            || self.starting
            || self.recording.is_some()
            || self.transcribing
        {
            return;
        }
        let Some(key) = self
            .settings
            .config
            .as_ref()
            .and_then(|c| c.llm.autodl_key())
            .map(str::to_owned)
        else {
            return;
        };
        self.settings.polish_checking = true;
        self.settings.autodl_catalog.testing = Some(model.clone());
        self.settings.polish_report = None;
        self.settings.notice = None;
        let task = cx.background_spawn(async move {
            let service = LlmOpenAIConfig {
                base_url: autodl::BASE_URL.into(),
                model,
                api_key: key,
            };
            let text = polish::organize_with_prompt(
                &service,
                &polish::Profile::Balanced.prompt(),
                "嗯，记一下，明天下午两点开会。",
            );
            (service, text)
        });
        cx.spawn(async move |entity, cx| {
            let (service, result) = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.settings.polish_checking = false;
                view.settings.autodl_catalog.testing = None;
                view.settings.notice = Some(result.and_then(|text| {
                    if activate {
                        view.update_preference(
                            Preference::AutodlRewrite {
                                model: service.model.clone(),
                                checked_key: service.api_key,
                            },
                            cx,
                        )?;
                    } else if view
                        .settings
                        .config
                        .as_ref()
                        .and_then(|c| c.llm.autodl_key())
                        != Some(service.api_key.as_str())
                    {
                        return Err("AutoDL 密钥已变更，请重新测试模型。".into());
                    }
                    let status = if activate {
                        "已启用"
                    } else {
                        "测试通过"
                    };
                    Ok(format!(
                        "AutoDL · {} {status}。样本文字：{text}",
                        service.model
                    ))
                }));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn autodl_rewrite_row(
        &self,
        config: &Config,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let catalog = &self.settings.autodl_catalog;
        let has_key = config.llm.autodl_key().is_some();
        let service = config.llm.organizer_service();
        let active = autodl::is_base_url(&service.base_url).then_some(service.model.as_str());
        let selected = config.llm.enabled && active.is_some();
        let editable = !busy && !self.settings.polish_checking;
        let key_label = if has_key {
            "修改 API Key"
        } else {
            "配置 API Key"
        };
        let mut controls = div().flex().flex_wrap().items_center().gap_1().child(
            action(key_label)
                .id("autodl-rewrite-key")
                .when(editable, |b| {
                    b.on_click(cx.listener(|v, _, window, cx| v.edit_autodl_key(window, cx)))
                }),
        );
        if has_key {
            controls = controls.child(
                action(if catalog.listing.is_some() {
                    "正在获取…"
                } else {
                    "刷新模型"
                })
                .id("autodl-rewrite-refresh")
                .when(editable && catalog.listing.is_none(), |b| {
                    b.on_click(cx.listener(|v, _, _, cx| v.refresh_autodl_rewrite_models(cx)))
                }),
            );
            if let Some(model) = active {
                let model = model.to_owned();
                controls = controls.child(action("测试当前模型").id("autodl-rewrite-test").when(
                    editable,
                    |b| {
                        b.on_click(cx.listener(move |v, _, _, cx| {
                            v.test_autodl_rewrite(model.clone(), false, cx);
                        }))
                    },
                ));
            }
        }
        let mut seen = HashSet::new();
        let models = active
            .into_iter()
            .chain(autodl::TEXT_MODELS.iter().copied())
            .chain(catalog.models.iter().map(String::as_str))
            .filter(|model| !model.is_empty() && seen.insert(*model));
        let options = div()
            .id("autodl-rewrite-models")
            .flex()
            .flex_wrap()
            .gap_1()
            .max_h(px(150.))
            .overflow_y_scroll()
            .children(models.enumerate().map(|(index, model)| {
                let current = active == Some(model);
                let model = model.to_owned();
                button(model.clone(), current)
                    .id(("autodl-rewrite-model", index))
                    .when(editable && has_key && !(current && selected), |b| {
                        b.focusable()
                            .tab_stop(true)
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.test_autodl_rewrite(model.clone(), true, cx);
                            }))
                    })
            }));
        let subtitle = if has_key {
            "与语音转写共用 API Key。选模型后会用内置文字测试。"
        } else {
            "配置一次 AutoDL API Key，即可用于语音转写和 AI 润色。"
        };
        model_row(
            "AutoDL".into(),
            false,
            selected,
            if selected {
                "已选择"
            } else if has_key {
                "已配置"
            } else {
                "待配置"
            },
            subtitle,
            "文字润色",
            None,
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(controls)
                .child(options)
                .when_some(catalog.testing.as_deref(), |row, model| {
                    row.child(detail(format!("正在测试 {model}…")))
                })
                .when_some(catalog.notice.as_deref(), |row, notice| {
                    row.child(detail(notice.to_owned()))
                }),
        )
    }
}

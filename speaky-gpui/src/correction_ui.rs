use super::{button, card, drafts, history, terms, Page, Speaky, BORDER, MUTED};
use gpui::{div, prelude::*, rgb, Context, Div, ElementId, FontWeight, Stateful, Task, Window};
use std::{collections::HashSet, ops::Range, time::Duration};
use terms::{Replacement, ReplacementUndo};
use uuid::Uuid;

pub(super) const RULES_LABEL: &str = "加入词表 · 勾选后，今后自动替换";
pub(super) const NO_RULES: &str = "可选中改好的完整词组来生成规则，也可在下方手动填写。";
pub(super) const MANUAL_RULE_LABEL: &str = "手动加入词表（选填，随本次保存）";

pub(super) fn manual_replacement(
    heard: &str,
    preferred: &str,
) -> Result<Option<Replacement>, &'static str> {
    match (heard.trim(), preferred.trim()) {
        ("", "") => Ok(None),
        ("", _) | (_, "") => Err("请同时填写识别错词和正确写法。"),
        (heard, preferred) => Replacement::from_pair(heard, preferred)
            .map(Some)
            .ok_or("请填写两个不同的词，且不要包含换行或 =>。"),
    }
}

pub(super) fn append_manual_replacement(rules: &mut Vec<Replacement>, manual: Option<Replacement>) {
    if let Some(rule) = manual {
        rules.retain(|candidate| candidate.original != rule.original);
        rules.push(rule);
    }
}

pub(super) struct CorrectionAnswer {
    pub text: String,
    pub rules: Vec<Replacement>,
}

pub(super) type CorrectionResult =
    tokio::sync::oneshot::Receiver<Result<Option<CorrectionAnswer>, String>>;

pub(super) struct RulePreview {
    original: String,
    pub text: String,
    selection: Option<Range<usize>>,
    included: HashSet<Replacement>,
    pub rules: Vec<Replacement>,
}

impl RulePreview {
    pub(super) fn new(original: &str) -> Self {
        Self {
            original: original.to_owned(),
            text: String::new(),
            selection: None,
            included: HashSet::new(),
            rules: Vec::new(),
        }
    }

    // Keep choices for exact pairs, even when a pair temporarily disappears.
    pub(super) fn update(
        &mut self,
        text: String,
        selection: Option<Range<usize>>,
        checked: impl Fn(usize) -> bool,
    ) -> bool {
        if text == self.text && selection == self.selection {
            return false;
        }
        let rules = terms::suggest_replacements(&self.original, &text, selection.clone());
        self.text = text;
        self.selection = selection;
        if rules == self.rules {
            return false;
        }
        for (index, rule) in self.rules.drain(..).enumerate() {
            if checked(index) {
                self.included.insert(rule);
            } else {
                self.included.remove(&rule);
            }
        }
        self.rules = rules;
        true
    }

    pub(super) fn included(&self, rule: &Replacement) -> bool {
        self.included.contains(rule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_rule_is_optional_and_overrides_a_suggestion_for_the_same_word() {
        assert_eq!(manual_replacement("", " "), Ok(None));
        assert!(manual_replacement("扣德克斯", "").is_err());
        assert!(manual_replacement("codex", "codex").is_err());
        let mut rules = vec![Replacement::from_pair("扣德克斯", "Codex").unwrap()];
        let manual = manual_replacement(" 扣德克斯 ", "codex").unwrap();
        append_manual_replacement(&mut rules, manual);
        assert_eq!(
            rules,
            [Replacement::from_pair("扣德克斯", "codex").unwrap()]
        );
    }

    #[test]
    fn rules_start_unchecked_and_keep_explicit_choices() {
        let original = "我们用 red 开发 agent 工具";
        let edited = "我们用 RAG 开发 Agent 工具";
        let mut preview = RulePreview::new(original);
        assert!(preview.update(edited.into(), None, |_| true));
        assert_eq!(preview.rules.len(), 2);
        assert!(preview.rules.iter().all(|rule| !preview.included(rule)));
        assert!(preview.update(original.into(), None, |index| index == 0));
        assert!(preview.rules.is_empty());
        assert!(preview.update(edited.into(), None, |_| true));
        assert!(preview.included(&preview.rules[0]));
        assert!(!preview.included(&preview.rules[1]));
        assert!(preview.update(original.into(), None, |index| index == 1));
        assert!(preview.update(edited.into(), None, |_| true));
        assert!(!preview.included(&preview.rules[0]));
        assert!(preview.included(&preview.rules[1]));
    }
}

struct Edit {
    target: history::Target,
    original: String,
    corrected: String,
}

struct RuleBatch {
    id: Uuid,
    rules: Vec<Replacement>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RuleAction {
    Undo(Uuid),
    Retry(Uuid),
}

#[derive(Default)]
pub(super) struct CorrectionEditor {
    pub(super) busy: bool,
    unsaved: Option<Edit>,
    pending_rules: Vec<RuleBatch>,
    undo: Option<(Uuid, ReplacementUndo)>,
    rule_notice: Option<(String, RuleAction)>,
    success_expiry: Option<Task<()>>,
}

impl Speaky {
    pub(super) fn correction_button(
        &self,
        id: impl Into<ElementId>,
        target: history::Target,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        #[cfg(any(windows, target_os = "macos"))]
        if !self.transcribing
            && self.recording.is_none()
            && !self.correction.busy
            && self.unsaved.is_none()
        {
            // The home result may be an automatic rewrite. Edit the ASR transcript.
            let text = match &target {
                history::Target::Draft(path) => self
                    .archive
                    .iter()
                    .find(|entry| &entry.path == path)
                    .and_then(|entry| match &entry.state {
                        drafts::State::Transcribed { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .unwrap_or(text)
                    .to_owned(),
                _ => text.to_owned(),
            };
            return Some(button("改错词", false).id(id).on_click(cx.listener(
                move |view, _, window, cx| {
                    view.open_correction(target.clone(), text.clone(), window, cx);
                },
            )));
        }
        None
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn open_correction(
        &mut self,
        target: history::Target,
        original: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.correction.busy || self.transcribing || self.recording.is_some() {
            return;
        }
        let page = self.page;
        let current = self
            .correction
            .unsaved
            .as_ref()
            .filter(|entry| entry.target == target && entry.original == original)
            .map_or(original.as_str(), |entry| entry.corrected.as_str());
        #[cfg(windows)]
        let editor = crate::windows_dialog::edit_correction(window, &original, current);
        #[cfg(target_os = "macos")]
        let editor = crate::terms_ui::edit_correction(window, &original, current);
        let receiver = match editor {
            Ok(receiver) => receiver,
            Err(error) => {
                self.set_feedback(page, format!("无法打开改错词窗口：{error}"));
                cx.notify();
                return;
            }
        };
        self.correction.busy = true;
        cx.notify();
        cx.spawn(async move |entity, cx| {
            let answer = receiver
                .await
                .unwrap_or_else(|_| Err("编辑窗口已关闭。".into()));
            let _ = entity.update(cx, |view, cx| {
                view.correction.busy = false;
                match answer {
                    Ok(Some(answer)) => {
                        let CorrectionAnswer {
                            text: corrected,
                            rules,
                        } = answer;
                        if corrected.trim().is_empty() {
                            view.set_feedback(page, "转写内容不能为空，已保留原文。");
                        } else if corrected != original {
                            view.save_correction(
                                Edit {
                                    target,
                                    original,
                                    corrected,
                                },
                                rules,
                                page,
                                cx,
                            );
                        } else if !rules.is_empty() {
                            view.add_correction_rules(
                                RuleBatch {
                                    id: Uuid::new_v4(),
                                    rules,
                                },
                                page,
                                cx,
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(error) => view.set_feedback(page, error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_correction(
        &mut self,
        edit: Edit,
        rules: Vec<Replacement>,
        page: Page,
        cx: &mut Context<Self>,
    ) {
        self.correction.busy = true;
        let task = cx.background_spawn(async move {
            let saved = history::edit(&edit.target, &edit.original, &edit.corrected);
            (edit, saved)
        });
        cx.spawn(async move |entity, cx| {
            let (edit, result) = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.correction.busy = false;
                view.correction.rule_notice = None;
                match result {
                    Err(error) => {
                        view.correction.unsaved = Some(edit);
                        view.set_feedback(
                            page,
                            format!("转写保存失败：{error}。再次点「改错词」可重试。"),
                        );
                    }
                    Ok(()) => {
                        if view
                            .correction
                            .unsaved
                            .as_ref()
                            .is_some_and(|last| last.target == edit.target)
                        {
                            view.correction.unsaved = None;
                        }
                        view.archive = drafts::list();
                        if let Some((target, text)) = &mut view.result {
                            if *target == edit.target {
                                text.clone_from(&edit.corrected);
                            }
                        }
                        if rules.is_empty() {
                            view.show_correction_success(page, "已保存本次修改".into(), None, cx);
                        } else {
                            view.add_correction_rules(
                                RuleBatch {
                                    id: Uuid::new_v4(),
                                    rules,
                                },
                                page,
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_correction_rules(&mut self, batch: RuleBatch, page: Page, cx: &mut Context<Self>) {
        self.correction.busy = true;
        let task = cx.background_spawn(async move {
            let result = terms::add_replacements(&batch.rules);
            (batch, result)
        });
        cx.spawn(async move |entity, cx| {
            let (batch, result) = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.correction.busy = false;
                match result {
                    Ok(undo) if undo.count() > 0 => {
                        let message = format!("已添加 {} 条替换规则", undo.count());
                        view.correction.undo = Some((batch.id, undo));
                        view.show_correction_success(
                            page,
                            message,
                            Some(RuleAction::Undo(batch.id)),
                            cx,
                        );
                    }
                    Ok(_) => {
                        view.show_correction_success(
                            page,
                            "所选替换规则已在词典中".into(),
                            None,
                            cx,
                        );
                    }
                    Err(error) => {
                        log::warn!("Could not add correction rules: {error}");
                        let id = batch.id;
                        view.correction.pending_rules.push(batch);
                        view.set_rule_notice(
                            page,
                            "替换规则添加失败".into(),
                            RuleAction::Retry(id),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_rule_notice(&mut self, page: Page, message: String, action: RuleAction) {
        self.set_feedback(page, message.clone());
        self.correction.rule_notice = Some((message, action));
    }

    fn show_correction_success(
        &mut self,
        page: Page,
        message: String,
        action: Option<RuleAction>,
        cx: &mut Context<Self>,
    ) {
        self.set_feedback(page, message.clone());
        self.correction.rule_notice = action.map(|action| (message.clone(), action));
        let timer = cx.background_executor().timer(Duration::from_secs(3));
        self.correction.success_expiry = Some(cx.spawn(async move |entity, cx| {
            timer.await;
            let _ = entity.update(cx, |view, cx| {
                view.correction
                    .rule_notice
                    .take_if(|(notice, _)| notice == &message);
                if view.feedback_for(page) == Some(message.as_str()) {
                    view.clear_feedback(page);
                    cx.notify();
                }
            });
        }));
    }

    fn perform_rule_action(&mut self, action: RuleAction, cx: &mut Context<Self>) {
        if self.correction.busy {
            return;
        }
        let page = self.page;
        match action {
            RuleAction::Retry(id) => {
                if let Some(index) = self
                    .correction
                    .pending_rules
                    .iter()
                    .position(|batch| batch.id == id)
                {
                    let batch = self.correction.pending_rules.remove(index);
                    self.add_correction_rules(batch, page, cx);
                }
            }
            RuleAction::Undo(id) => {
                let Some((_, undo)) = self
                    .correction
                    .undo
                    .take_if(|(saved_id, _)| *saved_id == id)
                else {
                    return;
                };
                self.correction.busy = true;
                let task = cx.background_spawn(async move {
                    let result = terms::undo_replacements(&undo);
                    (undo, result)
                });
                cx.spawn(async move |entity, cx| {
                    let (undo, result) = task.await;
                    let _ = entity.update(cx, |view, cx| {
                        view.correction.busy = false;
                        match result {
                            Ok(()) => {
                                view.show_correction_success(
                                    page,
                                    "已撤销词典规则".into(),
                                    None,
                                    cx,
                                );
                            }
                            Err(error) => {
                                view.correction.undo = Some((id, undo));
                                view.set_rule_notice(
                                    page,
                                    format!("撤销词典规则失败：{error}"),
                                    action,
                                );
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn rule_action_button(
        &self,
        action: RuleAction,
        id: impl Into<ElementId>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = match action {
            RuleAction::Undo(_) => "撤销词典规则",
            RuleAction::Retry(_) => "重试添加规则",
        };
        button(label, false)
            .id(id)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.perform_rule_action(action, cx);
            }))
    }

    pub(super) fn correction_notice_action(
        &self,
        status: &str,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let (message, action) = self.correction.rule_notice.as_ref()?;
        (!self.correction.busy && message == status)
            .then(|| self.rule_action_button(*action, "correction-notice-action", cx))
    }

    pub(super) fn dictionary_correction_actions(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.correction.busy {
            return None;
        }
        // Keep failed writes and the latest undo accessible after navigating away.
        let visible = self
            .correction
            .rule_notice
            .as_ref()
            .filter(|(message, _)| {
                self.unsaved.is_none() && self.feedback_for(Page::Terms) == Some(message.as_str())
            })
            .map(|(_, action)| *action);
        let undo = self
            .correction
            .undo
            .as_ref()
            .map(|(id, _)| RuleAction::Undo(*id))
            .filter(|action| Some(*action) != visible);
        let mut pending = self
            .correction
            .pending_rules
            .iter()
            .enumerate()
            .filter(|(_, batch)| Some(RuleAction::Retry(batch.id)) != visible)
            .peekable();
        if undo.is_none() && pending.peek().is_none() {
            return None;
        }
        let mut panel = card().flex().flex_col().gap_2();
        if let Some(action) = undo {
            panel = panel.child(div().child(self.rule_action_button(
                action,
                "dictionary-undo-rules",
                cx,
            )));
        }
        if pending.peek().is_some() {
            panel = panel.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("待添加的替换规则"),
            );
        }
        for (index, batch) in pending {
            let rules = batch
                .rules
                .iter()
                .map(|rule| format!("{} → {}", rule.original, rule.preferred))
                .collect::<Vec<_>>()
                .join("、");
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().flex_1().min_w(gpui::px(0.)).child(rules))
                    .child(self.rule_action_button(
                        RuleAction::Retry(batch.id),
                        ("dictionary-retry-rules", index),
                        cx,
                    )),
            );
        }
        Some(panel)
    }

    pub(super) fn learned_terms_card(&self, cx: &mut Context<Self>) -> Div {
        let panel = card().flex().flex_col().gap_2().child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child("已学习的常用词"),
        );
        let words = match terms::learned() {
            Ok(words) => words,
            Err(error) => {
                return panel.child(
                    div()
                        .text_color(rgb(crate::ERROR()))
                        .child(format!("无法读取自动添加的词：{error}")),
                );
            }
        };
        if words.is_empty() {
            return panel.child(
                div()
                    .text_color(rgb(MUTED()))
                    .child("暂无已学习的识别提示词。"),
            );
        }
        panel.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .children(words.into_iter().enumerate().map(|(index, word)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .justify_between()
                        .border_t_1()
                        .border_color(rgb(BORDER()))
                        .py_1()
                        .child(word.clone())
                        .child(button("移除", false).id(("forget-term", index)).on_click(
                            cx.listener(move |view, _, _, cx| {
                                let message = match terms::forget_learned(&word) {
                                    Ok(()) => format!("已从自动添加的词中移除「{word}」。"),
                                    Err(error) => format!("移除失败：{error}"),
                                };
                                view.set_feedback(Page::Terms, message);
                                cx.notify();
                            }),
                        ))
                })),
        )
    }
}

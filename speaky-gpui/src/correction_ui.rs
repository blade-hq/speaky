use super::{button, card, drafts, history, terms, Speaky, BORDER, MUTED};
use gpui::{div, prelude::*, rgb, Context, Div, ElementId, FontWeight, Stateful, Window};
use std::{io, ops::Range};

struct Edit {
    target: history::Target,
    original: String,
    corrected: String,
    selection: Option<Range<usize>>,
}

impl Edit {
    fn learn(&self) -> io::Result<Vec<String>> {
        terms::learn_edits_with_selection(&self.original, &self.corrected, self.selection.clone())
    }
}

#[derive(Default)]
pub(super) struct CorrectionEditor {
    pub(super) busy: bool,
    unsaved: Option<Edit>,
    // Only retained if saving the transcript succeeds but writing the vocabulary fails.
    pending_learning: Vec<Edit>,
}

fn saved_notice(words: &[String]) -> String {
    if words.is_empty() {
        return "已保存转写。".into();
    }
    format!("已保存转写，常用词已自动添加：{}。", words.join("、"))
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
            let text = text.to_owned();
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
        let current = self
            .correction
            .unsaved
            .as_ref()
            .filter(|entry| entry.target == target && entry.original == original)
            .map_or(original.as_str(), |entry| entry.corrected.as_str());
        #[cfg(windows)]
        let editor = crate::windows_dialog::edit_correction(window, current);
        #[cfg(target_os = "macos")]
        let editor = crate::terms_ui::edit_text(
            window,
            current,
            "改错词",
            "直接修改转写，保存后会自动记住改过的词。",
        );
        let receiver = match editor {
            Ok(receiver) => receiver,
            Err(error) => {
                self.status = format!("无法打开改错词窗口：{error}");
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
                        #[cfg(windows)]
                        let (corrected, selection) = answer;
                        #[cfg(target_os = "macos")]
                        let (corrected, selection) = (answer, None);
                        if corrected.trim().is_empty() {
                            view.status = "转写内容不能为空，已保留原文。".into();
                        } else if corrected != original {
                            view.save_correction(
                                Edit {
                                    target,
                                    original,
                                    corrected,
                                    selection,
                                },
                                cx,
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(error) => view.status = error,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_correction(&mut self, edit: Edit, cx: &mut Context<Self>) {
        self.correction.busy = true;
        let task = cx.background_spawn(async move {
            // Text is authoritative even if the separate dictionary cannot be written.
            let saved = history::edit(&edit.target, &edit.original, &edit.corrected);
            let learned = saved.map(|()| edit.learn());
            (edit, learned)
        });
        cx.spawn(async move |entity, cx| {
            let (edit, result) = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.correction.busy = false;
                match result {
                    Err(error) => {
                        view.correction.unsaved = Some(edit);
                        view.status = format!("转写保存失败：{error}。再次点「改错词」可重试。");
                    }
                    Ok(learned) => {
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
                        view.status = match learned {
                            Ok(words) => saved_notice(&words),
                            Err(error) => {
                                view.correction.pending_learning.push(edit);
                                format!("转写已保存，词典写入失败：{error}。可在「常用词」中重试。")
                            }
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn learned_terms_card(&self, cx: &mut Context<Self>) -> Div {
        let mut panel = card()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("自动添加"));
        if !self.correction.pending_learning.is_empty() {
            panel = panel.child(button("重试添加", false).id("retry-learning").on_click(
                cx.listener(|view, _, _, cx| {
                    let mut failure = None;
                    view.correction
                        .pending_learning
                        .retain(|edit| match edit.learn() {
                            Ok(_) => false,
                            Err(error) => {
                                failure = Some(error);
                                true
                            }
                        });
                    view.status = failure.map_or_else(
                        || "已更新自动添加的词。".into(),
                        |e| format!("词典写入失败：{e}"),
                    );
                    cx.notify();
                }),
            ));
        }
        let words = match terms::learned() {
            Ok(words) => words,
            Err(error) => {
                return panel.child(
                    div()
                        .text_color(rgb(crate::ERROR))
                        .child(format!("无法读取自动添加的词：{error}")),
                );
            }
        };
        if words.is_empty() {
            return panel.child(
                div()
                    .text_color(rgb(MUTED))
                    .child("在转写旁点「改错词」。保存后，从修改中学到的词会出现在这里。"),
            );
        }
        panel.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(words.into_iter().enumerate().map(|(index, word)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(BORDER))
                        .pl_2()
                        .pr_1()
                        .py_1()
                        .child(word.clone())
                        .child(button("移除", false).id(("forget-term", index)).on_click(
                            cx.listener(move |view, _, _, cx| {
                                view.status = match terms::forget_learned(&word) {
                                    Ok(()) => format!("已从自动添加的词中移除「{word}」。"),
                                    Err(error) => format!("移除失败：{error}"),
                                };
                                cx.notify();
                            }),
                        ))
                })),
        )
    }
}

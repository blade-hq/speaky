mod input;

use super::{
    button, card, drafts, history, icon_button, playback, settings::detail, terms, Anchor, Icon,
    Page, RecordingDestination, RecordingStart, Speaky, ACCENT, BORDER, ERROR, MUTED, SIDEBAR,
    SURFACE,
};
use crate::redesign_home::local_stamp;
use gpui::{
    div, prelude::*, px, rgb, ClipboardItem, Context, Div, Entity, FontWeight, ScrollAnchor,
    ScrollHandle, Window,
};
use input::Input;
use std::{
    cell::OnceCell,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Pending,
    Rewritten,
    Original,
}

impl Filter {
    fn matches(self, pending: bool, rewritten: bool) -> bool {
        match self {
            Self::All => true,
            Self::Pending => pending,
            Self::Rewritten => rewritten,
            Self::Original => !pending && !rewritten,
        }
    }
}

#[derive(Default)]
pub(super) struct RedesignLibrary {
    filter: Filter,
    expanded: Option<history::Target>,
    pub(super) scroll: ScrollHandle,
    jump_anchor: Option<ScrollAnchor>,
    search: OnceCell<Entity<Input>>,
    heard: OnceCell<Entity<Input>>,
    preferred: OnceCell<Entity<Input>>,
    terms_copy: Option<Result<Instant, String>>,
}

fn field(
    cell: &OnceCell<Entity<Input>>,
    placeholder: &'static str,
    cx: &mut Context<Speaky>,
) -> Entity<Input> {
    cell.get_or_init(|| {
        let input = cx.new(|cx| Input::new(placeholder, cx));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        input
    })
    .clone()
}

fn title(text: &'static str) -> Div {
    div()
        .text_size(px(26.))
        .line_height(px(34.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

fn date_group(text: String) -> Div {
    div()
        .pt_3()
        .pb_1()
        .text_size(px(12.))
        .text_color(rgb(MUTED()))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

pub(super) fn current_rewrite<'a>(
    entry: &'a drafts::Entry,
    source: Option<&str>,
) -> Option<&'a str> {
    entry
        .organized
        .as_ref()
        .filter(|result| Some(result.source.as_str()) == source && !result.text.trim().is_empty())
        .map(|result| result.text.as_str())
}

fn matches_query(query: &str, fields: &[&str]) -> bool {
    query.is_empty()
        || fields
            .iter()
            .any(|value| value.to_lowercase().contains(query))
}

fn export_wav(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::{fs, io};
    let source = fs::canonicalize(source)?;
    if fs::canonicalize(destination).is_ok_and(|selected| selected == source) {
        return Err(io::Error::other("请选择与原始录音不同的位置。"));
    }
    let mut audio = fs::File::open(source)?;
    let temporary = destination.with_file_name(format!(".speaky-{}.tmp", uuid::Uuid::new_v4()));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let copied = io::copy(&mut audio, &mut output).and_then(|_| output.sync_all());
    drop(output);
    let saved = copied.and_then(|_| fs::rename(&temporary, destination));
    if saved.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    saved
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_protects_the_original_and_replaces_a_selected_destination() {
        let dir = std::env::temp_dir().join(format!("speaky-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join("record.wav");
        let target = dir.join("copy.wav");
        std::fs::write(&source, b"complete audio").unwrap();
        std::fs::write(&target, b"old copy").unwrap();
        assert!(export_wav(&source, &source).is_err());
        export_wav(&source, &target).unwrap();
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(&target).unwrap()
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

impl Speaky {
    pub(super) fn show_library_target(
        &mut self,
        target: history::Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let library = &mut self.redesign_library;
        library.filter = Filter::All;
        if let Some(input) = library.search.get() {
            input.update(cx, |field, cx| field.clear(cx));
        }
        library.expanded = Some(target);
        let anchor = ScrollAnchor::for_handle(library.scroll.clone());
        library.jump_anchor = Some(anchor.clone());
        self.open_page(super::Page::History, cx);
        window.on_next_frame(move |window, cx| {
            anchor.scroll_to(window, cx);
            window.request_animation_frame();
        });
    }
    pub(super) fn v30_history(&self, cx: &mut Context<Self>) -> Div {
        let state = &self.redesign_library;
        let search = field(&state.search, "搜索历史记录…", cx);
        let query = search.read(cx).text.trim().to_lowercase();
        let mut body = div().flex().flex_col().gap_2().child(title("历史记录"));
        let filters = div()
            .flex()
            .gap_1()
            .p_1()
            .rounded(px(9.))
            .bg(rgb(SIDEBAR()))
            .children(
                [
                    (Filter::All, "全部"),
                    (Filter::Pending, "待转写"),
                    (Filter::Rewritten, "已改写"),
                    (Filter::Original, "仅原文"),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, (filter, label))| {
                    let active = filter == state.filter;
                    button(label, false)
                        .id(("library-filter", i))
                        .h(px(27.))
                        .px_2()
                        .when(active, |b| {
                            b.bg(rgb(SURFACE()))
                                .text_color(rgb(ACCENT()))
                                .font_weight(FontWeight::SEMIBOLD)
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.redesign_library.filter = filter;
                            cx.notify();
                        }))
                }),
            );
        let clear = search.clone();
        body = body.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .justify_between()
                .child(filters)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().w(px(185.)).child(search))
                        .when(!query.is_empty(), |row| {
                            row.child(icon_button(Icon::Close, "清除搜索").on_click(cx.listener(
                                move |_, _, _, cx| {
                                    clear.update(cx, |input, cx| input.clear(cx));
                                },
                            )))
                        }),
                ),
        );
        if self.transcribing {
            body = body.child(detail("正在转写…"));
        }
        let today = local_stamp(SystemTime::now()).map(|date| date.serial);
        let mut last_group: Option<String> = None;
        let mut count = 0usize;
        let mut ordered: Vec<_> = self.archive.iter().collect();
        ordered.sort_by_key(|entry| std::cmp::Reverse(entry.recorded_at.or(entry.modified)));
        for (index, entry) in ordered.into_iter().enumerate() {
            let source = match &entry.state {
                drafts::State::Transcribed { text } => Some(text.as_str()),
                _ => None,
            };
            let rewritten = current_rewrite(entry, source);
            if !state.filter.matches(source.is_none(), rewritten.is_some()) {
                continue;
            }
            let (group, time, day) = entry
                .recorded_at
                .or(entry.modified)
                .and_then(local_stamp)
                .map(|stamp| {
                    let day = format!("{:04}-{:02}-{:02}", stamp.year, stamp.month, stamp.day);
                    let group = match today.map(|today| today - stamp.serial) {
                        Some(0) => "今天".to_owned(),
                        Some(1) => "昨天".to_owned(),
                        _ => day.clone(),
                    };
                    (group, format!("{:02}:{:02}", stamp.hour, stamp.minute), day)
                })
                .unwrap_or_else(|| ("日期未知".into(), "—".into(), String::new()));
            let preview = rewritten.or(source).unwrap_or(match entry.state {
                drafts::State::Cancelled => "转录已取消，原音已保留。",
                _ => "原音已保存，等待转写。",
            });
            if !matches_query(
                &query,
                &[
                    preview,
                    source.unwrap_or_default(),
                    entry.original_text.as_deref().unwrap_or_default(),
                    &group,
                    &day,
                    &time,
                ],
            ) {
                continue;
            }
            if last_group.as_ref() != Some(&group) {
                body = body.child(date_group(group.clone()));
                last_group = Some(group);
            }
            body = body.child(self.library_recording(
                index,
                entry,
                time,
                preview,
                source,
                count == 0,
                cx,
            ));
            count += 1;
        }
        if state.filter.matches(false, false) {
            let mut older_header = false;
            for (index, (target, text)) in
                history::standalone(&self.archive).into_iter().enumerate()
            {
                if !matches_query(&query, &[&text, "过往转写"]) {
                    continue;
                }
                if !older_header {
                    body = body.child(date_group("过往转写".into()));
                    older_header = true;
                }
                let expanded = state.expanded.as_ref() == Some(&target);
                let copy = text.clone();
                let target_to_open = target.clone();
                body = body.child(
                    div()
                        .id(("legacy-library-row", index))
                        .rounded(px(8.))
                        .when(expanded, |r| r.anchor_scroll(state.jump_anchor.clone()))
                        .bg(rgb(SURFACE()))
                        .px_3()
                        .py_1()
                        .when(count == 0, |r| {
                            r.relative().children(self.tour.mark(Anchor::History))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .id(("legacy-history-text", index))
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .cursor_pointer()
                                        .child(text.clone())
                                        .on_click(cx.listener(move |v, _, _, cx| {
                                            v.toggle_library_row(&target_to_open, cx)
                                        })),
                                )
                                .child(
                                    icon_button(Icon::Copy, "复制文本")
                                        .id(("legacy-copy", index))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy.clone(),
                                            ));
                                        })),
                                ),
                        )
                        .when(expanded, |r| {
                            r.child(
                                div()
                                    .py_2()
                                    .child(detail("历史文本"))
                                    .child(text.clone())
                                    .children(self.correction_button(
                                        ("legacy-correction", index),
                                        target,
                                        &text,
                                        cx,
                                    )),
                            )
                        }),
                );
                count += 1;
            }
        }
        if count == 0 {
            body = body.child(
                card()
                    .relative()
                    .children(self.tour.mark(Anchor::History))
                    .py_5()
                    .text_color(rgb(MUTED()))
                    .child(if !query.is_empty() {
                        "没有找到匹配的历史记录。"
                    } else if state.filter != Filter::All {
                        "此分类中还没有记录。"
                    } else {
                        "还没有录音。停止录音后，可在这里回听和查看转写。"
                    }),
            );
        }
        body
    }

    fn toggle_library_row(&mut self, target: &history::Target, cx: &mut Context<Self>) {
        self.redesign_library.jump_anchor = None;
        self.redesign_library.expanded = if self.redesign_library.expanded.as_ref() == Some(target)
        {
            None
        } else {
            Some(target.clone())
        };
        cx.notify();
    }

    fn library_recording(
        &self,
        index: usize,
        entry: &drafts::Entry,
        time: String,
        preview: &str,
        source: Option<&str>,
        first: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let target = history::Target::Draft(entry.path.clone());
        let expanded = self.redesign_library.expanded.as_ref() == Some(&target);
        let details = target.clone();
        let runnable = self.recording.is_none()
            && !self.starting
            && !self.transcribing
            && self.unsaved.is_none()
            && !self.recognition_model_busy();
        let play = entry.path.clone();
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(38.))
            .child(
                div()
                    .w(px(45.))
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED()))
                    .child(time),
            )
            .child(
                div()
                    .id(("library-expand", index))
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .cursor_pointer()
                    .when(source.is_none(), |t| t.text_color(rgb(MUTED())))
                    .child(preview.to_owned())
                    .on_click(
                        cx.listener(move |view, _, _, cx| view.toggle_library_row(&target, cx)),
                    ),
            );
        if let Some(text) = source {
            let copy = current_rewrite(entry, source).unwrap_or(text).to_owned();
            row = row.child(
                div()
                    .flex()
                    .gap_1()
                    .when(!expanded, |actions| {
                        actions
                            .invisible()
                            .group_hover("history-row", |s| s.visible())
                    })
                    .child(
                        icon_button(Icon::Copy, "复制文本")
                            .id(("library-copy", index))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            })),
                    )
                    .when(
                        self.recording.is_none() && self.unsaved.is_none(),
                        |actions| {
                            actions.child(
                                icon_button(Icon::Play, "播放原音")
                                    .id(("library-play", index))
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        v.play_library_audio(&play, cx)
                                    })),
                            )
                        },
                    ),
            );
        } else if runnable {
            let path = entry.path.clone();
            row = row.child(button("重试", false).id(("library-retry", index)).on_click(
                cx.listener(move |v, _, _, cx| {
                    v.transcribe(path.clone(), RecordingDestination::Archive, cx)
                }),
            ));
        }
        row = row.child(
            button(if expanded { "收起" } else { "···" }, false)
                .id(("library-more", index))
                .w(px(35.))
                .px_0()
                .on_click(cx.listener(move |v, _, _, cx| v.toggle_library_row(&details, cx))),
        );
        let mut card = div()
            .id(("library-recording-row", index))
            .group("history-row")
            .when(expanded, |r| {
                r.anchor_scroll(self.redesign_library.jump_anchor.clone())
            })
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(BORDER()))
            .bg(rgb(SURFACE()))
            .px_3()
            .when(first, |r| {
                r.relative().children(self.tour.mark(Anchor::History))
            })
            .child(row);
        if !expanded {
            return card;
        }
        let mut panel = div()
            .pb_3()
            .pt_2()
            .border_t_1()
            .border_color(rgb(BORDER()))
            .flex()
            .flex_col()
            .gap_2();
        panel = panel.child(detail(format!(
            "原始录音 · {}:{:02}{}",
            entry.duration_seconds / 60,
            entry.duration_seconds % 60,
            entry
                .recognizer
                .as_ref()
                .map(|name| format!(" · {name}"))
                .unwrap_or_default()
        )));
        let mut actions = div().flex().items_center().gap_1().flex_wrap();
        if source.is_none() && self.recording.is_none() && self.unsaved.is_none() {
            let play = entry.path.clone();
            actions = actions.child(
                button("播放原音", false)
                    .id(("library-pending-play", index))
                    .on_click(cx.listener(move |v, _, _, cx| v.play_library_audio(&play, cx))),
            );
        }
        if runnable {
            let path = entry.path.clone();
            if source.is_some() {
                actions = actions.child(
                    button("重新转写", false)
                        .id(("library-redo", index))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.transcribe(path.clone(), RecordingDestination::Archive, cx)
                        })),
                );
            } else {
                actions = actions.child(
                    button("继续录音", false)
                        .id(("library-resume", index))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.begin_recording(path.clone(), RecordingStart::Resume, cx)
                        })),
                );
            }
            let path = entry.path.clone();
            actions = actions.child(
                button("测试模型", false)
                    .id(("library-compare", index))
                    .on_click(
                        cx.listener(move |v, _, _, cx| v.compare_recording(path.clone(), cx)),
                    ),
            );
        }
        if let Some(text) = source {
            let original = text.to_owned();
            let path = entry.path.clone();
            actions = actions
                .children(self.correction_button(
                    ("library-edit", index),
                    history::Target::Draft(path.clone()),
                    text,
                    cx,
                ))
                .when(self.organizer_enabled, |r| {
                    r.child(
                        button("整理", false)
                            .id(("library-organize", index))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.organize_transcript(path.clone(), original.clone(), cx)
                            })),
                    )
                });
        }
        let reveal = entry.path.clone();
        let export = reveal.clone();
        actions = actions
            .child(
                button("下载音频", false)
                    .id(("library-export", index))
                    .on_click(
                        cx.listener(move |v, _, _, cx| v.export_library_audio(export.clone(), cx)),
                    ),
            )
            .child(
                icon_button(Icon::Folder, "打开录音所在位置")
                    .id(("library-folder", index))
                    .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&reveal))),
            );
        panel = panel.child(actions).when_some(
            self.transcript_feedback_for(&entry.path),
            |panel, message| panel.child(detail(message.to_owned())),
        );
        if let Some(text) = source {
            let original = entry.original_text.as_deref().unwrap_or(text);
            panel = panel
                .child(self.organized_result(entry, text, cx))
                .when(original != text, |p| {
                    p.child(detail("当前转写")).child(text.to_owned())
                })
                .child(detail("ASR 原文"))
                .child(original.to_owned());
        }
        panel = panel.children(entry.comparisons.iter().map(|comparison| {
            div()
                .rounded_md()
                .bg(rgb(SIDEBAR()))
                .p_2()
                .child(detail(comparison.model.clone()))
                .child(comparison.text.clone())
        }));
        card = card.child(panel);
        card
    }

    fn play_library_audio(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.recording.is_some() || self.unsaved.is_some() {
            return;
        }
        self.stop_playback();
        match playback::Playback::start(path) {
            Ok(player) => {
                self.playback = Some(player);
                self.clear_feedback(Page::History);
            }
            Err(error) => self.set_feedback(Page::History, format!("播放失败：{error}")),
        }
        cx.notify();
    }

    fn export_library_audio(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Speaky.wav");
        let selected = cx.prompt_for_new_path(path.parent().unwrap_or(Path::new(".")), Some(name));
        cx.spawn(async move |this, cx| {
            let result = match selected.await {
                Ok(Ok(Some(to))) => {
                    cx.background_spawn(async move {
                        export_wav(&path, &to).map_err(|e| e.to_string())?;
                        Ok(format!("已导出音频至 {}", to.display()))
                    })
                    .await
                }
                Ok(Ok(None)) => return,
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => return,
            };
            let _ = this.update(cx, |v, cx| {
                v.set_feedback(
                    Page::History,
                    result.unwrap_or_else(|error| format!("导出失败：{error}")),
                );
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_terms(&mut self, cx: &mut Context<Self>) {
        match terms::load() {
            Ok(source) => {
                cx.write_to_clipboard(ClipboardItem::new_string(source));
                let copied_at = Instant::now();
                self.redesign_library.terms_copy = Some(Ok(copied_at));
                let timeout = cx.background_executor().timer(Duration::from_secs(2));
                cx.spawn(async move |entity, cx| {
                    timeout.await;
                    let _ = entity.update(cx, |view, cx| {
                        if matches!(&view.redesign_library.terms_copy, Some(Ok(time)) if *time == copied_at) {
                            view.redesign_library.terms_copy = None;
                            cx.notify();
                        }
                    });
                }).detach();
            }
            Err(error) => {
                self.redesign_library.terms_copy = Some(Err(format!("复制词表失败：{error}")));
            }
        }
        cx.notify();
    }

    pub(super) fn v30_terms(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let heard = field(&self.redesign_library.heard, "识别错词（可选）", cx);
        let preferred = field(&self.redesign_library.preferred, "正确写法", cx);
        let mut manual = card()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("常用词与替换规则"),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().min_w(px(0.)).child(heard.clone()))
                    .child(div().flex_1().min_w(px(0.)).child(preferred.clone()))
                    .child(
                        button("添加", true)
                            .id("library-add-rule")
                            .on_click(cx.listener(move |v, _, _, cx| {
                                if v.correction.busy {
                                    return;
                                }
                                let from = heard.read(cx).text.trim().to_owned();
                                let to = preferred.read(cx).text.trim().to_owned();
                                if to.is_empty() || from.contains("=>") || to.contains("=>") {
                                    v.set_feedback(
                                        Page::Terms,
                                        "请输入正确写法；替换符请留给程序自动添加。",
                                    );
                                    cx.notify();
                                    return;
                                }
                                let rule = if from.is_empty() {
                                    to
                                } else {
                                    format!("{from} => {to}")
                                };
                                let result = terms::load().and_then(|mut source| {
                                    if source.lines().any(|line| line.trim() == rule) {
                                        return Err(std::io::Error::other("词表中已有此项。"));
                                    }
                                    if !source.is_empty() && !source.ends_with('\n') {
                                        source.push('\n');
                                    }
                                    source.push_str(&rule);
                                    source.push('\n');
                                    terms::save(&source)
                                });
                                match result {
                                    Ok(()) => {
                                        heard.update(cx, |i, cx| i.clear(cx));
                                        preferred.update(cx, |i, cx| i.clear(cx));
                                        v.set_feedback(Page::Terms, "已添加到词典。");
                                    }
                                    Err(error) => {
                                        v.set_feedback(Page::Terms, format!("添加失败：{error}"));
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            );
        match terms::load() {
            Ok(source) => {
                if source.trim().is_empty() {
                    manual = manual.child(detail("还没有手动添加的词。"));
                }
                for (i, line) in source
                    .lines()
                    .enumerate()
                    .filter(|(_, l)| !l.trim().is_empty())
                {
                    let value = line.to_owned();
                    let label = line
                        .split_once("=>")
                        .map(|(a, b)| format!("{}　→　{}", a.trim(), b.trim()))
                        .unwrap_or_else(|| line.trim().to_owned());
                    manual = manual.child(
                        div()
                            .border_t_1()
                            .border_color(rgb(BORDER()))
                            .py_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(label)
                            .child(
                                button("移除", false)
                                    .id(("library-remove-term", i))
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        if v.correction.busy {
                                            return;
                                        }
                                        let result = terms::load().and_then(|source| {
                                            let mut lines: Vec<_> = source.lines().collect();
                                            if lines.get(i).copied() != Some(value.as_str()) {
                                                return Err(std::io::Error::other(
                                                    "词表已更新，请重试。",
                                                ));
                                            }
                                            lines.remove(i);
                                            terms::save(&lines.join("\n"))
                                        });
                                        let message = result
                                            .map(|_| "已移除词条。".to_owned())
                                            .unwrap_or_else(|e| format!("移除失败：{e}"));
                                        v.set_feedback(Page::Terms, message);
                                        cx.notify();
                                    })),
                            ),
                    );
                }
            }
            Err(e) => {
                manual = manual.child(
                    div()
                        .text_color(rgb(ERROR()))
                        .child(format!("无法读取词表：{e}")),
                )
            }
        }
        let mut top = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().child(title("词典")));
        #[cfg(any(windows, target_os = "macos"))]
        {
            top = top.child(
                button("批量编辑", false)
                    .id("library-bulk-edit")
                    .on_click(cx.listener(|v, _, window, cx| v.edit_terms(window, cx))),
            );
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            top = top.child(
                button("粘贴并保存", false)
                    .id("library-paste-terms")
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(source) = cx.read_from_clipboard().and_then(|item| item.text())
                        {
                            let message = match terms::save(&source) {
                                Ok(()) => "已保存词表。".into(),
                                Err(error) => format!("保存失败：{error}"),
                            };
                            view.set_feedback(Page::Terms, message);
                            cx.notify();
                        }
                    })),
            );
        }
        let copied = matches!(&self.redesign_library.terms_copy, Some(Ok(_)));
        top = top.child(
            button(if copied { "已复制" } else { "复制词表" }, false)
                .id("library-copy-terms")
                .on_click(cx.listener(|view, _, _, cx| view.copy_terms(cx))),
        );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(top)
            .when_some(
                self.redesign_library
                    .terms_copy
                    .as_ref()
                    .and_then(|result| result.as_ref().err()),
                |page, error| page.child(div().text_color(rgb(ERROR())).child(error.clone())),
            )
            .children(self.dictionary_correction_actions(cx))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_3()
                    .when(!wide, |r| r.flex_col())
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .when(wide, |r| r.flex_1())
                            .child(manual),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .when(wide, |r| r.flex_1())
                            .child(self.learned_terms_card(cx)),
                    ),
            )
    }
}

use crate::{ACCENT, BORDER, INK, MUTED, SELECTION, SURFACE};
use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, Bounds, ClipboardItem, Context,
    CursorStyle, ElementInputHandler, EntityInputHandler, FocusHandle, KeyDownEvent, MouseButton,
    Pixels, Point, ShapedLine, TextRun, UTF16Selection, Window,
};
use std::ops::Range;

/// A single-line GPUI editor. The platform IME supplies its composition ranges in UTF-16.
pub(super) struct Input {
    pub(super) text: String,
    placeholder: &'static str,
    focus: FocusHandle,
    anchor: usize,
    cursor: usize,
    marked: Option<Range<usize>>,
    layout: Option<(ShapedLine, Point<Pixels>)>,
    dragging: bool,
}

fn byte_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        if units >= utf16 {
            return index;
        }
        units += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    #[test]
    fn ime_utf16_offsets_follow_scalar_boundaries_in_mixed_text() {
        let text = "中🎙A";
        assert_eq!(
            [0, 1, 2, 3, 4, 99].map(|index| super::byte_offset(text, index)),
            [0, 3, 7, 7, 8, 8]
        );
    }
}

impl Input {
    pub(super) fn new(placeholder: &'static str, cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            placeholder,
            focus: cx.focus_handle(),
            anchor: 0,
            cursor: 0,
            marked: None,
            layout: None,
            dragging: false,
        }
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.text.clear();
        self.anchor = 0;
        self.cursor = 0;
        self.marked = None;
        cx.notify();
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }
    fn utf16(&self, r: Range<usize>) -> Range<usize> {
        self.text[..r.start].encode_utf16().count()..self.text[..r.end].encode_utf16().count()
    }
    fn bytes(&self, r: Range<usize>) -> Range<usize> {
        byte_offset(&self.text, r.start)..byte_offset(&self.text, r.end)
    }
    fn edit(&mut self, range: Option<Range<usize>>, text: &str) -> Range<usize> {
        let range = range
            .map(|r| self.bytes(r))
            .or_else(|| self.marked.take())
            .unwrap_or_else(|| self.selection());
        let value = text.replace(['\r', '\n'], " ");
        self.text.replace_range(range.clone(), &value);
        self.cursor = range.start + value.len();
        self.anchor = self.cursor;
        self.marked = None;
        range.start..self.cursor
    }
    fn index_at(&self, p: Point<Pixels>) -> usize {
        self.layout
            .as_ref()
            .filter(|(line, _)| line.text.as_ref() == self.text.as_str())
            .map_or(self.text.len(), |(line, origin)| {
                line.closest_index_for_x(p.x - origin.x)
                    .min(self.text.len())
            })
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.is_some() {
            return;
        }
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let range = self.selection();
        let command = if cfg!(target_os = "macos") {
            keystroke.modifiers.platform
        } else {
            keystroke.modifiers.control
        };
        match (command, key) {
            (true, "a") => {
                self.anchor = 0;
                self.cursor = self.text.len();
            }
            (true, "v") => {
                if let Some(value) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.edit(None, &value);
                }
            }
            (true, "c" | "x") => {
                if !range.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(self.text[range].to_owned()));
                    if key == "x" {
                        self.edit(None, "");
                    }
                }
            }
            (false, "backspace" | "delete" | "left" | "right" | "home" | "end") => {
                let prev = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i);
                let next = self.text[self.cursor..]
                    .chars()
                    .next()
                    .map_or(self.cursor, |c| self.cursor + c.len_utf8());
                match key {
                    "backspace" | "delete" => {
                        if range.is_empty() {
                            self.anchor = if key == "backspace" { prev } else { next };
                        }
                        self.edit(None, "");
                    }
                    _ => {
                        self.cursor = match key {
                            "home" => 0,
                            "end" => self.text.len(),
                            "left" if !range.is_empty() && !keystroke.modifiers.shift => {
                                range.start
                            }
                            "right" if !range.is_empty() && !keystroke.modifiers.shift => range.end,
                            "left" => prev,
                            _ => next,
                        };
                        if !keystroke.modifiers.shift {
                            self.anchor = self.cursor;
                        }
                    }
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        window.prevent_default();
        cx.notify();
    }
}

impl EntityInputHandler for Input {
    fn text_for_range(
        &mut self,
        r: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.bytes(r);
        *actual = Some(self.utf16(range.clone()));
        Some(self.text[range].to_owned())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.utf16(self.selection()),
            reversed: self.cursor < self.anchor,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.clone().map(|r| self.utf16(r))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit(range, text);
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inserted = self.edit(range, text);
        if !inserted.is_empty() {
            if let Some(selection) = selected {
                let value = &self.text[inserted.clone()];
                self.anchor = inserted.start + byte_offset(value, selection.start);
                self.cursor = inserted.start + byte_offset(value, selection.end);
            }
            self.marked = Some(inserted);
        }
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.bytes(range);
        let (line, origin) = self.layout.as_ref()?;
        Some(Bounds::from_corners(
            point(origin.x + line.x_for_index(range.start), bounds.top()),
            point(origin.x + line.x_for_index(range.end), bounds.bottom()),
        ))
    }
    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.text[..self.index_at(p)].encode_utf16().count())
    }
}

impl Render for Input {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id("single-line-editor")
            .track_focus(&self.focus)
            .tab_stop(true)
            .cursor(CursorStyle::IBeam)
            .h(px(33.))
            .w_full()
            .min_w(px(0.))
            .px_2()
            .border_1()
            .border_color(rgb(BORDER()))
            .focus(|s| s.border_color(rgb(ACCENT())))
            .bg(rgb(SURFACE()))
            .rounded(px(8.))
            .flex()
            .items_center()
            .overflow_hidden()
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|input, e: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&input.focus);
                    input.cursor = input.index_at(e.position);
                    if !e.modifiers.shift {
                        input.anchor = input.cursor;
                    }
                    input.dragging = true;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|input, e: &gpui::MouseMoveEvent, _, cx| {
                if input.dragging {
                    input.cursor = input.index_at(e.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|input, _, _, _| input.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|input, _, _, _| input.dragging = false),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        let input = entity.read(cx);
                        let focus = input.focus.clone();
                        let (display, color) = if input.text.is_empty() {
                            (input.placeholder.to_owned(), MUTED())
                        } else {
                            (input.text.clone(), INK())
                        };
                        let style = window.text_style();
                        let run = TextRun {
                            len: display.len(),
                            font: style.font(),
                            color: rgb(color).into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let line =
                            window
                                .text_system()
                                .shape_line(display.into(), px(13.), &[run], None);
                        let caret = line.x_for_index(input.cursor);
                        let shift = (caret - bounds.size.width + px(2.)).max(px(0.));
                        let origin = point(bounds.left() - shift, bounds.top());
                        if focus.is_focused(window) {
                            let selection = input.selection();
                            if !selection.is_empty() {
                                let from =
                                    point(origin.x + line.x_for_index(selection.start), origin.y);
                                let to = point(
                                    origin.x + line.x_for_index(selection.end),
                                    bounds.bottom(),
                                );
                                window.paint_quad(fill(
                                    Bounds::from_corners(from, to),
                                    rgb(SELECTION()),
                                ));
                            }
                            if let Some(mark) = input.marked.as_ref() {
                                let from = point(
                                    origin.x + line.x_for_index(mark.start),
                                    bounds.bottom() - px(1.),
                                );
                                let to =
                                    point(origin.x + line.x_for_index(mark.end), bounds.bottom());
                                window.paint_quad(fill(
                                    Bounds::from_corners(from, to),
                                    rgb(ACCENT()),
                                ));
                            }
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(origin.x + caret, origin.y),
                                    size(px(1.), bounds.size.height),
                                ),
                                rgb(ACCENT()),
                            ));
                        }
                        let _ = line.paint(origin, bounds.size.height, window, cx);
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                        entity.update(cx, |input, _| input.layout = Some((line, origin)));
                    },
                )
                .w_full()
                .h(px(19.)),
            )
    }
}

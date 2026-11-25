use super::*;
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::UI::Controls::{EM_GETSEL, EM_SETSEL};
use windows_sys::Win32::UI::WindowsAndMessaging::{WM_GETTEXT, WM_GETTEXTLENGTH};

const EDIT_CHANGED: &str = "输入框内容或光标已改变，已停止向这里写入。";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    before: String,
    selected: String,
    after: String,
}

impl Selection {
    fn after_writing(&self, text: &str) -> Self {
        Self {
            before: [&self.before, text].concat(),
            selected: String::new(),
            after: self.after.clone(),
        }
    }

    fn with_selection(&self, selected: &str) -> Self {
        Self {
            selected: selected.to_owned(),
            ..self.clone()
        }
    }
}

// Keep the common text in place. When the new hypothesis is a strict prefix of
// the old one, rewrite the whole hypothesis so no simulated Delete key is needed.
fn revision<'a>(old: &'a str, new: &'a str) -> (&'a str, &'a str, &'a str) {
    let common = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let cut = if common == new.len() && old != new {
        0
    } else {
        common
    };
    (&old[..cut], &old[cut..], &new[cut..])
}

pub(super) struct Editor {
    focus: Focus,
    automation: Option<AutomationEditor>,
}

impl Editor {
    pub(super) fn new(focus: Focus, automation: Option<AutomationEditor>) -> Self {
        Self { focus, automation }
    }

    pub(super) fn require_focus(&self) -> Result<(), String> {
        if let Some(editor) = &self.automation {
            editor.require_focus(self.focus)
        } else {
            self.focus.require_current()
        }
    }

    fn writable(&self) -> Result<(), String> {
        self.require_focus()?;
        if let Some(editor) = &self.automation {
            editor.require_writable()
        } else if self.focus.is_native_edit() && self.focus.writable_native() {
            Ok(())
        } else {
            Err(UNSUPPORTED.into())
        }
    }

    fn snapshot(&self) -> Result<Selection, String> {
        self.writable()?;
        self.snapshot_contents()
    }

    fn snapshot_contents(&self) -> Result<Selection, String> {
        if let Some(editor) = &self.automation {
            unsafe {
                let pattern: IUIAutomationTextPattern =
                    readable(editor.field.GetCurrentPatternAs(UIA_TextPatternId))?;
                let range = editor.selection().ok_or(UNSUPPORTED)?;
                let before = readable(pattern.DocumentRange())?;
                let after = readable(pattern.DocumentRange())?;
                readable(before.MoveEndpointByRange(
                    TextPatternRangeEndpoint_End,
                    &range,
                    TextPatternRangeEndpoint_Start,
                ))?;
                readable(after.MoveEndpointByRange(
                    TextPatternRangeEndpoint_Start,
                    &range,
                    TextPatternRangeEndpoint_End,
                ))?;
                let mut selection = Selection {
                    before: contents(readable(before.GetText(-1))?)?,
                    selected: contents(readable(range.GetText(-1))?)?,
                    after: contents(readable(after.GetText(-1))?)?,
                };
                if editor.displays_placeholder(
                    &[&*selection.before, &*selection.selected, &*selection.after].concat(),
                ) {
                    selection.before.clear();
                    selection.selected.clear();
                    selection.after.clear();
                }
                return Ok(selection);
            }
        }
        let (buffer, a, b) = self.native_selection()?;
        let part = |s: &[u16]| {
            String::from_utf16(s)
                .map(|v| normalized_lines(&v))
                .map_err(|_| UNSUPPORTED.to_string())
        };
        Ok(Selection {
            before: part(&buffer[..a])?,
            selected: part(&buffer[a..b])?,
            after: part(&buffer[b..])?,
        })
    }

    fn native_selection(&self) -> Result<(Vec<u16>, usize, usize), String> {
        let count = self.focus.message(WM_GETTEXTLENGTH, 0, 0)?;
        // Large documents still support ordinary one-time insertion.
        if count > 4 * 1024 * 1024 {
            return Err(UNSUPPORTED.into());
        }
        let mut buffer = vec![0u16; count + 1];
        let count = self
            .focus
            .message(WM_GETTEXT, buffer.len(), buffer.as_mut_ptr() as isize)?;
        let (mut start, mut end) = (u32::MAX, u32::MAX);
        let packed = self.focus.message(
            EM_GETSEL,
            (&mut start as *mut u32) as usize,
            (&mut end as *mut u32) as isize,
        )?;
        // The packed result crosses process boundaries without depending on
        // optional out-pointer marshalling. Never mistake untouched pointers for caret 0.
        if packed as u32 != u32::MAX {
            start = (packed as u32) & 0xffff;
            end = ((packed as u32) >> 16) & 0xffff;
        } else if start == u32::MAX || end == u32::MAX {
            return Err(UNSUPPORTED.into());
        }
        let (a, b) = (start as usize, end as usize);
        if a > b || b > count || count >= buffer.len() {
            return Err(UNSUPPORTED.into());
        }
        buffer.truncate(count);
        Ok((buffer, a, b))
    }

    fn check_selection(&self, expected: &Selection) -> Result<(), String> {
        if self.snapshot()? == *expected {
            Ok(())
        } else {
            Err(EDIT_CHANGED.into())
        }
    }

    fn confirm_selection(&self, expected: &Selection, deadline: Instant) -> Result<(), String> {
        if self.automation.is_none() {
            return self.check_selection(expected);
        }
        // Chromium can publish the text and its new caret in separate UIA updates.
        // Only wait after an edit we initiated. A subsequent write still checks
        // the exact position immediately before sending any input.
        let until = deadline.min(Instant::now() + Duration::from_millis(450));
        loop {
            if self.snapshot()? == *expected {
                return Ok(());
            }
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(EDIT_CHANGED.into());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(20)));
        }
    }

    fn select_previous(
        &self,
        previous: &str,
        expected: &Selection,
        deadline: Instant,
    ) -> Result<(), String> {
        self.writable()?;
        if let Some(editor) = &self.automation {
            unsafe {
                let pattern: IUIAutomationTextPattern =
                    readable(editor.field.GetCurrentPatternAs(UIA_TextPatternId))?;
                let caret = editor.selection().ok_or(UNSUPPORTED)?;
                let prefix = readable(pattern.DocumentRange())?;
                readable(prefix.MoveEndpointByRange(
                    TextPatternRangeEndpoint_End,
                    &caret,
                    TextPatternRangeEndpoint_Start,
                ))?;
                // Providers count UTF-16, Unicode characters and CRLF differently.
                // Match the actual text immediately before the current caret.
                let range = prefix
                    .FindText(&BSTR::from(previous), true, false)
                    .or_else(|_| {
                        prefix.FindText(&BSTR::from(previous.replace('\n', "\r\n")), true, false)
                    });
                if let Ok(range) = range {
                    let exact = readable(range.CompareEndpoints(
                        TextPatternRangeEndpoint_End,
                        &caret,
                        TextPatternRangeEndpoint_Start,
                    ))? == 0
                        && contents(readable(range.GetText(-1))?)? == previous;
                    if exact {
                        editor.require_focus(self.focus)?;
                        if range.Select().is_ok()
                            && self.confirm_selection(expected, deadline).is_ok()
                        {
                            return Ok(());
                        }
                    }
                }
                if !editor.is_chromium() {
                    return Err(EDIT_CHANGED.into());
                }
                // A Chromium range may report a different caret or ignore Select.
                // Use the visible caret only if the entire prior snapshot is intact.
                editor.wait_for_input(self.focus, deadline)?;
                self.check_selection(&expected.after_writing(previous))?;
                editor.check_input(self.focus, deadline)?;
                let mut keys = vec![virtual_key(VK_SHIFT, 0)];
                for _ in previous.graphemes(true) {
                    keys.push(virtual_key(VK_LEFT, KEYEVENTF_EXTENDEDKEY));
                    keys.push(virtual_key(
                        VK_LEFT,
                        KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
                    ));
                }
                keys.push(virtual_key(VK_SHIFT, KEYEVENTF_KEYUP));
                send_inputs(&keys)?;
            }
        } else {
            let (buffer, caret, end) = self.native_selection()?;
            if caret != end {
                return Err(EDIT_CHANGED.into());
            }
            let before =
                String::from_utf16(&buffer[..caret]).map_err(|_| UNSUPPORTED.to_string())?;
            let width = ["\r\n", "\r", "\n"]
                .iter()
                .map(|ending| previous.replace('\n', ending))
                .find(|candidate| before.ends_with(candidate))
                .ok_or(EDIT_CHANGED)?
                .encode_utf16()
                .count();
            self.focus
                .message(EM_SETSEL, caret - width, caret as isize)?;
        }
        self.confirm_selection(expected, deadline)
    }

    fn write(
        &self,
        text: &str,
        deadline: Instant,
        expected: Option<&Selection>,
    ) -> Result<(), String> {
        self.writable()?;
        if Instant::now() >= deadline {
            return Err(UNSUPPORTED.into());
        }
        let check = || expected.map_or(Ok(()), |selection| self.check_selection(selection));
        if let Some(editor) = &self.automation {
            let after = expected.map(|selection| selection.after_writing(text));
            let verify = after
                .as_ref()
                .map(|after| move || self.check_selection(after).is_ok());
            return editor.insert(self.focus, text, deadline, check, verify);
        }
        let wide: Vec<_> = text
            .replace('\n', "\r\n")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        check()?;
        self.focus
            .message(EM_REPLACESEL, 1, wide.as_ptr() as isize)?;
        Ok(())
    }
}

pub(super) struct Composition {
    original: Option<Selection>,
    written: Option<String>,
    attempted: Option<String>,
    interrupted: Option<String>,
}

fn cancellation_span(
    current: &str,
    original: &Selection,
    attempted: Option<&str>,
    written: Option<&str>,
) -> Option<(usize, usize)> {
    let remaining = current.strip_prefix(&original.before)?;
    if written.is_some()
        && !original.selected.is_empty()
        && current == [&*original.before, &*original.after].concat()
    {
        // The selected original also belongs to the user. Restore it if an
        // acknowledged replacement has since been completely removed.
        return Some((original.before.len(), original.before.len()));
    }
    // A write can fail before changing the field at all. An attempted hypothesis
    // that happens to prefix the user's original text is not evidence of an edit.
    let attempted =
        attempted.filter(|text| current == [&*original.before, text, &*original.after].concat());
    [written, attempted]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .filter(|text| remaining.starts_with(text))
        .max_by_key(|text| text.len())
        .map(|text| (original.before.len(), original.before.len() + text.len()))
}

impl Composition {
    pub(super) fn cancel(&mut self, editor: &Editor) -> Result<(), String> {
        if self.written.is_none() && self.attempted.is_none() {
            return Ok(());
        }
        let original = self.original.clone().ok_or(EDIT_CHANGED)?;
        let current = editor.snapshot_contents()?;
        let document = [&*current.before, &*current.selected, &*current.after].concat();
        if document == [&*original.before, &*original.selected, &*original.after].concat() {
            self.written = None;
            self.attempted = None;
            return Ok(());
        }
        let (start, end) = cancellation_span(
            &document,
            &original,
            self.attempted.as_deref(),
            self.written.as_deref(),
        )
        .ok_or("听写预览已有手动修改，已保留修改；请检查输入框中的文字。")?;
        // No synthetic keys or focus changes: only restore an exact, known composition.
        if let Some(automation) = &editor.automation {
            automation.require_writable()?;
            let value = [&document[..start], &original.selected, &document[end..]].concat();
            unsafe {
                if let Ok(pattern) = automation
                    .field
                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                {
                    readable(pattern.SetValue(&BSTR::from(value)))?;
                } else {
                    // Text-only providers (including contenteditable) can clear their
                    // exact preview through a verified selection while still focused.
                    editor.require_focus()?;
                    let pattern: IUIAutomationTextPattern =
                        readable(automation.field.GetCurrentPatternAs(UIA_TextPatternId))?;
                    let range = readable(pattern.DocumentRange())?;
                    let range =
                        readable(range.FindText(&BSTR::from(&document[start..end]), false, false))?;
                    readable(range.Select())?;
                    let expected = Selection {
                        before: document[..start].into(),
                        selected: document[start..end].into(),
                        after: document[end..].into(),
                    };
                    editor.check_selection(&expected)?;
                    editor.require_focus()?;
                    let deadline = Instant::now() + Duration::from_secs(2);
                    if original.selected.is_empty() {
                        send_inputs(&[
                            virtual_key(VK_BACK, 0),
                            virtual_key(VK_BACK, KEYEVENTF_KEYUP),
                        ])?;
                    } else {
                        editor.write(&original.selected, deadline, Some(&expected))?;
                    }
                    editor
                        .confirm_selection(&expected.after_writing(&original.selected), deadline)?;
                }
            }
        } else {
            if !editor.focus.writable_native() {
                return Err(UNSUPPORTED.into());
            }
            let (buffer, caret, selection_end) = editor.native_selection()?;
            let actual = String::from_utf16(&buffer).map_err(|_| UNSUPPORTED)?;
            if normalized_lines(&actual) != document {
                return Err(EDIT_CHANGED.into());
            }
            let (native_start, native_end) = ["\r\n", "\r", "\n"]
                .iter()
                .find_map(|ending| {
                    let prefix = document[..start].replace('\n', ending);
                    let through = document[..end].replace('\n', ending);
                    actual.starts_with(&through).then_some((
                        prefix.encode_utf16().count(),
                        through.encode_utf16().count(),
                    ))
                })
                .ok_or(EDIT_CHANGED)?;
            editor
                .focus
                .message(EM_SETSEL, native_start, native_end as isize)?;
            let text: Vec<_> = original
                .selected
                .replace('\n', "\r\n")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            editor
                .focus
                .message(EM_REPLACESEL, 1, text.as_ptr() as isize)?;
            let new_end = native_start + text.len() - 1;
            let adjust = |offset: usize| {
                if offset <= native_start {
                    offset
                } else if offset < native_end {
                    new_end
                } else {
                    offset - native_end + new_end
                }
            };
            editor
                .focus
                .message(EM_SETSEL, adjust(caret), adjust(selection_end) as isize)?;
        }
        self.written = None;
        self.attempted = None;
        Ok(())
    }

    pub(super) fn capture(editor: &Editor) -> Self {
        Self {
            original: editor.snapshot().ok(),
            written: None,
            attempted: None,
            interrupted: None,
        }
    }

    fn update(&mut self, editor: &Editor, text: &str, deadline: Instant) -> Result<(), String> {
        let original = self.original.as_ref().ok_or(UNSUPPORTED)?;
        let (replacement, expected) = if let Some(previous) = &self.written {
            let caret = original.after_writing(previous);
            editor.check_selection(&caret)?;
            let (kept, replaced, added) = revision(previous, text);
            if replaced.is_empty() && added.is_empty() {
                return Ok(());
            }
            if !replaced.is_empty() {
                let selection = original.after_writing(kept).with_selection(replaced);
                editor.select_previous(replaced, &selection, deadline)?;
                (added, selection)
            } else {
                (added, caret)
            }
        } else {
            editor.check_selection(original)?;
            (text, original.clone())
        };
        self.attempted = Some(text.to_owned());
        editor.write(replacement, deadline, Some(&expected))?;
        if editor.automation.is_none() {
            // A native EM_REPLACESEL has acknowledged the edit. A user's next
            // keystroke can arrive before the caret snapshot is confirmed.
            self.written = self.attempted.take();
        }
        editor.confirm_selection(&original.after_writing(text), deadline)?;
        if let Some(attempted) = self.attempted.take() {
            self.written = Some(attempted);
        }
        Ok(())
    }

    pub(super) fn preview(&mut self, editor: &Editor, text: &str) -> Result<(), String> {
        if self.original.is_none() {
            return Err("无法读取此输入框的光标位置，将在录音结束后尝试写入。".into());
        }
        if self.interrupted.is_none() {
            if let Err(error) = self.update(editor, text, Instant::now() + Duration::from_secs(2)) {
                // Pause previews after an unconfirmed edit. The final write may
                // resume only if it can reconcile the full target snapshot.
                self.interrupted = Some(error);
            }
        }
        self.interrupted.clone().map_or(Ok(()), Err)
    }

    fn reconcile(&mut self, current: &Selection, original: Selection) -> bool {
        // An unacknowledged write may have left the old text or the entire
        // attempted update. Both need to match with their original surroundings.
        if let Some(attempted) = self
            .attempted
            .take()
            .filter(|attempted| *current == original.after_writing(attempted))
        {
            self.written = Some(attempted);
            return true;
        }
        self.written.as_ref().map_or_else(
            || *current == original,
            |written| *current == original.after_writing(written),
        )
    }

    pub(super) fn finish(
        &mut self,
        editor: &Editor,
        text: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        if let Some(error) = self.interrupted.take() {
            if error != UNCONFIRMED_WRITE && error != FOCUS_CHANGED {
                return Err(error);
            }
            let original = self.original.as_ref().ok_or_else(|| error.clone())?;
            let current = editor.snapshot()?;
            if !self.reconcile(&current, original.clone()) {
                return Err(error);
            }
        }
        if self.original.is_some() {
            self.update(editor, text, deadline)
        } else {
            editor.write(text, deadline, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{revision, Composition, Selection};

    #[test]
    fn cancellation_removes_only_known_preview_and_preserves_manual_suffix_edits() {
        let original = Selection {
            before: "用户原有：".into(),
            selected: "选中文字".into(),
            after: "，尾部".into(),
        };
        assert_eq!(
            super::cancellation_span(
                "用户原有：临时预览，尾部 手动编辑",
                &original,
                None,
                Some("临时预览")
            ),
            Some(("用户原有：".len(), "用户原有：临时预览".len()))
        );
        assert!(super::cancellation_span(
            "用户改写：临时预览，尾部",
            &original,
            None,
            Some("临时预览")
        )
        .is_none());
        assert!(super::cancellation_span(
            "用户原有：手动改过预览，尾部",
            &original,
            None,
            Some("临时预览")
        )
        .is_none());
    }

    #[test]
    fn cancellation_does_not_infer_an_attempted_write_from_a_matching_prefix() {
        let original = Selection {
            before: String::new(),
            selected: String::new(),
            after: "hello".into(),
        };
        assert!(super::cancellation_span("hello", &original, Some("he"), None).is_none());
        assert!(
            super::cancellation_span("hello manually edited", &original, Some("he"), None)
                .is_none()
        );
        assert_eq!(
            super::cancellation_span("hehello", &original, Some("he"), None),
            Some((0, 2))
        );
    }

    #[test]
    fn cancelling_a_removed_replacement_restores_the_original_selection() {
        let original = Selection {
            before: "前缀".into(),
            selected: "用户原文".into(),
            after: "后缀".into(),
        };
        assert_eq!(
            super::cancellation_span("前缀后缀", &original, None, Some("预览")),
            Some(("前缀".len(), "前缀".len()))
        );
        assert!(super::cancellation_span("前缀后缀", &original, Some("预览"), None).is_none());
    }

    #[test]
    fn cancellation_keeps_the_entire_confirmed_preview_after_a_failed_shortening() {
        let original = Selection {
            before: "Before: ".into(),
            selected: "selected".into(),
            after: " after".into(),
        };
        let start = original.before.len();
        assert_eq!(
            super::cancellation_span(
                "Before: preview longer after",
                &original,
                Some("preview"),
                Some("preview longer")
            ),
            Some((start, start + "preview longer".len()))
        );
        assert_eq!(
            super::cancellation_span(
                "Before: preview longer after",
                &original,
                Some("preview longer"),
                Some("preview")
            ),
            Some((start, start + "preview longer".len()))
        );
    }

    #[test]
    fn final_write_recovers_only_from_an_exact_known_snapshot() {
        let original = Selection {
            before: "开头：".into(),
            selected: "".into(),
            after: "。结尾".into(),
        };
        for (current, safe, written) in [
            (original.after_writing("那。"), true, "那。"),
            (original.after_writing("那你继续"), true, "那你继续"),
            (original.after_writing("那你"), false, "那。"),
            (
                original.after_writing("那你继续").with_selection("。"),
                false,
                "那。",
            ),
        ] {
            let mut composition = Composition {
                original: Some(original.clone()),
                written: Some("那。".into()),
                attempted: Some("那你继续".into()),
                interrupted: Some("无法确认目标已写入".into()),
            };
            assert_eq!(composition.reconcile(&current, original.clone()), safe);
            assert_eq!(composition.written.as_deref(), Some(written));
        }
    }

    #[test]
    fn grows_revises_or_shortens_the_original_hypothesis() {
        assert_eq!(revision("你好", "你好世界"), ("你好", "", "世界"));
        assert_eq!(
            revision("🙂今田下雨", "🙂今天下雨了"),
            ("🙂今", "田下雨", "天下雨了")
        );
        assert_eq!(revision("请开会吧", "请开会"), ("", "请开会吧", "请开会"));
        assert_eq!(revision("完成", "完成"), ("完成", "", ""));
    }
}

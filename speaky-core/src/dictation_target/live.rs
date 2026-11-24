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

    fn writable(&self) -> Result<(), String> {
        self.focus.require_current()?;
        if let Some(editor) = &self.automation {
            editor.require_focus(self.focus)?;
            editor.require_writable()
        } else if self.focus.is_native_edit() && self.focus.writable_native() {
            Ok(())
        } else {
            Err(UNSUPPORTED.into())
        }
    }

    fn snapshot(&self) -> Result<Selection, String> {
        self.writable()?;
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
                let selection = Selection {
                    before: contents(readable(before.GetText(-1))?)?,
                    selected: contents(readable(range.GetText(-1))?)?,
                    after: contents(readable(after.GetText(-1))?)?,
                };
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
        let (mut start, mut end) = (0u32, 0u32);
        self.focus.message(
            EM_GETSEL,
            (&mut start as *mut u32) as usize,
            (&mut end as *mut u32) as isize,
        )?;
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
                let range = readable(prefix.FindText(&BSTR::from(previous), true, false).or_else(
                    |_| prefix.FindText(&BSTR::from(previous.replace('\n', "\r\n")), true, false),
                ))?;
                let ends_at_caret = readable(range.CompareEndpoints(
                    TextPatternRangeEndpoint_End,
                    &caret,
                    TextPatternRangeEndpoint_Start,
                ))? == 0;
                if contents(readable(range.GetText(-1))?)? != previous {
                    return Err(EDIT_CHANGED.into());
                }
                if ends_at_caret {
                    editor.require_focus(self.focus)?;
                    readable(range.Select())?;
                } else {
                    if !editor.is_chromium() {
                        return Err(EDIT_CHANGED.into());
                    }
                    // Chromium sometimes finds the exact suffix at a UIA position
                    // different from the displayed caret. Select from the real
                    // caret only when the original text and selection still match.
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
            return editor.insert(self.focus, text, deadline, check);
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
    interrupted: Option<String>,
}

impl Composition {
    pub(super) fn capture(editor: &Editor) -> Self {
        Self {
            original: editor.snapshot().ok(),
            written: None,
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
        editor.write(replacement, deadline, Some(&expected))?;
        editor.confirm_selection(&original.after_writing(text), deadline)?;
        self.written = Some(text.to_owned());
        Ok(())
    }

    pub(super) fn preview(&mut self, editor: &Editor, text: &str) -> Result<(), String> {
        if self.original.is_none() {
            return Err("无法读取此输入框的光标位置，将在录音结束后尝试写入。".into());
        }
        if self.interrupted.is_none() {
            if let Err(error) = self.update(editor, text, Instant::now() + Duration::from_secs(2)) {
                // A user edit, changed focus, or unconfirmed write ends our ownership.
                // Never append the same dictation again on an ambiguous outcome.
                self.interrupted = Some(error);
            }
        }
        self.interrupted.clone().map_or(Ok(()), Err)
    }

    pub(super) fn finish(
        &mut self,
        editor: &Editor,
        text: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        if let Some(error) = self.interrupted.take() {
            return Err(error);
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
    use super::revision;

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

//! Weixin 4.2 exposes only a Qt top-level window. Never infer an editable range
//! from that UIA window: verify every replacement by copying exactly our span.
use super::*;
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_RIGHT;

pub(super) struct Composer {
    focus: Focus,
    guard: interaction::Guard,
    written: Option<String>,
    interrupted: Option<String>,
}

impl Composer {
    pub(super) fn capture(focus: Focus, guard: interaction::Guard) -> Result<Self, String> {
        guard.check()?;
        Ok(Self {
            focus,
            guard,
            written: None,
            interrupted: None,
        })
    }

    pub(super) fn require_focus(&self) -> Result<(), String> {
        if let Some(error) = &self.interrupted {
            return Err(error.clone());
        }
        self.guard.check()
    }

    fn ready(&self, deadline: Instant) -> Result<(), String> {
        self.require_focus()?;
        while modifiers_down() && Instant::now() < deadline {
            self.guard.check()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        if Instant::now() >= deadline {
            return Err(UNSUPPORTED.into());
        }
        self.guard.check()
    }

    fn select_and_verify(
        &self,
        clipboard: &mut wechat_clipboard::Transaction,
        text: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        self.ready(deadline)?;
        let count = text.graphemes(true).count();
        if count == 0 || count > 8192 {
            return Err(UNSUPPORTED.into());
        }
        let mut keys = vec![virtual_key(VK_SHIFT, 0)];
        for _ in 0..count {
            keys.push(virtual_key(VK_LEFT, KEYEVENTF_EXTENDEDKEY));
            keys.push(virtual_key(
                VK_LEFT,
                KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
            ));
        }
        keys.push(virtual_key(VK_SHIFT, KEYEVENTF_KEYUP));
        self.guard.check_input()?;
        send_inputs(&keys)?;
        // Wait for Qt to process the selection before its standard Copy action.
        std::thread::sleep(Duration::from_millis(35));
        let copied = clipboard.copy_selection(self.focus, &self.guard, deadline)?;
        self.guard.check()?;
        if copied.as_deref() == Some(text) {
            Ok(())
        } else {
            Err("所选文字与本次听写不一致，已停止更新；结果保留在 Speaky 中供复制。".into())
        }
    }

    fn collapse(&self) -> Result<(), String> {
        self.guard.check_input()?;
        send_inputs(&[
            virtual_key(VK_RIGHT, KEYEVENTF_EXTENDEDKEY),
            virtual_key(VK_RIGHT, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
        ])
    }

    fn update(&mut self, text: &str, deadline: Instant) -> Result<(), String> {
        self.ready(deadline)?;
        if self.written.as_deref() == Some(text) {
            return Ok(());
        }
        if text.graphemes(true).take(8193).count() > 8192 {
            return Err(UNSUPPORTED.into());
        }
        let mut clipboard = wechat_clipboard::Transaction::start(deadline)?;
        if let Some(previous) = &self.written {
            self.select_and_verify(&mut clipboard, previous, deadline)?;
        } else {
            // Never delete a selection that existed before this dictation.
            if clipboard
                .copy_selection(self.focus, &self.guard, deadline)?
                .is_some()
            {
                return Err(
                    "微信已有选中文字，请取消选区后重新听写；结果保留在 Speaky 中。".into(),
                );
            }
        }
        clipboard.paste(text, &self.guard, deadline)?;
        std::thread::sleep(Duration::from_millis(90));
        self.select_and_verify(&mut clipboard, text, deadline)?;
        self.collapse()?;
        self.written = Some(text.to_owned());
        clipboard.restore()?;
        Ok(())
    }

    pub(super) fn write(&mut self, text: &str, deadline: Instant) -> Result<(), String> {
        self.require_focus()?;
        let result = self.update(text, deadline);
        if let Err(error) = &result {
            // Never retry an uncertain paste, including at stop or after LLM.
            self.interrupted = Some(error.clone());
        }
        result
    }

    pub(super) fn cancel(&mut self) -> Result<(), String> {
        let Some(written) = self.written.clone() else {
            return Ok(());
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut clipboard = wechat_clipboard::Transaction::start(deadline)?;
        self.select_and_verify(&mut clipboard, &written, deadline)?;
        self.guard.check_input()?;
        send_inputs(&[
            virtual_key(VK_BACK, 0),
            virtual_key(VK_BACK, KEYEVENTF_KEYUP),
        ])?;
        self.written = None;
        clipboard.restore()?;
        Ok(())
    }
}

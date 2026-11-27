use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{Interface, BSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::*;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Controls::{
    EM_GETPASSWORDCHAR, EM_REPLACESEL, STATE_SYSTEM_UNAVAILABLE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, IsWindowEnabled, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VK_BACK, VK_CONTROL, VK_LEFT,
    VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowLongW,
    GetWindowThreadProcessId, IsWindowUnicode, SendMessageTimeoutW, ES_PASSWORD, ES_READONLY,
    GA_ROOT, GUITHREADINFO, GWL_STYLE, SMTO_ABORTIFHUNG, SMTO_BLOCK, STATE_SYSTEM_PROTECTED,
    STATE_SYSTEM_READONLY,
};

const UNCONFIRMED_WRITE: &str = "无法确认目标已写入，请核对输入框和转写。";
const FOCUS_CHANGED: &str = "输入焦点已改变。";
const UNSUPPORTED: &str = "此输入框无法安全地插入转写，请复制结果。";
const NO_EDITOR: &str = "没有找到可写的输入框。请点一下聊天输入框，再启动听写。";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Focus {
    foreground: usize,
    control: usize,
    process: u32,
    thread: u32,
}

impl Focus {
    fn is_wechat(self) -> bool {
        self.is_process(&["weixin.exe", "wechat.exe"])
    }

    fn is_process(self, names: &[&str]) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, self.process);
            if handle.is_null() {
                return false;
            }
            let mut path = [0u16; 1024];
            let mut length = path.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut length);
            CloseHandle(handle);
            if ok == 0 {
                return false;
            }
            let path = String::from_utf16_lossy(&path[..length as usize]);
            path.rsplit('\\').next().is_some_and(|name| {
                names
                    .iter()
                    .any(|candidate| name.eq_ignore_ascii_case(candidate))
            })
        }
    }

    fn current() -> Option<Self> {
        foreground_focus().1
    }

    fn from_window(foreground: HWND, process: u32, thread: u32) -> Option<Self> {
        unsafe {
            if thread == 0 {
                return None;
            }
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            if GetGUIThreadInfo(thread, &mut info) == 0
                || info.hwndFocus.is_null()
                || GetAncestor(info.hwndFocus, GA_ROOT) != foreground
            {
                return None;
            }
            Some(Self {
                foreground: foreground as usize,
                control: info.hwndFocus as usize,
                process,
                thread,
            })
        }
    }

    fn require_current(self) -> Result<(), String> {
        if Self::current() == Some(self) {
            Ok(())
        } else {
            Err(FOCUS_CHANGED.into())
        }
    }

    fn is_native_edit(self) -> bool {
        let class = self.control_class();
        matches!(
            class.as_str(),
            "edit" | "richedit" | "richedit20w" | "richedit50w" | "richedit60w" | "richeditd2dpt"
        )
    }

    fn is_opaque_wechat_window(self) -> bool {
        self.control == self.foreground && self.control_class() == "qt51514qwindowicon"
    }

    fn control_class(self) -> String {
        let mut name = [0; 128];
        let len =
            unsafe { GetClassNameW(self.control as HWND, name.as_mut_ptr(), name.len() as i32) };
        String::from_utf16_lossy(&name[..len as usize]).to_ascii_lowercase()
    }

    fn message(self, message: u32, wparam: usize, lparam: isize) -> Result<usize, String> {
        let mut result = 0;
        let sent = unsafe {
            SendMessageTimeoutW(
                self.control as HWND,
                message,
                wparam,
                lparam,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                500,
                &mut result,
            )
        };
        if sent == 0 {
            Err("目标应用未响应或 Windows 拒绝了写入，请复制结果。".into())
        } else {
            Ok(result)
        }
    }

    fn writable_native(self) -> bool {
        let hwnd = self.control as HWND;
        (unsafe {
            IsWindowEnabled(hwnd) != 0
                && IsWindowUnicode(hwnd) != 0
                && GetWindowLongW(hwnd, GWL_STYLE) & (ES_READONLY | ES_PASSWORD) == 0
        }) && self.message(EM_GETPASSWORDCHAR, 0, 0) == Ok(0)
    }
}

fn foreground_focus() -> (bool, Option<Focus>) {
    let hwnd = unsafe { GetForegroundWindow() };
    let mut process = 0;
    let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
    if process == std::process::id() {
        (true, None)
    } else {
        (false, Focus::from_window(hwnd, process, thread))
    }
}

enum EditRequest {
    Freeze {
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Cancel {
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Preview {
        sequence: u64,
        text: String,
    },
    Final {
        text: String,
        revision: bool,
        deadline: Instant,
        reply: mpsc::SyncSender<FinalDelivery>,
    },
}

enum FinalDelivery {
    TargetChanged,
    Finished(Result<(), String>),
}

#[derive(Clone)]
pub struct DictationTarget {
    focus: Focus,
    wechat: bool,
    writer: mpsc::Sender<EditRequest>,
    sequence: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    issue: Arc<Mutex<Option<String>>>,
}

pub enum Foreground {
    Speaky,
    External(Option<DictationTarget>),
}

#[derive(Clone, Copy)]
pub struct StopFocus(Option<Focus>);
impl StopFocus {
    pub fn capture() -> Self {
        Self(foreground_focus().1)
    }

    pub fn is_wechat(self) -> bool {
        self.0.is_some_and(Focus::is_wechat)
    }
}

pub fn copy_fallback(text: &str) -> Result<(), String> {
    clipboard::copy_text(text)
}

impl DictationTarget {
    pub fn capture_foreground() -> Foreground {
        let (own, focus) = foreground_focus();
        if own {
            Foreground::Speaky
        } else {
            Foreground::External(focus.and_then(Self::capture_field))
        }
    }

    fn capture_field(focus: Focus) -> Option<Self> {
        let is_wechat = focus.is_wechat();
        let opaque = is_wechat && focus.is_opaque_wechat_window();
        let guard = opaque.then(|| interaction::Guard::snapshot(focus));
        let (writer, incoming) = mpsc::channel::<EditRequest>();
        let sequence = Arc::new(AtomicU64::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let latest = sequence.clone();
        let finished = stopped.clone();
        let issue = Arc::new(Mutex::new(None));
        let worker_issue = issue.clone();
        std::thread::Builder::new()
            .name("speaky-dictation-target".into())
            .spawn(move || {
                // UIA ranges and their original focused element belong to this worker's MTA.
                let native = focus.is_native_edit();
                let _apartment = if native || opaque {
                    None
                } else {
                    let Ok(com) = ComApartment::enter() else {
                        *worker_issue.lock().unwrap() = Some(UNSUPPORTED.into());
                        return;
                    };
                    Some(com)
                };
                let mut editor = if let Some(guard) = guard {
                    // This WeChat window class exposes the outer HWND through
                    // UIA. A usable clipboard selection is verified by the editor.
                    match guard
                        .start_watching()
                        .and_then(|guard| wechat::Composer::capture(focus, guard))
                    {
                        Ok(composer) => Writer::WeChat(composer),
                        Err(error) => {
                            *worker_issue.lock().unwrap() = Some(error);
                            return;
                        }
                    }
                } else {
                    let automation = if native {
                        if !focus.writable_native() {
                            *worker_issue.lock().unwrap() = Some(NO_EDITOR.into());
                            return;
                        }
                        None
                    } else {
                        match AutomationEditor::capture(focus) {
                            Ok(editor) => Some(editor),
                            Err(error) => {
                                *worker_issue.lock().unwrap() = Some(error);
                                return;
                            }
                        }
                    };
                    let editor = live::Editor::new(focus, automation);
                    let composition = live::Composition::capture(&editor);
                    Writer::Accessible(editor, composition)
                };
                let mut delivered = false;
                for request in incoming {
                    match request {
                        EditRequest::Freeze { reply } => {
                            // A browser can host several fields behind the same HWND.
                            // Compare the captured UIA element before retaining its preview.
                            let _ = reply.send(editor.require_focus());
                        }
                        EditRequest::Cancel { reply } => {
                            let result = editor.cancel();
                            if let Err(error) = &result {
                                *worker_issue.lock().unwrap() = Some(error.clone());
                            }
                            let _ = reply.send(result);
                            break;
                        }
                        EditRequest::Preview { sequence, text } => {
                            if finished.load(Ordering::Acquire)
                                || sequence != latest.load(Ordering::Acquire)
                            {
                                continue;
                            }
                            if let Err(error) = editor.preview(&text) {
                                *worker_issue.lock().unwrap() = Some(error);
                            }
                        }
                        EditRequest::Final {
                            text,
                            revision,
                            deadline,
                            reply,
                        } => {
                            let result = match editor.require_focus() {
                                Ok(()) if revision == delivered => {
                                    let result = editor.finish(&text, deadline, revision);
                                    delivered = result.is_ok();
                                    FinalDelivery::Finished(result)
                                }
                                Ok(()) => FinalDelivery::Finished(Err(UNSUPPORTED.into())),
                                Err(error) if error == FOCUS_CHANGED => {
                                    FinalDelivery::TargetChanged
                                }
                                Err(error) => FinalDelivery::Finished(Err(error)),
                            };
                            let _ = reply.send(result);
                            if revision || !delivered {
                                break;
                            }
                        }
                    }
                }
            })
            .ok()?;
        Some(Self {
            focus,
            wechat: is_wechat,
            writer,
            sequence,
            stopped,
            issue,
        })
    }

    pub fn delivery_issue(&self) -> Option<String> {
        self.issue.lock().unwrap().clone()
    }

    pub fn is_wechat(&self) -> bool {
        self.wechat
    }

    pub fn preview(&self, text: &str) {
        if text.is_empty() || text.contains('\0') || self.stopped.load(Ordering::Acquire) {
            return;
        }
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.writer.send(EditRequest::Preview {
            sequence,
            text: normalized_lines(text),
        });
    }

    pub fn stop_preview(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    /// Capture synchronously at the stop gesture, before saving/ASR/LLM can change focus.
    pub fn capture_at_stop(previous: Option<&Self>) -> Result<Self, String> {
        Self::capture_stopped(StopFocus::capture(), previous)
    }

    pub fn capture_stopped(stopped: StopFocus, previous: Option<&Self>) -> Result<Self, String> {
        let focus = stopped.0;
        if let Some(previous) = previous {
            previous.stop_preview();
            if previous.wechat {
                if Some(previous.focus) != focus {
                    return Err(FOCUS_CHANGED.into());
                }
                // The lease and exact draft are checked on the editor worker,
                // after any pending preview, immediately before delivery.
                return Ok(previous.clone());
            }
            let mut capture_error = None;
            if Some(previous.focus) == focus {
                match previous.freeze_at_stop(previous.focus.is_native_edit()) {
                    Ok(()) => return Ok(previous.clone()),
                    // The focused HWND is unchanged, but another UIA field owns focus.
                    Err(error) if error == FOCUS_CHANGED => {}
                    Err(error) => capture_error = Some(error),
                }
            }
            let previous = previous.clone();
            std::thread::spawn(move || {
                if let Err(error) = previous.cancel_preview() {
                    log::warn!("清理听写预览失败：{error}");
                }
            });
            if let Some(error) = capture_error {
                return Err(error);
            }
        }
        let focus = focus.ok_or(NO_EDITOR)?;
        let target = Self::capture_field(focus).ok_or(NO_EDITOR)?;
        target.freeze_at_stop(focus.is_native_edit())?;
        Ok(target)
    }

    fn freeze_at_stop(&self, native: bool) -> Result<(), String> {
        if native {
            return Ok(());
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.writer
            .send(EditRequest::Freeze { reply })
            .map_err(|_| self.delivery_issue().unwrap_or_else(|| UNSUPPORTED.into()))?;
        result
            .recv_timeout(Duration::from_millis(150))
            .map_err(|_| "无法及时确认停止时的输入框，请从剪贴板粘贴。".to_owned())?
    }

    pub fn cancel_preview(&self) -> Result<(), String> {
        self.stop_preview();
        let (reply, result) = mpsc::sync_channel(1);
        self.writer
            .send(EditRequest::Cancel { reply })
            .map_err(|_| self.delivery_issue().unwrap_or_else(|| UNSUPPORTED.into()))?;
        result
            .recv_timeout(Duration::from_secs(4))
            .map_err(|_| UNSUPPORTED.to_owned())?
    }

    pub fn deliver(&self, text: &str) -> Result<(), String> {
        self.deliver_stage(text, false)
    }

    pub fn revise(&self, text: &str) -> Result<(), String> {
        self.deliver_stage(text, true)
    }

    fn deliver_stage(&self, text: &str, revision: bool) -> Result<(), String> {
        if text.trim().is_empty() || text.contains('\0') {
            return Err("没有可写入的有效文本。".into());
        }
        match self.insert(text, revision)? {
            FinalDelivery::Finished(result) => result,
            FinalDelivery::TargetChanged => Err(FOCUS_CHANGED.into()),
        }
    }

    fn insert(&self, text: &str, revision: bool) -> Result<FinalDelivery, String> {
        self.stop_preview();
        let (reply, result) = mpsc::sync_channel(1);
        // A preview already writing to the editor must complete before the final replacement.
        let deadline = Instant::now() + Duration::from_secs(6);
        self.writer
            .send(EditRequest::Final {
                text: normalized_lines(text),
                revision,
                deadline,
                reply,
            })
            .map_err(|_| {
                self.delivery_issue()
                    .unwrap_or_else(|| UNSUPPORTED.to_owned())
            })?;
        result.recv_timeout(Duration::from_secs(7)).map_err(|_| {
            self.delivery_issue()
                .unwrap_or_else(|| UNSUPPORTED.to_owned())
        })
    }
}

enum Writer {
    Accessible(live::Editor, live::Composition),
    WeChat(wechat::Composer),
}

impl Writer {
    fn require_focus(&self) -> Result<(), String> {
        match self {
            Self::Accessible(editor, _) => editor.require_focus(),
            Self::WeChat(composer) => composer.require_focus(),
        }
    }

    fn preview(&mut self, text: &str) -> Result<(), String> {
        match self {
            Self::Accessible(editor, composition) => composition.preview(editor, text),
            Self::WeChat(composer) => composer.write(text, Instant::now() + Duration::from_secs(3)),
        }
    }

    fn cancel(&mut self) -> Result<(), String> {
        match self {
            Self::Accessible(editor, composition) => composition.cancel(editor),
            Self::WeChat(composer) => composer.cancel(),
        }
    }

    fn finish(&mut self, text: &str, deadline: Instant, revision: bool) -> Result<(), String> {
        match self {
            Self::Accessible(editor, composition) if revision => {
                composition.revise(editor, text, deadline)
            }
            Self::Accessible(editor, composition) => composition.finish(editor, text, deadline),
            Self::WeChat(composer) => composer.write(text, deadline),
        }
    }
}

struct ComApartment;

impl ComApartment {
    fn enter() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn normalized_lines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn contents(value: BSTR) -> Result<String, String> {
    String::from_utf16(&value)
        .map(|s| normalized_lines(&s))
        .map_err(|_| UNSUPPORTED.into())
}

fn readable<T>(value: windows::core::Result<T>) -> Result<T, String> {
    value.map_err(|_| UNSUPPORTED.to_owned())
}

struct AutomationEditor {
    client: IUIAutomation,
    field: IUIAutomationElement,
}

impl AutomationEditor {
    fn capture(focus: Focus) -> Result<Self, String> {
        focus.require_current()?;
        unsafe {
            let client: IUIAutomation =
                match CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) {
                    Ok(client) => {
                        let client: IUIAutomation = client;
                        if let Ok(options) = client.cast::<IUIAutomation2>() {
                            let _ = options.SetConnectionTimeout(500);
                            let _ = options.SetTransactionTimeout(500);
                        }
                        client
                    }
                    Err(_) => {
                        readable(CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))?
                    }
                };
            let field = readable(client.GetFocusedElement())?;
            let editor = Self { client, field };
            editor
                .require_writable()
                .map_err(|_| NO_EDITOR.to_owned())?;
            editor.require_focus(focus)?;
            Ok(editor)
        }
    }

    fn require_focus(&self, focus: Focus) -> Result<(), String> {
        focus.require_current()?;
        unsafe {
            let current = readable(self.client.GetFocusedElement())?;
            if !readable(self.client.CompareElements(&self.field, &current))?.as_bool()
                || !readable(self.field.CurrentHasKeyboardFocus())?.as_bool()
            {
                return Err(FOCUS_CHANGED.into());
            }
        }
        Ok(())
    }

    fn require_writable(&self) -> Result<(), String> {
        unsafe {
            if readable(self.field.CurrentIsPassword())?.as_bool()
                || !readable(self.field.CurrentIsEnabled())?.as_bool()
            {
                return Err(UNSUPPORTED.into());
            }
            let role = readable(self.field.CurrentControlType())?;
            // Older desktop chat clients may expose only the MSAA edit role.
            let mut legacy_edit = false;
            if let Ok(legacy) = self
                .field
                .GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                    UIA_LegacyIAccessiblePatternId,
                )
            {
                if legacy.CurrentRole().ok() == Some(ROLE_SYSTEM_TEXT) {
                    let flags = readable(legacy.CurrentState())?;
                    if flags
                        & (STATE_SYSTEM_PROTECTED
                            | STATE_SYSTEM_READONLY
                            | STATE_SYSTEM_UNAVAILABLE)
                        != 0
                    {
                        return Err(UNSUPPORTED.into());
                    }
                    legacy_edit = true;
                }
            }
            if let Ok(value) = self
                .field
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            {
                if readable(value.CurrentIsReadOnly())?.as_bool() {
                    return Err(UNSUPPORTED.into());
                }
                if role == UIA_EditControlTypeId || role == UIA_DocumentControlTypeId {
                    return Ok(());
                }
            }
            if legacy_edit {
                return Ok(());
            }
            let text: IUIAutomationTextPattern =
                readable(self.field.GetCurrentPatternAs(UIA_TextPatternId))?;
            let selection = readable(text.GetSelection())?;
            if readable(selection.Length())? != 1 {
                return Err(UNSUPPORTED.into());
            }
            let range = readable(selection.GetElement(0))?;
            let read_only = readable(range.GetAttributeValue(UIA_IsReadOnlyAttributeId))?;
            let typed = &read_only.Anonymous.Anonymous;
            if typed.vt != windows::Win32::System::Variant::VT_BOOL
                || typed.Anonymous.boolVal.0 != 0
            {
                return Err(UNSUPPORTED.into());
            }
            Ok(())
        }
    }

    fn check_input(&self, focus: Focus, deadline: Instant) -> Result<(), String> {
        self.require_writable()?;
        self.require_focus(focus)?;
        if modifiers_down() || Instant::now() >= deadline {
            return Err(UNSUPPORTED.into());
        }
        Ok(())
    }

    fn wait_for_input(&self, focus: Focus, deadline: Instant) -> Result<(), String> {
        while modifiers_down() && Instant::now() < deadline {
            focus.require_current()?;
            std::thread::sleep(Duration::from_millis(15));
        }
        self.check_input(focus, deadline)
    }

    fn is_chromium(&self) -> bool {
        unsafe { self.field.CurrentFrameworkId() }
            .is_ok_and(|id| id.to_string().eq_ignore_ascii_case("Chrome"))
    }

    // Codex exposes the paragraph's CSS placeholder and trailing <br> as
    // the value of an empty editor. A matching string alone could be a real draft.
    fn displays_placeholder(&self, document: &str) -> bool {
        let Some(label) = document.strip_suffix('\n').filter(|s| !s.is_empty()) else {
            return false;
        };
        unsafe {
            let matches = || -> Option<bool> {
                let class = self.field.CurrentClassName().ok()?.to_string();
                if !class.split_ascii_whitespace().any(|s| s == "ProseMirror")
                    || self.field.CurrentName().ok()?.to_string() != label
                {
                    return Some(false);
                }
                let walker = self.client.RawViewWalker().ok()?;
                let paragraph = walker.GetFirstChildElement(&self.field).ok()?;
                let class = paragraph.CurrentClassName().ok()?.to_string();
                if paragraph.CurrentControlType().ok()? != UIA_GroupControlTypeId
                    || !class.split_ascii_whitespace().any(|s| s == "placeholder")
                    || !class
                        .split_ascii_whitespace()
                        .any(|s| s == "before:composer-placeholder")
                {
                    return Some(false);
                }
                let pattern: IUIAutomationTextPattern =
                    self.field.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
                let span = pattern.RangeFromChild(&paragraph).ok()?;
                Some(contents(span.GetText(-1).ok()?).ok()? == document)
            };
            matches() == Some(true)
        }
    }

    fn text_value(&self) -> Result<String, String> {
        unsafe {
            if let Ok(value) = self
                .field
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            {
                if let Ok(text) = value.CurrentValue() {
                    return contents(text);
                }
            }
            if let Ok(pattern) = self
                .field
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            {
                if let Ok(text) = pattern.DocumentRange().and_then(|range| range.GetText(-1)) {
                    return contents(text);
                }
            }
            let legacy: IUIAutomationLegacyIAccessiblePattern = readable(
                self.field
                    .GetCurrentPatternAs(UIA_LegacyIAccessiblePatternId),
            )?;
            contents(readable(legacy.CurrentValue())?)
        }
    }

    fn selection(&self) -> Option<IUIAutomationTextRange> {
        unsafe {
            let pattern: IUIAutomationTextPattern =
                self.field.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
            let selection = pattern.GetSelection().ok()?;
            (selection.Length().ok()? == 1).then_some(())?;
            selection.GetElement(0).ok()
        }
    }

    fn edit_expectation(&self, before: &str, text: &str) -> Option<(String, bool)> {
        unsafe {
            let range = self.selection()?;
            let pattern: IUIAutomationTextPattern =
                self.field.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
            let prefix = pattern.DocumentRange().ok()?;
            prefix
                .MoveEndpointByRange(
                    TextPatternRangeEndpoint_End,
                    &range,
                    TextPatternRangeEndpoint_Start,
                )
                .ok()?;
            let prefix = contents(prefix.GetText(-1).ok()?).ok()?;
            let selected = contents(range.GetText(-1).ok()?).ok()?;
            let suffix = before.strip_prefix(&prefix)?.strip_prefix(&selected)?;
            Some(([prefix.as_str(), text, suffix].concat(), selected == text))
        }
    }

    fn saw_edit(&self, before: &str, text: &str, expected: Option<&(String, bool)>) -> bool {
        let Ok(now) = self.text_value() else {
            return false;
        };
        if let Some((expected, identical_selection)) = expected {
            if now != *expected {
                return false;
            }
            if now != before {
                return true;
            }
            // Replacing a selected phrase with itself changes the selection,
            // not the value. Do not mistake an already present phrase for a paste.
            return *identical_selection
                && self
                    .selection()
                    .and_then(|r| unsafe { r.GetText(1).ok() })
                    .is_some_and(|s| s.is_empty());
        }
        now != before && now.contains(text)
    }

    fn insert(
        &self,
        focus: Focus,
        text: &str,
        deadline: Instant,
        check_position: impl Fn() -> Result<(), String>,
        confirm_composition: Option<impl Fn() -> bool>,
    ) -> Result<(), String> {
        self.wait_for_input(focus, deadline)?;
        let chromium = self.is_chromium();
        let needs_paste = text.contains('\n') || text.contains('\t');
        let clipboard = if needs_paste {
            Some(if chromium {
                clipboard::TemporaryText::for_chromium(text, deadline)?
            } else {
                clipboard::TemporaryText::new(text, deadline)?
            })
        } else if !chromium {
            // Custom editors can ignore VK_PACKET. Prefer standard paste, but
            // keep Unicode input available when the clipboard cannot be saved.
            clipboard::TemporaryText::new(text, deadline).ok()
        } else {
            None
        };
        let expected_value = if confirm_composition.is_none() {
            let before = self.text_value()?;
            let expected = self.edit_expectation(&before, text);
            Some((before, expected))
        } else {
            None
        };
        self.check_input(focus, deadline)?;
        check_position()?;
        if let Some(clipboard) = &clipboard {
            clipboard.require_current()?;
            send_inputs(&[
                virtual_key(VK_CONTROL, 0),
                virtual_key(b'V' as u16, 0),
                virtual_key(b'V' as u16, KEYEVENTF_KEYUP),
                virtual_key(VK_CONTROL, KEYEVENTF_KEYUP),
            ])?;
        } else {
            send_inputs(&unicode_packets(text)?)?;
        }
        let mut result = Err(UNCONFIRMED_WRITE.into());
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
            if let Err(error) = self
                .require_focus(focus)
                .and_then(|()| self.require_writable())
            {
                result = Err(error);
                break;
            }
            if confirm_composition.as_ref().map_or_else(
                || {
                    expected_value.as_ref().is_some_and(|(before, expected)| {
                        self.saw_edit(before, text, expected.as_ref())
                    })
                },
                |confirm| confirm(),
            ) {
                result = Ok(());
                break;
            }
        }
        if let Some(clipboard) = clipboard {
            let restored = clipboard.restore();
            if result.is_ok()
                || result
                    .as_ref()
                    .err()
                    .is_some_and(|error: &String| error == UNCONFIRMED_WRITE)
            {
                restored?;
            }
        }
        result
    }
}

fn virtual_key(vk: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                dwExtraInfo: interaction::INPUT_MARKER,
                ..Default::default()
            },
        },
    }
}

fn send_inputs(input: &[INPUT]) -> Result<(), String> {
    let count = u32::try_from(input.len()).map_err(|_| UNSUPPORTED.to_owned())?;
    let sent = unsafe { SendInput(count, input.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent == count {
        Ok(())
    } else {
        Err("Windows 未能完整写入，请核对输入框和转写。".into())
    }
}
fn modifiers_down() -> bool {
    [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
        .iter()
        .any(|&vk| unsafe { GetAsyncKeyState(i32::from(vk)) < 0 })
}

fn unicode_packets(text: &str) -> Result<Vec<INPUT>, String> {
    if text.chars().any(char::is_control) {
        return Err(UNSUPPORTED.into());
    }
    let mut events = Vec::new();
    for unit in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            events.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: unit,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            });
        }
    }
    Ok(events)
}

mod live;

mod clipboard;
mod interaction;
mod wechat;
mod wechat_clipboard;

#[cfg(test)]
mod tests {
    use super::*;

    fn target_with_worker(writer: mpsc::Sender<EditRequest>) -> DictationTarget {
        DictationTarget {
            focus: Focus {
                foreground: 0,
                control: 0,
                process: 0,
                thread: 0,
            },
            wechat: false,
            writer,
            sequence: Arc::new(AtomicU64::new(0)),
            stopped: Arc::new(AtomicBool::new(false)),
            issue: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn stop_field_check_distinguishes_fields_sharing_a_window() {
        for expected in [Ok(()), Err(FOCUS_CHANGED.to_owned())] {
            let (writer, incoming) = mpsc::channel();
            let target = target_with_worker(writer);
            let response = expected.clone();
            let worker = std::thread::spawn(move || {
                let EditRequest::Freeze { reply } = incoming.recv().unwrap() else {
                    panic!("expected a stop-time field check");
                };
                reply.send(response).unwrap();
            });
            assert_eq!(target.freeze_at_stop(false), expected);
            worker.join().unwrap();
        }
    }

    #[test]
    fn unresponsive_stop_field_check_fails_without_waiting_for_the_preview() {
        let (writer, incoming) = mpsc::channel();
        let target = target_with_worker(writer);
        assert!(target.freeze_at_stop(false).is_err());
        assert!(matches!(
            incoming.try_recv().unwrap(),
            EditRequest::Freeze { .. }
        ));
        // Native HWNDs identify the field without a UIA round-trip.
        assert!(target.freeze_at_stop(true).is_ok());
        assert!(matches!(
            incoming.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn unicode_packets_preserve_surrogate_pairs_without_synthesizing_virtual_keys() {
        let packets = unicode_packets("甲🦀").unwrap();
        assert_eq!(packets.len(), 6);
        assert!(packets
            .iter()
            .all(|packet| unsafe { packet.Anonymous.ki.wVk == 0 }));
        assert_eq!(normalized_lines("甲\r\n乙\r丙"), "甲\n乙\n丙");
    }
    #[test]
    fn rejects_control_sequences() {
        assert!(unicode_packets("文字\r").is_err());
        assert!(unicode_packets("\u{1b}").is_err());
    }
}

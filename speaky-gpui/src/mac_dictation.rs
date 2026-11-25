use core_foundation::{
    base::{Boolean, CFRange, CFType, CFTypeID, CFTypeRef, TCFType},
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryRef},
    string::{CFString, CFStringRef},
};
use core_graphics::event::{CGEvent, CGEventFlags};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc::{
    class, msg_send,
    rc::StrongPtr,
    runtime::{Object, BOOL, YES},
    sel, sel_impl,
};
use std::{
    ffi::c_void,
    ptr,
    time::{Duration, Instant},
};

const FOCUS_CHANGED: &str = "输入焦点已改变。";
const NO_EDITOR: &str = "开始听写前，请先点一下可编辑的输入框。";
const ACCESSIBILITY: &str = "请在「系统设置 → 隐私与安全性 → 辅助功能」中允许 Speaky 写入输入框。";
const AX_VALUE_CF_RANGE: u32 = 4;
const ANSI_V: u16 = 0x09;
const PASTE_FAILED: &str = "无法向原输入框粘贴转写。";

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrusted() -> Boolean;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean;
    fn AXValueGetTypeID() -> CFTypeID;
    fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> Boolean;
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXUIElementGetPid(element: CFTypeRef, pid: *mut i32) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, timeout: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;

}

pub(super) const ACCESSIBILITY_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

pub(super) fn has_access() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

pub(super) fn request_access() {
    if !has_access() {
        unsafe {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
            let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
        }
    }
}

pub(super) struct Target {
    pid: i32,
    field: CFType,
    window_scope: bool,
    terminal: bool,
}

impl Target {
    // All callers run on GPUI's foreground executor.
    pub(super) fn capture() -> Result<Option<Self>, String> {
        let pid = frontmost_pid().ok_or(NO_EDITOR)?;
        if pid as u32 == std::process::id() {
            return Ok(None);
        }
        if !has_access() {
            return Err(ACCESSIBILITY.into());
        }
        let field = focused_field(pid);
        let role = field
            .as_ref()
            .and_then(|field| attribute(field, "AXRole"))
            .and_then(|value| value.downcast::<CFString>());
        let subrole = field
            .as_ref()
            .and_then(|field| attribute(field, "AXSubrole"))
            .and_then(|value| value.downcast::<CFString>());
        if subrole.is_some_and(|role| role.to_string() == "AXSecureTextField") {
            return Err(NO_EDITOR.into());
        }
        if role.as_ref().is_some_and(|role| {
            matches!(
                role.to_string().as_str(),
                "AXTextField" | "AXTextArea" | "AXComboBox"
            )
        }) {
            return Ok(field.map(|field| Self {
                pid,
                field,
                window_scope: false,
                terminal: false,
            }));
        }

        // Herdr and WeChat can expose just a window, even with their input
        // areas focused. Retain the window's identity for the eventual paste.
        let terminal = bundle_is(pid, "dev.herdr.app");
        if terminal || bundle_is(pid, "com.tencent.xinWeChat") {
            let field = focused_window(pid).ok_or(NO_EDITOR)?;
            return Ok(Some(Self {
                pid,
                field,
                window_scope: true,
                terminal,
            }));
        }
        Err(NO_EDITOR.into())
    }

    fn current_target(&self) -> Option<CFType> {
        if self.window_scope {
            focused_window(self.pid)
        } else {
            focused_field(self.pid)
        }
    }

    pub(super) fn require_current(&self) -> Result<(), String> {
        if frontmost_pid() == Some(self.pid)
            && self
                .current_target()
                .is_some_and(|field| field == self.field)
        {
            Ok(())
        } else {
            Err(FOCUS_CHANGED.into())
        }
    }

    pub(super) fn insert(
        &self,
        text: &str,
        cx: &mut gpui::Context<crate::Speaky>,
    ) -> Result<&'static str, String> {
        self.require_current()?;
        if self.terminal && text.chars().any(char::is_control) {
            return Err("转写含有换行或控制字符，请从历史复制到终端。".into());
        }
        // A successful AX setter can be ignored by custom text editors.
        // Deliver through the editor's regular paste command instead.
        let cf_text = CFString::new(text);
        let expected = if self.window_scope {
            None
        } else {
            self.expected_value(text)
        };
        let source = CGEventSource::new(CGEventSourceStateID::Private).map_err(|_| PASTE_FAILED)?;
        let down =
            CGEvent::new_keyboard_event(source.clone(), ANSI_V, true).map_err(|_| PASTE_FAILED)?;
        let up = CGEvent::new_keyboard_event(source, ANSI_V, false).map_err(|_| PASTE_FAILED)?;
        down.set_flags(CGEventFlags::CGEventFlagCommand);
        up.set_flags(CGEventFlags::CGEventFlagCommand);
        let mut clipboard = Pasteboard::put(&cf_text).ok_or(PASTE_FAILED)?;
        self.require_current()?;
        if !clipboard.is_current() {
            return Err(PASTE_FAILED.into());
        }
        // After posting the key, the target can read the pasteboard at any
        // time. Restore the old contents only when the new value is visible.
        clipboard.restore_on_drop = false;
        down.post_to_pid(self.pid);
        up.post_to_pid(self.pid);
        if let Some(expected) = expected {
            let field = self.field.clone();
            let executor = cx.background_executor().clone();
            cx.spawn(async move |_, _| {
                let deadline = Instant::now() + Duration::from_secs(2);
                while clipboard.is_current() && Instant::now() < deadline {
                    if attribute(&field, "AXValue")
                        .and_then(|value| value.downcast::<CFString>())
                        .is_some_and(|value| value.to_string() == expected)
                    {
                        clipboard.restore_on_drop = true;
                        break;
                    }
                    executor.timer(Duration::from_millis(100)).await;
                }
            })
            .detach();
        }
        Ok("已发送粘贴，请核对输入框。无法确认写入时，转写会留在剪贴板。")
    }

    fn expected_value(&self, transcript: &str) -> Option<String> {
        let value = attribute(&self.field, "AXValue")?
            .downcast::<CFString>()?
            .to_string();
        let selection = attribute(&self.field, "AXSelectedTextRange")?;
        let mut range = CFRange {
            location: 0,
            length: 0,
        };
        unsafe {
            if selection.type_of() != AXValueGetTypeID()
                || AXValueGetValue(
                    selection.as_CFTypeRef(),
                    AX_VALUE_CF_RANGE,
                    (&mut range as *mut CFRange).cast(),
                ) == 0
            {
                return None;
            }
        }
        let start = usize::try_from(range.location).ok()?;
        let end = start.checked_add(usize::try_from(range.length).ok()?)?;
        let mut units: Vec<_> = value.encode_utf16().collect();
        if end > units.len() {
            return None;
        }
        units.splice(start..end, transcript.encode_utf16());
        let expected = String::from_utf16(&units).ok()?;
        (expected != value).then_some(expected)
    }
}

fn bundle_is(pid: i32, identifier: &str) -> bool {
    let identifier = CFString::new(identifier);
    unsafe {
        let app: *mut Object =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        let actual: *mut Object = msg_send![app, bundleIdentifier];
        let matches: BOOL =
            msg_send![actual, isEqualToString: identifier.as_concrete_TypeRef() as *mut Object];
        matches == YES
    }
}

fn frontmost_pid() -> Option<i32> {
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut Object = msg_send![workspace, frontmostApplication];
        let pid: i32 = msg_send![app, processIdentifier];
        (pid > 0).then_some(pid)
    }
}

fn focused_field(pid: i32) -> Option<CFType> {
    unsafe {
        let system = CFType::wrap_under_create_rule(AXUIElementCreateSystemWide());
        AXUIElementSetMessagingTimeout(system.as_CFTypeRef(), 0.2);
        let field = attribute(&system, "AXFocusedUIElement")?;
        if field.type_of() != AXUIElementGetTypeID() {
            return None;
        }
        let mut owner = 0;
        (AXUIElementGetPid(field.as_CFTypeRef(), &mut owner) == 0 && owner == pid).then_some(field)
    }
}

fn focused_window(pid: i32) -> Option<CFType> {
    let app = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) };
    attribute(&app, "AXFocusedWindow").or_else(|| {
        let field = focused_field(pid)?;
        let role = attribute(&field, "AXRole")?.downcast::<CFString>()?;
        (role.to_string() == "AXWindow").then_some(field)
    })
}

fn attribute(element: &CFType, name: &str) -> Option<CFType> {
    let name = CFString::new(name);
    let mut value = ptr::null();
    unsafe {
        let status = AXUIElementCopyAttributeValue(
            element.as_CFTypeRef(),
            name.as_concrete_TypeRef(),
            &mut value,
        );
        (status == 0 && !value.is_null()).then(|| CFType::wrap_under_create_rule(value))
    }
}

struct Pasteboard {
    board: StrongPtr,
    saved: StrongPtr,
    change: isize,
    restore_on_drop: bool,
}

impl Pasteboard {
    fn put(text: &CFString) -> Option<Self> {
        unsafe {
            let board: *mut Object = msg_send![class!(NSPasteboard), generalPasteboard];
            let initial: isize = msg_send![board, changeCount];
            let items: *mut Object = msg_send![board, pasteboardItems];
            let count: usize = msg_send![items, count];
            let copies = StrongPtr::new(msg_send![class!(NSMutableArray), new]);
            for index in 0..count {
                let item: *mut Object = msg_send![items, objectAtIndex: index];
                let copy = StrongPtr::new(msg_send![class!(NSPasteboardItem), new]);
                let types: *mut Object = msg_send![item, types];
                let type_count: usize = msg_send![types, count];
                for index in 0..type_count {
                    let kind: *mut Object = msg_send![types, objectAtIndex: index];
                    let data: *mut Object = msg_send![item, dataForType: kind];
                    if data.is_null() {
                        return None;
                    }
                    let copied: BOOL = msg_send![*copy, setData: data forType: kind];
                    if copied != YES {
                        return None;
                    }
                }
                let _: () = msg_send![*copies, addObject: *copy];
            }
            let change: isize = msg_send![board, changeCount];
            if change != initial {
                return None;
            }
            let change: isize = msg_send![board, clearContents];
            let mut session = Self {
                board: StrongPtr::retain(board),
                saved: copies,
                change,
                restore_on_drop: true,
            };
            let kind = CFString::new("public.utf8-plain-text");
            let written: BOOL = msg_send![board, setString: text.as_concrete_TypeRef() as *mut Object forType: kind.as_concrete_TypeRef() as *mut Object];
            session.change = msg_send![board, changeCount];
            (written == YES).then_some(session)
        }
    }

    fn is_current(&self) -> bool {
        let change: isize = unsafe { msg_send![*self.board, changeCount] };
        self.change == change
    }
}

impl Drop for Pasteboard {
    fn drop(&mut self) {
        if self.restore_on_drop && self.is_current() {
            unsafe {
                let _: isize = msg_send![*self.board, clearContents];
                let _: BOOL = msg_send![*self.board, writeObjects: *self.saved];
            }
        }
    }
}

type EditResult = tokio::sync::oneshot::Receiver<Result<Option<String>, String>>;

pub fn edit(window: &mut gpui::Window, current: &str) -> Result<EditResult, String> {
    edit_text(
        window,
        current,
        "编辑常用词",
        "每行一个术语，或输入“误识别词 => 正确写法”。改写规则会应用到预览和最终稿。",
    )
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
pub fn edit_text(
    window: &mut gpui::Window,
    current: &str,
    title: &str,
    detail: &str,
) -> Result<EditResult, String> {
    use block::ConcreteBlock;
    use objc::{class, msg_send, rc::StrongPtr, runtime, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{cell::Cell, ffi::c_void};

    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("无法获取 macOS 窗口。".into());
    };
    let view = handle.ns_view.cast::<runtime::Object>().as_ptr();
    let (send, receive) = tokio::sync::oneshot::channel();
    let send = Cell::new(Some(send));
    let frame = Rect {
        origin: Point { x: 0., y: 0. },
        size: Size {
            width: 470.,
            height: 220.,
        },
    };

    unsafe {
        let parent: *mut runtime::Object = msg_send![view, window];
        if parent.is_null() {
            return Err("无法打开编辑器。".into());
        }
        let existing_sheet: *mut runtime::Object = msg_send![parent, attachedSheet];
        if !existing_sheet.is_null() {
            return Err("请先关闭当前窗口中的表单。".into());
        }
        let alert: *mut runtime::Object = msg_send![class!(NSAlert), new];
        let alert = StrongPtr::new(alert);
        let scroll: *mut runtime::Object = msg_send![class!(NSScrollView), alloc];
        let scroll: *mut runtime::Object = msg_send![scroll, initWithFrame: frame];
        let scroll = StrongPtr::new(scroll);
        let editor: *mut runtime::Object = msg_send![class!(NSTextView), alloc];
        let editor: *mut runtime::Object = msg_send![editor, initWithFrame: frame];
        let editor = StrongPtr::new(editor);
        let container: *mut runtime::Object = msg_send![*editor, textContainer];
        let _: () = msg_send![container, setWidthTracksTextView: runtime::YES];
        let _: () = msg_send![*editor, setVerticallyResizable: runtime::YES];
        let _: () = msg_send![*editor, setHorizontallyResizable: runtime::NO];
        let _: () = msg_send![*editor, setAutoresizingMask: 2usize];
        let _: () = msg_send![*editor, setRichText: runtime::NO];
        let _: () = msg_send![*editor, setAllowsUndo: runtime::YES];
        let _: () = msg_send![*editor, setAutomaticQuoteSubstitutionEnabled: runtime::NO];
        let _: () = msg_send![*editor, setAutomaticDashSubstitutionEnabled: runtime::NO];
        let _: () = msg_send![*editor, setAutomaticSpellingCorrectionEnabled: runtime::NO];
        let _: () = msg_send![*editor, setString: ns_string(current)];
        let font: *mut runtime::Object = msg_send![class!(NSFont), systemFontOfSize: 14.0f64];
        let _: () = msg_send![*editor, setFont: font];
        let _: () = msg_send![*scroll, setDocumentView: *editor];
        let _: () = msg_send![*scroll, setHasVerticalScroller: runtime::YES];
        let _: () = msg_send![*scroll, setBorderType: 2usize];
        let _: () = msg_send![*alert, setAccessoryView: *scroll];
        let _: () = msg_send![*alert, setMessageText: ns_string(title)];
        let _: () = msg_send![*alert, setInformativeText: ns_string(detail)];
        let _: *mut runtime::Object = msg_send![*alert, addButtonWithTitle: ns_string("保存")];
        let cancel: *mut runtime::Object = msg_send![*alert, addButtonWithTitle: ns_string("取消")];
        let _: () = msg_send![cancel, setKeyEquivalent: ns_string("\u{1b}")];

        let alert_window: *mut runtime::Object = msg_send![*alert, window];
        let _: () = msg_send![alert_window, setInitialFirstResponder: *editor];
        let _: runtime::BOOL = msg_send![alert_window, makeFirstResponder: *editor];
        let complete = ConcreteBlock::new(move |answer: isize| {
            if let Some(send) = send.take() {
                let result = if answer == 1000 {
                    let value: *mut runtime::Object = msg_send![*editor, string];
                    let count: usize = msg_send![value, lengthOfBytesUsingEncoding: 4usize];
                    let utf8: *const c_void = msg_send![value, UTF8String];
                    if count == 0 {
                        Ok(Some(String::new()))
                    } else if utf8.is_null() {
                        Err("无法读取编辑内容。".into())
                    } else {
                        std::str::from_utf8(std::slice::from_raw_parts(utf8.cast(), count))
                            .map(|value| Some(value.to_owned()))
                            .map_err(|error| error.to_string())
                    }
                } else {
                    Ok(None)
                };
                let _ = send.send(result);
            }
        })
        .copy();
        let _: () = msg_send![
            *alert,
            beginSheetModalForWindow: parent
            completionHandler: complete
        ];
    }
    Ok(receive)
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
unsafe fn ns_string(value: &str) -> *mut objc::runtime::Object {
    use objc::{class, msg_send, rc::StrongPtr, sel, sel_impl};

    let string: *mut objc::runtime::Object = msg_send![class!(NSString), alloc];
    let string: *mut objc::runtime::Object = msg_send![
        string,
        initWithBytes: value.as_ptr()
        length: value.len()
        encoding: 4usize
    ];
    StrongPtr::new(string).autorelease()
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    width: f64,
    height: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    origin: Point,
    size: Size,
}

#[cfg(target_os = "macos")]
unsafe impl objc::Encode for Rect {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

#[cfg(windows)]
pub fn edit_text(
    window: &mut gpui::Window,
    current: &str,
    title: &str,
    detail: &str,
) -> Result<EditResult, String> {
    crate::windows_dialog::edit(window, 102, title, detail, [current.to_owned()], |[text]| {
        text
    })
}

pub struct ServiceInput {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

type EditResult = tokio::sync::oneshot::Receiver<Result<Option<ServiceInput>, String>>;

#[cfg(target_os = "macos")]
pub use macos::edit;

#[cfg(not(target_os = "macos"))]
pub fn edit(_: &mut gpui::Window, _: &str, _: &str) -> Result<EditResult, String> {
    Err("识别服务编辑器当前仅支持 macOS。".into())
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod macos {
    use super::{EditResult, ServiceInput};
    use block::ConcreteBlock;
    use objc::{class, msg_send, rc::StrongPtr, runtime::Object, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{cell::Cell, ptr::null_mut};

    const FORM_WIDTH: f64 = 440.;
    const UTF8: usize = 4;
    const SAVE: isize = 1000;

    pub fn edit(
        window: &mut gpui::Window,
        base_url: &str,
        model: &str,
    ) -> Result<EditResult, String> {
        let raw_handle =
            HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
        let RawWindowHandle::AppKit(handle) = raw_handle.as_raw() else {
            return Err("无法获取 macOS 窗口。".into());
        };
        let view = handle.ns_view.cast::<Object>().as_ptr();
        let (send, receive) = tokio::sync::oneshot::channel();
        let send = Cell::new(Some(send));

        unsafe {
            let parent: *mut Object = msg_send![view, window];
            if parent.is_null() {
                return Err("无法打开识别服务编辑器。".into());
            }
            let existing_sheet: *mut Object = msg_send![parent, attachedSheet];
            if !existing_sheet.is_null() {
                return Err("请先关闭当前窗口中的表单。".into());
            }

            let alert: *mut Object = msg_send![class!(NSAlert), new];
            let alert = StrongPtr::new(alert);
            let form: *mut Object = msg_send![class!(NSView), alloc];
            let form: *mut Object =
                msg_send![form, initWithFrame: Rect::new(0., 0., FORM_WIDTH, 168.)];
            let form = StrongPtr::new(form);
            let base_field = add_field(
                *form,
                126.,
                "服务地址（Base URL）",
                base_url,
                "https://api.example.com/v1",
                false,
            );
            let model_field = add_field(*form, 70., "模型 ID", model, "sensevoice-small", false);
            let key_field = add_field(
                *form,
                14.,
                "API Key（选填）",
                "",
                "同一服务地址可留空沿用原密钥",
                true,
            );
            let _: () = msg_send![*alert, setMessageText: ns_string("编辑识别服务")];
            let help = ns_string(
                "填写 OpenAI 兼容服务的基础地址和模型 ID。\n同一服务地址可留空沿用原密钥；更换服务器请填入该服务器的密钥。无鉴权的内网服务可填 EMPTY。",
            );
            let _: () = msg_send![*alert, setInformativeText: help];
            let _: () = msg_send![*alert, setAccessoryView: *form];
            let save: *mut Object = msg_send![*alert, addButtonWithTitle: ns_string("保存")];
            let cancel: *mut Object = msg_send![*alert, addButtonWithTitle: ns_string("取消")];
            let _: () = msg_send![cancel, setKeyEquivalent: ns_string("\u{1b}")];

            let alert_window: *mut Object = msg_send![*alert, window];
            let _: () = msg_send![alert_window, setInitialFirstResponder: *base_field];
            let _: () = msg_send![*base_field, setNextKeyView: *model_field];
            let _: () = msg_send![*model_field, setNextKeyView: *key_field];
            let _: () = msg_send![*key_field, setNextKeyView: save];
            let _: () = msg_send![save, setNextKeyView: cancel];
            let _: () = msg_send![cancel, setNextKeyView: *base_field];
            let _: objc::runtime::BOOL = msg_send![alert_window, makeFirstResponder: *base_field];

            let complete = ConcreteBlock::new(move |answer: isize| {
                if let Some(send) = send.take() {
                    let result = if answer == SAVE {
                        let _: objc::runtime::BOOL =
                            msg_send![alert_window, makeFirstResponder: null_mut::<Object>()];
                        read_form(&base_field, &model_field, &key_field).map(Some)
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

    unsafe fn add_field(
        form: *mut Object,
        top: f64,
        caption: &str,
        value: &str,
        placeholder: &str,
        secure: bool,
    ) -> StrongPtr {
        let label: *mut Object =
            msg_send![class!(NSTextField), labelWithString: ns_string(caption)];
        let _: () = msg_send![label, setFrame: Rect::new(0., top + 24., FORM_WIDTH, 18.)];
        let _: () = msg_send![form, addSubview: label];

        let class = if secure {
            class!(NSSecureTextField)
        } else {
            class!(NSTextField)
        };
        let field: *mut Object = msg_send![class, alloc];
        let field: *mut Object =
            msg_send![field, initWithFrame: Rect::new(0., top, FORM_WIDTH, 22.)];
        let field = StrongPtr::new(field);
        let _: () = msg_send![*field, setStringValue: ns_string(value)];
        let _: () = msg_send![*field, setPlaceholderString: ns_string(placeholder)];
        let _: () = msg_send![*field, setAccessibilityLabel: ns_string(caption)];
        let _: () = msg_send![form, addSubview: *field];
        field
    }

    unsafe fn read_form(
        base: &StrongPtr,
        model: &StrongPtr,
        key: &StrongPtr,
    ) -> Result<ServiceInput, String> {
        let base_url = field_text(**base)?;
        let model = field_text(**model)?;
        let api_key = field_text(**key)?;
        Ok(ServiceInput {
            base_url,
            model,
            api_key: (!api_key.is_empty()).then_some(api_key),
        })
    }

    unsafe fn field_text(field: *mut Object) -> Result<String, String> {
        let value: *mut Object = msg_send![field, stringValue];
        let length: usize = msg_send![value, lengthOfBytesUsingEncoding: UTF8];
        if length == 0 {
            return Ok(String::new());
        }
        let bytes: *const u8 = msg_send![value, UTF8String];
        if bytes.is_null() {
            return Err("无法读取识别服务表单。".into());
        }
        std::str::from_utf8(std::slice::from_raw_parts(bytes, length))
            .map(|value| value.trim().to_owned())
            .map_err(|_| "无法读取识别服务表单。".into())
    }

    unsafe fn ns_string(value: &str) -> *mut Object {
        let string: *mut Object = msg_send![class!(NSString), alloc];
        let string: *mut Object = msg_send![
            string,
            initWithBytes: value.as_ptr()
            length: value.len()
            encoding: UTF8
        ];
        StrongPtr::new(string).autorelease()
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }

    impl Rect {
        fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
            Self {
                origin: Point { x, y },
                size: Size { width, height },
            }
        }
    }

    unsafe impl objc::Encode for Rect {
        fn encode() -> objc::Encoding {
            unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
        }
    }
}

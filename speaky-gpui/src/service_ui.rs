pub struct ServiceInput {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

type EditResult<T = ServiceInput> = tokio::sync::oneshot::Receiver<Result<Option<T>, String>>;

const AUTODL_KEY_TITLE: &str = "配置 AutoDL API 密钥";

fn autodl_key_help(has_key: bool) -> &'static str {
    if has_key {
        "语音转写和 AI 润色共用此密钥。留空可保留已保存的密钥。"
    } else {
        "填入 AutoDL 的 API 密钥，语音转写和 AI 润色可共用。"
    }
}

#[cfg(windows)]
pub(super) fn edit_autodl_key(
    window: &mut gpui::Window,
    has_key: bool,
) -> Result<EditResult<String>, String> {
    crate::windows_dialog::edit(
        window,
        104,
        AUTODL_KEY_TITLE,
        autodl_key_help(has_key),
        [String::new()],
        |[key]| key.trim().to_owned(),
    )
}

#[cfg(target_os = "macos")]
pub(super) use macos::edit_autodl_key;

#[cfg(windows)]
pub(super) const VOLC_URL: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async";

#[cfg(windows)]
pub(super) fn edit_volc(window: &gpui::Window, resource_id: &str) -> Result<EditResult, String> {
    crate::windows_dialog::edit(
        window,
        101,
        "配置 Doubao 语音识别",
        "服务地址固定为火山引擎。模型 ID 填资源 ID，密钥留空沿用已有凭证。",
        [VOLC_URL.to_owned(), resource_id.to_owned(), String::new()],
        |[base_url, model, key]| ServiceInput {
            base_url,
            model,
            api_key: (!key.trim().is_empty()).then_some(key),
        },
    )
}

#[cfg(target_os = "macos")]
pub use macos::edit_model;

#[cfg(windows)]
pub fn edit_model(
    window: &mut gpui::Window,
    base_url: &str,
    model: &str,
    organizer: bool,
) -> Result<EditResult, String> {
    let (title, help) = if organizer {
        (
            "编辑整理模型",
            "密钥留空可保留同一服务已有的密钥。本地无鉴权服务可填 EMPTY。",
        )
    } else {
        (
            "连接识别服务",
            "填写已运行的服务地址。OpenAI 兼容服务可填写模型 ID。\n保存时会用内置样本检测接口和转写。同一服务可留空沿用原密钥。",
        )
    };
    crate::windows_dialog::edit(
        window,
        101,
        title,
        help,
        [base_url.to_owned(), model.to_owned(), String::new()],
        |[base_url, model, key]| ServiceInput {
            base_url,
            model,
            api_key: (!key.trim().is_empty()).then_some(key),
        },
    )
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod macos {
    use super::{autodl_key_help, EditResult, ServiceInput, AUTODL_KEY_TITLE};
    use block::ConcreteBlock;
    use objc::{class, msg_send, rc::StrongPtr, runtime::Object, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{cell::Cell, ptr::null_mut};

    const FORM_WIDTH: f64 = 440.;
    const UTF8: usize = 4;
    const SAVE: isize = 1000;

    struct Field<'a> {
        caption: &'static str,
        value: &'a str,
        placeholder: &'static str,
        secure: bool,
    }

    pub fn edit_autodl_key(
        window: &mut gpui::Window,
        has_key: bool,
    ) -> Result<EditResult<String>, String> {
        edit_fields(
            window,
            AUTODL_KEY_TITLE,
            autodl_key_help(has_key),
            [Field {
                caption: "API 密钥",
                value: "",
                placeholder: "AutoDL API Key",
                secure: true,
            }],
            |[key]| unsafe { field_text(**key) },
        )
    }

    pub fn edit_model(
        window: &mut gpui::Window,
        base_url: &str,
        model: &str,
        organizer: bool,
    ) -> Result<EditResult, String> {
        let (title, help, service_caption, example) = if organizer {
            (
                "编辑整理模型",
                "填写 OpenAI 兼容服务的基础地址和模型 ID。\n同一服务器可留空沿用原密钥。更换服务器请填入该服务器的密钥，无鉴权服务可填 EMPTY。",
                "服务地址（Base URL）",
                "gpt-4o-mini",
            )
        } else {
            (
                "自动检测识别服务",
                "保存时自动识别接入方式并转写内置样本。同一服务可留空沿用原密钥。",
                "服务地址（HTTP 或 WebSocket）",
                "OpenAI 兼容服务可填模型 ID",
            )
        };
        edit_fields(
            window,
            title,
            help,
            [
                Field {
                    caption: service_caption,
                    value: base_url,
                    placeholder: "https://api.example.com/v1",
                    secure: false,
                },
                Field {
                    caption: "模型 ID",
                    value: model,
                    placeholder: example,
                    secure: false,
                },
                Field {
                    caption: "API Key（选填）",
                    value: "",
                    placeholder: "同一服务地址可留空沿用原密钥",
                    secure: true,
                },
            ],
            |[base, model, key]| unsafe { read_form(base, model, key) },
        )
    }

    fn edit_fields<const N: usize, T: 'static>(
        window: &mut gpui::Window,
        title: &str,
        help: &str,
        fields: [Field<'_>; N],
        read: impl Fn(&[StrongPtr; N]) -> Result<T, String> + 'static,
    ) -> Result<EditResult<T>, String> {
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
                msg_send![form, initWithFrame: Rect::new(0., 0., FORM_WIDTH, 56. * N as f64)];
            let form = StrongPtr::new(form);
            let controls = std::array::from_fn(|index| {
                let field = &fields[index];
                add_field(
                    *form,
                    14. + 56. * (N - index - 1) as f64,
                    field.caption,
                    field.value,
                    field.placeholder,
                    field.secure,
                )
            });
            let _: () = msg_send![*alert, setMessageText: ns_string(title)];
            let _: () = msg_send![*alert, setInformativeText: ns_string(help)];
            let _: () = msg_send![*alert, setAccessoryView: *form];
            let save: *mut Object = msg_send![*alert, addButtonWithTitle: ns_string("保存")];
            let cancel: *mut Object = msg_send![*alert, addButtonWithTitle: ns_string("取消")];
            let _: () = msg_send![cancel, setKeyEquivalent: ns_string("\u{1b}")];

            let alert_window: *mut Object = msg_send![*alert, window];
            let first = *controls[0];
            let _: () = msg_send![alert_window, setInitialFirstResponder: first];
            for fields in controls.windows(2) {
                let _: () = msg_send![*fields[0], setNextKeyView: *fields[1]];
            }
            let _: () = msg_send![*controls[N - 1], setNextKeyView: save];
            let _: () = msg_send![save, setNextKeyView: cancel];
            let _: () = msg_send![cancel, setNextKeyView: first];
            let _: objc::runtime::BOOL = msg_send![alert_window, makeFirstResponder: first];

            let complete = ConcreteBlock::new(move |answer: isize| {
                if let Some(send) = send.take() {
                    let result = if answer == SAVE {
                        let _: objc::runtime::BOOL =
                            msg_send![alert_window, makeFirstResponder: null_mut::<Object>()];
                        read(&controls).map(Some)
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

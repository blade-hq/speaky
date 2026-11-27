use crate::correction_ui::{CorrectionAnswer, CorrectionResult, RulePreview};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use speaky_core::terms::Replacement;
use std::{io, ops::Range, ptr, thread};
use tokio::sync::oneshot;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Controls::EM_SETLIMITTEXT, WindowsAndMessaging::*},
};

mod correction;

const EDITOR: i32 = 100;

struct Form<const N: usize> {
    title: String,
    hint: String,
    fields: [String; N],
    preview: Option<RulePreview>,
    rules: Vec<Replacement>,
}

// A Windows multiline edit counts UTF-16 code units and CRLF. The transcript
// stored by Speaky uses UTF-8 and LF, including when the selection spans lines.
fn selection_in_transcript(raw: &[u16], start: u32, end: u32) -> Option<Range<usize>> {
    if start >= end {
        return None;
    }
    let offset = |pos: u32| {
        Some(
            String::from_utf16(raw.get(..pos as usize)?)
                .ok()?
                .replace("\r\n", "\n")
                .len(),
        )
    };
    Some(offset(start)?..offset(end)?)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

unsafe fn read_text(control: HWND) -> Vec<u16> {
    let mut buffer = vec![0; GetWindowTextLengthW(control) as usize + 1];
    let count = GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32);
    buffer.truncate(count as usize);
    buffer
}

// The form lives in the dialog thread for the full duration of DialogBoxParamW.
unsafe extern "system" fn dialog_proc<const N: usize>(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_INITDIALOG => {
            let form = &mut *(lparam as *mut Form<N>);
            SetWindowTextW(hwnd, wide(&form.title).as_ptr());
            SetDlgItemTextW(hwnd, 90, wide(&form.hint).as_ptr());
            SetDlgItemTextW(hwnd, IDOK, wide("保存").as_ptr());
            SetDlgItemTextW(hwnd, IDCANCEL, wide("取消").as_ptr());
            if N == 3 {
                for (id, label) in [(91, "服务地址"), (92, "模型 ID"), (93, "API 密钥")] {
                    SetDlgItemTextW(hwnd, id, wide(label).as_ptr());
                }
            }
            for (i, value) in form.fields.iter().enumerate() {
                let id = EDITOR + i as i32;
                SendDlgItemMessageW(hwnd, id, EM_SETLIMITTEXT, 0, 0);
                let value = value.replace("\r\n", "\n").replace('\n', "\r\n");
                SetDlgItemTextW(hwnd, id, wide(&value).as_ptr());
            }
            if let Some(preview) = &mut form.preview {
                correction::init(hwnd);
                correction::refresh(hwnd, preview);
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
            1
        }
        WM_COMMAND => match (wparam & 0xffff) as i32 {
            IDOK => {
                let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Form<N>;
                let Some(form) = data.as_mut() else { return 0 };
                for (i, output) in form.fields.iter_mut().enumerate() {
                    let raw = read_text(GetDlgItem(hwnd, EDITOR + i as i32));
                    *output = String::from_utf16_lossy(&raw).replace("\r\n", "\n");
                }
                if let Some(preview) = &mut form.preview {
                    correction::refresh(hwnd, preview);
                    match correction::selected(hwnd, preview) {
                        Ok(rules) => form.rules = rules,
                        Err(error) => {
                            correction::set_manual_error(hwnd, error);
                            return 1;
                        }
                    }
                }
                EndDialog(hwnd, IDOK as isize);
                1
            }
            IDCANCEL => {
                EndDialog(hwnd, IDCANCEL as isize);
                1
            }
            EDITOR if (wparam >> 16) as u32 == EN_CHANGE => {
                PostMessageW(hwnd, correction::REFRESH, 0, 0);
                0
            }
            correction::HEARD | correction::PREFERRED if (wparam >> 16) as u32 == EN_CHANGE => {
                correction::set_manual_error(hwnd, "");
                1
            }
            _ => 0,
        },
        correction::REFRESH => {
            let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Form<N>;
            if let Some(preview) = data.as_mut().and_then(|form| form.preview.as_mut()) {
                correction::refresh(hwnd, preview);
            }
            0
        }
        WM_CLOSE => {
            EndDialog(hwnd, IDCANCEL as isize);
            1
        }
        _ => 0,
    }
}

pub fn edit<const N: usize, T: Send + 'static>(
    window: &gpui::Window,
    resource: u16,
    title: &str,
    hint: &str,
    fields: [String; N],
    saved: impl FnOnce([String; N]) -> T + Send + 'static,
) -> Result<oneshot::Receiver<Result<Option<T>, String>>, String> {
    open(window, resource, title, hint, fields, None, move |form| {
        saved(form.fields)
    })
}

pub(super) fn edit_correction(
    window: &gpui::Window,
    original: &str,
    current: &str,
) -> Result<CorrectionResult, String> {
    open(
        window,
        103,
        "改错词",
        "修改下方的转写内容。",
        [current.to_owned()],
        Some(RulePreview::new(original)),
        |form| {
            let [text] = form.fields;
            CorrectionAnswer {
                text,
                rules: form.rules,
            }
        },
    )
}

fn open<const N: usize, T: Send + 'static>(
    window: &gpui::Window,
    resource: u16,
    title: &str,
    hint: &str,
    fields: [String; N],
    preview: Option<RulePreview>,
    saved: impl FnOnce(Form<N>) -> T + Send + 'static,
) -> Result<oneshot::Receiver<Result<Option<T>, String>>, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("无法取得主窗口。".into());
    };
    let owner = handle.hwnd.get();
    let mut form = Form {
        title: title.into(),
        hint: hint.into(),
        fields,
        preview,
        rules: Vec::new(),
    };
    let (send, receive) = oneshot::channel();
    // A native modal message loop must not re-enter GPUI's active click handler.
    thread::Builder::new()
        .name("speaky-settings-dialog".into())
        .spawn(move || {
            if form.preview.is_some() {
                correction::register_controls();
            }
            let answer = unsafe {
                DialogBoxParamW(
                    GetModuleHandleW(ptr::null()),
                    resource as usize as *const u16,
                    owner as HWND,
                    Some(dialog_proc::<N>),
                    &mut form as *mut _ as LPARAM,
                )
            };
            let result = match answer {
                -1 | 0 => Err(format!("无法打开编辑窗口：{}", io::Error::last_os_error())),
                value if value == IDOK as isize => Ok(Some(saved(form))),
                _ => Ok(None),
            };
            let _ = send.send(result);
        })
        .map_err(|error| error.to_string())?;
    Ok(receive)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_selection_tracks_utf8_after_emoji_and_line_break() {
        let native: Vec<_> = "🙂\r\n张伟发言".encode_utf16().collect();
        let range = selection_in_transcript(&native, 4, 6).unwrap();
        assert_eq!(&"🙂\n张伟发言"[range], "张伟");
        assert!(selection_in_transcript(&native, 1, 3).is_none());
        assert!(selection_in_transcript(&native, 4, 4).is_none());
    }
}

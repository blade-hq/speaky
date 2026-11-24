use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{io, ops::Range, ptr, thread};
use tokio::sync::oneshot;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{EM_GETSEL, EM_SETLIMITTEXT},
        WindowsAndMessaging::*,
    },
};

struct Form<const N: usize> {
    title: String,
    hint: String,
    fields: [String; N],
    capture_selection: bool,
    selection: Option<Range<usize>>,
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

// The form lives in the dialog thread for the full duration of DialogBoxParamW.
unsafe extern "system" fn dialog_proc<const N: usize>(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
            let form = &*(lparam as *const Form<N>);
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
                let id = 100 + i as i32;
                SendDlgItemMessageW(hwnd, id, EM_SETLIMITTEXT, 0, 0);
                let value = value.replace("\r\n", "\n").replace('\n', "\r\n");
                SetDlgItemTextW(hwnd, id, wide(&value).as_ptr());
            }
            1
        }
        WM_COMMAND => match (wparam & 0xffff) as i32 {
            IDOK => {
                let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Form<N>;
                let Some(form) = data.as_mut() else { return 0 };
                for (i, output) in form.fields.iter_mut().enumerate() {
                    let control = GetDlgItem(hwnd, 100 + i as i32);
                    let mut buffer = vec![0; GetWindowTextLengthW(control) as usize + 1];
                    let count = GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32);
                    if form.capture_selection && i == 0 {
                        let (mut start, mut end) = (0u32, 0u32);
                        SendMessageW(
                            control,
                            EM_GETSEL,
                            (&mut start as *mut u32) as WPARAM,
                            (&mut end as *mut u32) as LPARAM,
                        );
                        form.selection =
                            selection_in_transcript(&buffer[..count as usize], start, end);
                    }
                    *output =
                        String::from_utf16_lossy(&buffer[..count as usize]).replace("\r\n", "\n");
                }
                EndDialog(hwnd, IDOK as isize);
                1
            }
            IDCANCEL => {
                EndDialog(hwnd, IDCANCEL as isize);
                1
            }
            _ => 0,
        },
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
    open(
        window,
        resource,
        title,
        hint,
        fields,
        false,
        move |fields, _| saved(fields),
    )
}

pub fn edit_correction(
    window: &gpui::Window,
    current: &str,
) -> Result<oneshot::Receiver<Result<Option<(String, Option<Range<usize>>)>, String>>, String> {
    open(
        window,
        102,
        "改错词",
        "直接修改转写，保存后会自动记住改过的词。中文人名或专有名词也可以选中完整写法再保存。",
        [current.to_owned()],
        true,
        |[text], range| (text, range),
    )
}

fn open<const N: usize, T: Send + 'static>(
    window: &gpui::Window,
    resource: u16,
    title: &str,
    hint: &str,
    fields: [String; N],
    capture_selection: bool,
    saved: impl FnOnce([String; N], Option<Range<usize>>) -> T + Send + 'static,
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
        capture_selection,
        selection: None,
    };
    let (send, receive) = oneshot::channel();
    // A native modal message loop must not re-enter GPUI's active click handler.
    thread::Builder::new()
        .name("speaky-settings-dialog".into())
        .spawn(move || {
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
                value if value == IDOK as isize => Ok(Some(saved(form.fields, form.selection))),
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

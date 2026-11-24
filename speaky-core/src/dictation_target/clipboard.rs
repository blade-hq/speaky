use std::collections::HashSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::{
    OleFlushClipboard, OleGetClipboard, OleInitialize, OleSetClipboard, OleUninitialize,
    ReleaseStgMedium, CF_UNICODETEXT,
};
use windows::Win32::UI::Shell::SHCreateDataObject;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

const BACKUP_ERROR: &str = "无法完整备份剪贴板，这次转写已保留。";
const RESTORE_ERROR: &str = "Windows 未能还原原剪贴板。";

struct Ole;
impl Ole {
    fn start() -> Result<Self, String> {
        unsafe { OleInitialize(None) }.map_err(|_| BACKUP_ERROR.to_owned())?;
        Ok(Self)
    }
}
impl Drop for Ole {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

struct Format(FORMATETC);
impl Drop for Format {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.ptd.cast())) };
    }
}

struct Medium(STGMEDIUM);
impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

impl Medium {
    fn bytes(bytes: &[u8]) -> Result<Self, String> {
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        if memory.is_null() {
            return Err(BACKUP_ERROR.into());
        }
        let medium = Self(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 {
                hGlobal: HGLOBAL(memory),
            },
            ..Default::default()
        });
        let dest = unsafe { GlobalLock(memory) };
        if dest.is_null() {
            return Err(BACKUP_ERROR.into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest.cast(), bytes.len());
            GlobalUnlock(memory);
        }
        Ok(medium)
    }

    fn give_to(mut self, object: &IDataObject, format: &FORMATETC) -> Result<(), String> {
        unsafe { object.SetData(format, &self.0, true) }.map_err(|_| BACKUP_ERROR.to_owned())?;
        self.0 = STGMEDIUM::default(); // SetData accepted ownership.
        Ok(())
    }
}

fn data_object() -> Result<IDataObject, String> {
    unsafe { SHCreateDataObject(None, None, None) }.map_err(|_| BACKUP_ERROR.to_owned())
}

fn format_name(id: u16) -> String {
    let mut name = [0; 256];
    let len =
        unsafe { GetClipboardFormatNameW(u32::from(id), name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..len as usize])
}

fn snapshot() -> Result<Option<IDataObject>, String> {
    if unsafe { CountClipboardFormats() } == 0 {
        return Ok(None);
    }
    let original = unsafe { OleGetClipboard() }.map_err(|_| BACKUP_ERROR.to_owned())?;
    let formats = unsafe { original.EnumFormatEtc(DATADIR_GET.0 as u32) }
        .map_err(|_| BACKUP_ERROR.to_owned())?;
    let saved = data_object()?;
    let mut public_formats = HashSet::new();
    let mut bytes = 0usize;
    loop {
        let mut entry = Format(FORMATETC::default());
        let mut count = 0;
        let result = unsafe { formats.Next(std::slice::from_mut(&mut entry.0), Some(&mut count)) };
        result.ok().map_err(|_| BACKUP_ERROR.to_owned())?;
        if count == 0 {
            break;
        }
        let format = &mut entry.0;
        // OLE rebuilds these transport records for the new IDataObject. They
        // contain process-local data and must never be replayed as raw handles.
        if matches!(
            format_name(format.cfFormat).as_str(),
            "DataObject"
                | "Ole Private Data"
                | "DataObjectAttributes"
                | "DataObjectAttributesRequiringElevation"
        ) {
            continue;
        }
        if !format.ptd.is_null() || format.lindex != -1 {
            return Err(BACKUP_ERROR.into());
        }
        if !public_formats.insert((format.cfFormat, format.dwAspect, format.tymed)) {
            continue;
        }
        if public_formats.len() > 256 {
            return Err(BACKUP_ERROR.into());
        }
        // GetData transfers an owned STGMEDIUM, not a reference to a native
        // clipboard handle. Prefer the flat representation for clipboard persistence.
        if format.tymed & TYMED_HGLOBAL.0 as u32 != 0 {
            format.tymed = TYMED_HGLOBAL.0 as u32;
        }
        let medium =
            Medium(unsafe { original.GetData(format) }.map_err(|_| BACKUP_ERROR.to_owned())?);
        format.tymed = medium.0.tymed;
        if medium.0.tymed == TYMED_HGLOBAL.0 as u32 {
            let memory = unsafe { medium.0.u.hGlobal }.0;
            let size = unsafe { GlobalSize(memory) };
            bytes = bytes
                .checked_add(size)
                .ok_or_else(|| BACKUP_ERROR.to_owned())?;
            if size == 0 || bytes > 64 * 1024 * 1024 {
                return Err(BACKUP_ERROR.into());
            }
            let ptr = unsafe { GlobalLock(memory) };
            if ptr.is_null() {
                return Err(BACKUP_ERROR.into());
            }
            let independent =
                Medium::bytes(unsafe { std::slice::from_raw_parts(ptr.cast(), size) });
            unsafe {
                GlobalUnlock(memory);
            }
            independent?.give_to(&saved, format)?;
        } else {
            // SetData(fRelease=false) requires the destination to retain its
            // own medium before we release the producer's STGMEDIUM.
            unsafe { saved.SetData(format, &medium.0, false) }
                .map_err(|_| BACKUP_ERROR.to_owned())?;
        }
    }
    if public_formats.is_empty() {
        return Err(BACKUP_ERROR.into());
    }
    Ok(Some(saved))
}

#[derive(Clone, Copy)]
struct Marker {
    format: u16,
    nonce: [u8; 16],
}
impl Marker {
    fn new() -> Result<Self, String> {
        let format =
            unsafe { RegisterClipboardFormatW(windows_sys::w!("Speaky.Dictation.Transaction")) }
                as u16;
        if format == 0 {
            return Err(BACKUP_ERROR.into());
        }
        let nonce = unsafe { CoCreateGuid() }
            .map_err(|_| BACKUP_ERROR.to_owned())?
            .to_u128()
            .to_le_bytes();
        Ok(Self { format, nonce })
    }
    fn matches(self, version: ClipboardVersion) -> Result<bool, ()> {
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(());
            }
            let mut matches = false;
            if ClipboardVersion::current() == version {
                let data = GetClipboardData(u32::from(self.format));
                if !data.is_null() && GlobalSize(data) >= self.nonce.len() {
                    let ptr = GlobalLock(data);
                    if !ptr.is_null() {
                        matches = std::slice::from_raw_parts(ptr.cast::<u8>(), self.nonce.len())
                            == self.nonce;
                        GlobalUnlock(data);
                    }
                }
            }
            CloseClipboard();
            Ok(matches)
        }
    }
}

fn html_payload(text: &str) -> Vec<u8> {
    const PREFIX: &str = "<html><body><!--StartFragment-->";
    const SUFFIX: &str = "<!--EndFragment--></body></html>";
    let mut fragment = String::from("<span style=\"white-space:pre-wrap\">");
    for ch in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
        match ch {
            '&' => fragment.push_str("&amp;"),
            '<' => fragment.push_str("&lt;"),
            '>' => fragment.push_str("&gt;"),
            '\n' => fragment.push_str("<br>"),
            _ => fragment.push(ch),
        }
    }
    fragment.push_str("</span>");
    // CF_HTML offsets are counted from byte zero of the whole UTF-8 payload.
    let header = |start, end, fragment_start, fragment_end| {
        format!("Version:1.0\r\nStartHTML:{start:010}\r\nEndHTML:{end:010}\r\nStartFragment:{fragment_start:010}\r\nEndFragment:{fragment_end:010}\r\n")
    };
    let start = header(0usize, 0usize, 0usize, 0usize).len();
    let fragment_start = start + PREFIX.len();
    let fragment_end = fragment_start + fragment.len();
    let end = fragment_end + SUFFIX.len();
    let mut payload = header(start, end, fragment_start, fragment_end).into_bytes();
    payload.extend_from_slice(PREFIX.as_bytes());
    payload.extend_from_slice(fragment.as_bytes());
    payload.extend_from_slice(SUFFIX.as_bytes());
    payload.push(0);
    payload
}

fn temporary_object(text: &str, signature: Marker) -> Result<IDataObject, String> {
    let data = data_object()?;
    let marker = unsafe {
        RegisterClipboardFormatW(windows_sys::w!(
            "ExcludeClipboardContentFromMonitorProcessing"
        ))
    } as u16;
    let html = unsafe { RegisterClipboardFormatW(windows_sys::w!("HTML Format")) } as u16;
    if marker == 0 || html == 0 {
        return Err(BACKUP_ERROR.into());
    }
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    for (id, block) in [
        (marker, Medium::bytes(&[1, 0, 0, 0])?),
        (signature.format, Medium::bytes(&signature.nonce)?),
        (html, Medium::bytes(&html_payload(text))?),
        (CF_UNICODETEXT.0, Medium::bytes(&bytes)?),
    ] {
        let format = FORMATETC {
            cfFormat: id,
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        block.give_to(&data, &format)?;
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::html_payload;

    #[test]
    fn clipboard_html_offsets_bound_escaped_utf8_fragment_and_one_break_per_line() {
        let payload = html_payload("中文🦀 <script>&\r\n二\n三 >");
        let data = std::str::from_utf8(&payload).unwrap();
        let offset = |tag: &str| -> usize {
            data.lines()
                .find_map(|line| line.strip_prefix(tag))
                .unwrap()
                .parse()
                .unwrap()
        };
        let html = &data[offset("StartHTML:")..offset("EndHTML:")];
        assert!(html.starts_with("<html>") && html.ends_with("</html>"));
        assert_eq!(data.as_bytes()[offset("EndHTML:")], 0);
        assert_eq!(&data[offset("StartFragment:")..offset("EndFragment:")],
            "<span style=\"white-space:pre-wrap\">中文🦀 &lt;script&gt;&amp;<br>二<br>三 &gt;</span>");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ClipboardVersion {
    owner: usize,
    sequence: u32,
}
impl ClipboardVersion {
    fn current() -> Self {
        unsafe {
            Self {
                owner: GetClipboardOwner() as usize,
                sequence: GetClipboardSequenceNumber(),
            }
        }
    }
}

fn restore(
    saved: Option<&IDataObject>,
    temporary: ClipboardVersion,
    marker: Marker,
) -> Result<(), String> {
    // Retry transient contention, checking ownership immediately before every attempt.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match marker.matches(temporary) {
            Ok(false) => return Ok(()),
            Ok(true) if unsafe { OleSetClipboard(saved) }.is_ok() => break,
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(RESTORE_ERROR.into());
        }
        std::thread::sleep(Duration::from_millis(12));
    }
    if saved.is_some() {
        let restored = ClipboardVersion::current();
        while unsafe { OleFlushClipboard() }.is_err() {
            if ClipboardVersion::current() != restored {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(RESTORE_ERROR.into());
            }
            std::thread::sleep(Duration::from_millis(12));
        }
    }
    Ok(())
}

pub(super) struct TemporaryText {
    release: Option<mpsc::Sender<()>>,
    result: mpsc::Receiver<Result<(), String>>,
    version: ClipboardVersion,
    marker: Marker,
}
impl TemporaryText {
    pub(super) fn new(text: &str, deadline: Instant) -> Result<Self, String> {
        Self::from_payload(text.replace('\n', "\r\n"), deadline)
    }

    pub(super) fn for_chromium(text: &str, deadline: Instant) -> Result<Self, String> {
        // Blink contenteditable can treat both halves of CRLF as independent
        // line breaks during plain-text paste. Its textareas also accept LF.
        Self::from_payload(text.to_owned(), deadline)
    }

    fn from_payload(text: String, deadline: Instant) -> Result<Self, String> {
        let (ready, status) = mpsc::sync_channel(1);
        let (release, released) = mpsc::channel();
        let (done, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("speaky-clipboard".into())
            .spawn(move || {
                let run = || -> Result<(), String> {
                    let _ole = Ole::start()?;
                    let original = ClipboardVersion::current();
                    let saved = snapshot()?;
                    let marker = Marker::new()?;
                    let temporary = temporary_object(&text, marker)?;
                    if original != ClipboardVersion::current() || Instant::now() >= deadline {
                        return Err(BACKUP_ERROR.into());
                    }
                    unsafe { OleSetClipboard(&temporary) }.map_err(|_| BACKUP_ERROR.to_owned())?;
                    // Render before notifying the input worker; no deferred request
                    // or message pump is needed while the target pastes these bytes.
                    let flushed = unsafe { OleFlushClipboard() }.is_ok();
                    let version = ClipboardVersion::current();
                    if flushed && ready.send(Ok((version, marker))).is_ok() {
                        let _ = released.recv();
                    }
                    let restored = restore(saved.as_ref(), version, marker);
                    let _ = done.send(restored);
                    if flushed {
                        Ok(())
                    } else {
                        Err(BACKUP_ERROR.into())
                    }
                };
                if let Err(error) = run() {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|_| BACKUP_ERROR.to_owned())?;
        let (version, marker) = status
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| BACKUP_ERROR.to_owned())??;
        Ok(Self {
            release: Some(release),
            result,
            version,
            marker,
        })
    }
    pub(super) fn require_current(&self) -> Result<(), String> {
        if self.marker.matches(self.version) == Ok(true) {
            Ok(())
        } else {
            Err("剪贴板已改变或正在使用，本次转写已保留。".into())
        }
    }
    pub(super) fn restore(mut self) -> Result<(), String> {
        self.release.take();
        self.result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| RESTORE_ERROR.to_owned())?
    }
}

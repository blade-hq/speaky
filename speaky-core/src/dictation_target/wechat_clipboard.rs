use super::*;
use std::ptr::null_mut;
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::Com::{
    STGMEDIUM, STGMEDIUM_0, TYMED_ENHMF, TYMED_GDI, TYMED_HGLOBAL, TYMED_MFPICT,
};
use windows::Win32::System::Memory::GMEM_MOVEABLE as OLE_MOVEABLE;
use windows::Win32::System::Ole::{OleDuplicateData, ReleaseStgMedium, CLIPBOARD_FORMAT};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

const UNAVAILABLE: &str = "剪贴板无法安全备份或正在使用，结果保留在 Speaky 中供复制。";
const CHANGED: &str = "剪贴板已被其他程序改变，结果保留在 Speaky 中供复制。";
const UNICODE_TEXT: u32 = 13;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Version {
    owner: HWND,
    sequence: u32,
}

impl Version {
    fn current() -> Self {
        unsafe {
            Self {
                owner: GetClipboardOwner(),
                sequence: GetClipboardSequenceNumber(),
            }
        }
    }
}

struct Open;
impl Open {
    fn acquire(owner: HWND, deadline: Instant) -> Result<Self, String> {
        loop {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Self);
            }
            if Instant::now() >= deadline {
                return Err(UNAVAILABLE.into());
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

struct Block {
    format: u32,
    data: HANDLE,
    medium: u32,
}

impl Block {
    fn capture(format: u32) -> Result<Self, String> {
        let source = unsafe { GetClipboardData(format) };
        if source.is_null() {
            return Err(UNAVAILABLE.into());
        }
        let (kind, real_format) = match format {
            2 | 9 => (TYMED_GDI, format),
            3 | 0x83 => (TYMED_MFPICT, 3),
            14 | 0x8e => (TYMED_ENHMF, 14),
            0x82 => (TYMED_GDI, 2),
            1 | 4..=8 | 10..=13 | 15..=17 | 0x81 | 0xc000..=0xffff => {
                let size = unsafe { GlobalSize(source) };
                if size == 0 || size > 64 * 1024 * 1024 {
                    return Err(UNAVAILABLE.into());
                }
                (TYMED_HGLOBAL, format)
            }
            _ => return Err(UNAVAILABLE.into()),
        };
        let data = unsafe {
            OleDuplicateData(
                HANDLE(source),
                CLIPBOARD_FORMAT(real_format as u16),
                OLE_MOVEABLE,
            )
        };
        if data.is_invalid() {
            return Err(UNAVAILABLE.into());
        }
        Ok(Self {
            format,
            data,
            medium: kind.0 as u32,
        })
    }

    fn bytes(format: u32, bytes: &[u8]) -> Result<Self, String> {
        let block = Self {
            format,
            data: HANDLE(unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }),
            medium: TYMED_HGLOBAL.0 as u32,
        };
        if block.data.is_invalid() {
            return Err(UNAVAILABLE.into());
        }
        let pointer = unsafe { GlobalLock(block.data.0) };
        if pointer.is_null() {
            return Err(UNAVAILABLE.into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
            GlobalUnlock(block.data.0);
        }
        Ok(block)
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        if !self.data.is_invalid() {
            let mut medium = STGMEDIUM {
                tymed: self.medium,
                u: STGMEDIUM_0 {
                    hGlobal: HGLOBAL(self.data.0),
                },
                ..Default::default()
            };
            unsafe {
                ReleaseStgMedium(&mut medium);
            }
        }
    }
}

pub(super) struct Transaction {
    window: HWND,
    original: Vec<Block>,
    version: Version,
    changed: bool,
}

impl Transaction {
    pub(super) fn start(deadline: Instant) -> Result<Self, String> {
        let window = unsafe {
            CreateWindowExW(
                0,
                windows_sys::w!("STATIC"),
                null_mut(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if window.is_null() {
            return Err(UNAVAILABLE.into());
        }
        let mut transaction = Self {
            window,
            original: Vec::new(),
            version: Version::current(),
            changed: false,
        };
        let open = Open::acquire(window, deadline)?;
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if transaction.original.len() >= 256 {
                return Err(UNAVAILABLE.into());
            }
            // These are OLE transport formats containing process-specific handles.
            // Restore the public data itself with its original clipboard formats.
            if format >= 0xc000 {
                let mut name = [0; 128];
                let size = unsafe {
                    GetClipboardFormatNameW(format, name.as_mut_ptr(), name.len() as i32)
                };
                if matches!(
                    String::from_utf16_lossy(&name[..size as usize]).as_str(),
                    "DataObject"
                        | "Ole Private Data"
                        | "DataObjectAttributes"
                        | "DataObjectAttributesRequiringElevation"
                ) {
                    continue;
                }
            }
            transaction.original.push(Block::capture(format)?);
        }
        drop(open);
        transaction.version = Version::current();
        Ok(transaction)
    }

    pub(super) fn copy_selection(
        &mut self,
        focus: Focus,
        guard: &interaction::Guard,
        deadline: Instant,
    ) -> Result<Option<String>, String> {
        guard.check_input()?;
        if Version::current() != self.version {
            return Err(CHANGED.into());
        }
        let original = self.version;
        send_inputs(&control_key(b'C'))?;
        let until = deadline.min(Instant::now() + Duration::from_millis(180));
        loop {
            if Version::current() != original {
                let open = Open::acquire(self.window, until)?;
                let version = Version::current();
                let mut pid = 0;
                unsafe {
                    GetWindowThreadProcessId(version.owner, &mut pid);
                }
                if version == original || pid != focus.process {
                    return Err(CHANGED.into());
                }
                let content = unicode_text();
                drop(open);
                let checked = Version::current();
                if checked.owner != version.owner {
                    return Err(CHANGED.into());
                }
                self.version = checked;
                self.changed = true;
                guard.check()?;
                return content.map(Some);
            }
            if Instant::now() >= until {
                guard.check()?;
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    }

    pub(super) fn paste(
        &mut self,
        text: &str,
        guard: &interaction::Guard,
        deadline: Instant,
    ) -> Result<(), String> {
        let bytes: Vec<u8> = text
            .replace('\n', "\r\n")
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        let exclusion = unsafe {
            RegisterClipboardFormatW(windows_sys::w!(
                "ExcludeClipboardContentFromMonitorProcessing"
            ))
        };
        let mut blocks = [
            Block::bytes(UNICODE_TEXT, &bytes)?,
            Block::bytes(exclusion, &[1, 0, 0, 0])?,
        ];
        guard.check_input()?;
        let open = Open::acquire(self.window, deadline)?;
        if Version::current() != self.version {
            return Err(CHANGED.into());
        }
        let result = install(&mut blocks);
        drop(open);
        self.version = Version::current();
        self.changed = self.version.owner == self.window;
        result?;
        if !self.changed {
            return Err(CHANGED.into());
        }
        guard.check_input()?;
        if Version::current() != self.version {
            return Err(CHANGED.into());
        }
        send_inputs(&control_key(b'V'))?;
        Ok(())
    }

    pub(super) fn restore(&mut self) -> Result<(), String> {
        if !self.changed || Version::current() != self.version {
            return Ok(());
        }
        let _open = Open::acquire(self.window, Instant::now() + Duration::from_millis(600))?;
        if Version::current() != self.version {
            return Ok(());
        }
        self.changed = false;
        install(&mut self.original)
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            log::warn!("{error}");
        }
        unsafe {
            DestroyWindow(self.window);
        }
    }
}

fn install(blocks: &mut [Block]) -> Result<(), String> {
    if unsafe { EmptyClipboard() } == 0 {
        return Err(UNAVAILABLE.into());
    }
    let mut result = Ok(());
    for block in blocks {
        if unsafe { SetClipboardData(block.format, block.data.0) }.is_null() {
            result = Err(UNAVAILABLE.into());
        } else {
            block.data = HANDLE(null_mut());
        }
    }
    result
}

fn unicode_text() -> Result<String, String> {
    unsafe {
        let data = GetClipboardData(UNICODE_TEXT);
        if data.is_null() {
            return Err(UNAVAILABLE.into());
        }
        let size = GlobalSize(data);
        if !(2..=8 * 1024 * 1024).contains(&size) {
            return Err(UNAVAILABLE.into());
        }
        let pointer = GlobalLock(data);
        if pointer.is_null() {
            return Err(UNAVAILABLE.into());
        }
        let units = std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2);
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        let decoded = String::from_utf16(&units[..end]).map_err(|_| UNAVAILABLE.to_string());
        GlobalUnlock(data);
        decoded.map(|text| normalized_lines(&text))
    }
}

fn control_key(key: u8) -> [INPUT; 4] {
    [
        virtual_key(VK_CONTROL, 0),
        virtual_key(u16::from(key), 0),
        virtual_key(u16::from(key), KEYEVENTF_KEYUP),
        virtual_key(VK_CONTROL, KEYEVENTF_KEYUP),
    ]
}

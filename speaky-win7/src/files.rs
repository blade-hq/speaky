use speaky_core::{audio::AudioRecorder, drafts};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Seek, SeekFrom},
    mem::size_of,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_ENABLESIZING, OFN_EXPLORER,
        OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    },
};

const FORMAT: &str = "支持 16 kHz、单声道、16 位 PCM WAV 文件（最大 256 MiB）。";
const MAX_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn pick_wav(hwnd: HWND) -> Result<Option<PathBuf>, String> {
    choose_path(hwnd, "WAV 音频\0*.wav\0\0", "wav", "", false)
}

pub(crate) fn export_text(hwnd: HWND, text: &str) -> Result<bool, String> {
    let Some(path) = choose_path(hwnd, "文本文件\0*.txt\0\0", "txt", "转写.txt", true)?
    else {
        return Ok(false);
    };
    drafts::write_atomically(&path, text.as_bytes())
        .map_err(|error| format!("导出转写失败：{error}"))?;
    Ok(true)
}

fn choose_path(
    hwnd: HWND,
    filter: &str,
    extension: &str,
    initial_name: &str,
    save: bool,
) -> Result<Option<PathBuf>, String> {
    let filter: Vec<_> = filter.encode_utf16().collect();
    let extension: Vec<_> = extension.encode_utf16().chain([0]).collect();
    let mut filename = vec![0u16; 32_768];
    for (slot, unit) in filename.iter_mut().zip(initial_name.encode_utf16()) {
        *slot = unit;
    }
    let flags = OFN_EXPLORER | OFN_ENABLESIZING | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: filename.as_mut_ptr(),
        nMaxFile: filename.len() as u32,
        Flags: flags
            | if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST
            },
        lpstrDefExt: extension.as_ptr(),
        ..Default::default()
    };
    // All buffers remain valid while the owned modal dialog is open.
    let selected = unsafe {
        if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    };
    if selected == 0 {
        let error = unsafe { CommDlgExtendedError() };
        return if error == 0 {
            Ok(None)
        } else {
            Err(format!("文件选择窗口无法打开（0x{error:04X}）。"))
        };
    }
    let end = filename
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(filename.len());
    Ok(Some(PathBuf::from(OsString::from_wide(&filename[..end]))))
}

fn read_error(e: std::io::Error) -> String {
    format!("读取 WAV 文件失败：{e}")
}

/// Strip optional RIFF chunks before using the same canonical WAV as local recordings.
/// Reading by chunk offsets avoids loading large artwork or LIST tags into memory.
fn canonical_wav<R: Read + Seek>(input: &mut R, length: u64) -> Result<Vec<u8>, String> {
    if length > MAX_BYTES {
        return Err("文件超过 256 MiB，请分段后再导入。".into());
    }
    let invalid = "WAV 文件头或音频数据不完整。";
    let mut header = [0; 12];
    input.read_exact(&mut header).map_err(read_error)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err("所选文件不是 RIFF WAV 音频。".into());
    }
    if u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8 != length {
        return Err(invalid.into());
    }
    let mut offset = 12;
    let mut format_found = false;
    let mut data = None;
    while offset < length {
        if length - offset < 8 {
            return Err(invalid.into());
        }
        input.seek(SeekFrom::Start(offset)).map_err(read_error)?;
        let mut chunk = [0; 8];
        input.read_exact(&mut chunk).map_err(read_error)?;
        let size = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        let begin = offset + 8;
        offset = begin + u64::from(size) + u64::from(size % 2);
        if offset > length {
            return Err(invalid.into());
        }
        match &chunk[..4] {
            b"fmt " => {
                if format_found || size < 16 {
                    return Err(invalid.into());
                }
                let mut info = [0u8; 40];
                input
                    .read_exact(&mut info[..size.min(40) as usize])
                    .map_err(read_error)?;
                // KSDATAFORMAT_SUBTYPE_PCM in its little-endian RIFF representation.
                let extension_size = u32::from(u16::from_le_bytes([info[16], info[17]]));
                let extensible_pcm = info[..2] == [0xfe, 0xff]
                    && size >= 40
                    && extension_size >= 22
                    && extension_size <= size - 18
                    && info[18..20] == 16u16.to_le_bytes()
                    && info[24..40] == [1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
                if !(info[..2] == 1u16.to_le_bytes() || extensible_pcm)
                    || info[2..4] != 1u16.to_le_bytes()
                    || info[4..8] != 16_000u32.to_le_bytes()
                    || info[8..12] != 32_000u32.to_le_bytes()
                    || info[12..14] != 2u16.to_le_bytes()
                    || info[14..16] != 16u16.to_le_bytes()
                {
                    return Err(format!("音频编码不受支持。{FORMAT}"));
                }
                format_found = true;
            }
            b"data" => {
                if data.replace((begin, size)).is_some() || size == 0 || size % 2 != 0 {
                    return Err(invalid.into());
                }
            }
            _ => {}
        }
    }
    let (begin, size) = data.filter(|_| format_found).ok_or(invalid)?;
    let mut wav = Vec::with_capacity(44 + size as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    wav.resize(44 + size as usize, 0);
    input.seek(SeekFrom::Start(begin)).map_err(read_error)?;
    input.read_exact(&mut wav[44..]).map_err(read_error)?;
    drop(AudioRecorder::recorded_frames(&wav)?);
    Ok(wav)
}

pub(crate) fn import_wav(path: &Path) -> Result<PathBuf, String> {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
    {
        return Err(format!("当前无法解码这个文件格式。{FORMAT}"));
    }
    let mut file = File::open(path).map_err(read_error)?;
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() {
        return Err("请选择一个音频文件。".into());
    }
    let wav = canonical_wav(&mut file, metadata.len())?;
    let draft = drafts::new_path();
    drafts::save(&draft, &wav).map_err(|e| format!("无法保存到录音历史：{e}"))?;
    Ok(draft)
}

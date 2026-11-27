use super::{
    button, drafts, Page, RecordingDestination, RecordingStart, Speaky, ACCENT, BORDER, ERROR, INK,
    MUTED, SURFACE,
};
use gpui::{
    canvas, div, point, prelude::*, px, rgb, Context, Div, ExternalPaths, FontWeight, PathBuilder,
    PathPromptOptions,
};
use speaky_core::audio::AudioRecorder;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const FORMAT: &str = "支持 16 kHz、单声道、16 位 PCM WAV 文件（最大 256 MiB）。";
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Default)]
pub(super) struct RedesignFiles {
    picking: bool,
    importing: bool,
    last: Option<(String, PathBuf)>,
    error: Option<String>,
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

fn archive_file(path: &Path) -> Result<PathBuf, String> {
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

impl Speaky {
    fn file_task_busy(&self) -> bool {
        self.transcribing
            || self.recording.is_some()
            || self.unsaved.is_some()
            || self.recognition_model_busy()
            || self.redesign_files.importing
    }

    fn choose_file(&mut self, cx: &mut Context<Self>) {
        if self.file_task_busy() || self.redesign_files.picking {
            return;
        }
        self.redesign_files.picking = true;
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择要转写的 WAV 文件".into()),
        });
        cx.spawn(async move |entity, cx| {
            let selection = result.await;
            let _ = entity.update(cx, |view, cx| {
                view.redesign_files.picking = false;
                match selection {
                    Ok(Ok(Some(paths))) => view.import_files(&paths, cx),
                    Ok(Ok(None)) => (),
                    Ok(Err(error)) => {
                        view.redesign_files.error = Some(format!("打开文件选择器失败：{error}"))
                    }
                    Err(_) => view.redesign_files.error = Some("文件选择器已中断，请重试。".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn import_files(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if self.file_task_busy() || self.redesign_files.picking {
            self.redesign_files.error = Some("请等待当前录音或识别任务结束，再导入文件。".into());
            cx.notify();
            return;
        }
        let [path] = paths else {
            self.redesign_files.error = Some("请一次选择一个音频文件。".into());
            cx.notify();
            return;
        };
        let source = path.clone();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.redesign_files.error = None;
        self.redesign_files.importing = true;
        let task = cx.background_spawn(async move { archive_file(&source) });
        cx.spawn(async move |entity, cx| {
            let imported = task.await;
            let _ = entity.update(cx, |view, cx| {
                view.redesign_files.importing = false;
                match imported {
                    Ok(path) => {
                        view.redesign_files.last = Some((name, path.clone()));
                        view.archive = drafts::list();
                        if view.file_task_busy() {
                            view.redesign_files.error =
                                Some("录音已导入。当前任务结束后可以开始转写。".into());
                        } else {
                            view.transcribe(path, RecordingDestination::Archive, cx);
                        }
                    }
                    Err(error) => view.redesign_files.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn v30_files(&self, cx: &mut Context<Self>) -> Div {
        let state = &self.redesign_files;
        let busy = self.file_task_busy() || state.picking;
        let drop_zone = div()
            .id("v30-file-drop")
            .w_full()
            .min_h(px(410.))
            .p_6()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .rounded(px(16.))
            .border_1()
            .border_dashed()
            .border_color(rgb(BORDER()))
            .bg(rgb(SURFACE()))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.border_color(rgb(ACCENT())))
            .on_drop(cx.listener(|view, dropped: &ExternalPaths, _, cx| {
                view.import_files(dropped.paths(), cx)
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    |bounds, (), window, _| {
                        let mut icon = PathBuilder::stroke(px(2.));
                        let at = |x: f32, y: f32| bounds.origin + point(px(x), px(y));
                        icon.move_to(at(9., 3.));
                        for (x, y) in [
                            (25., 3.),
                            (35., 13.),
                            (35., 39.),
                            (7., 39.),
                            (7., 5.),
                            (9., 3.),
                        ] {
                            icon.line_to(at(x, y));
                        }
                        icon.move_to(at(25., 3.));
                        icon.line_to(at(25., 13.));
                        icon.line_to(at(35., 13.));
                        icon.move_to(at(21., 19.));
                        icon.line_to(at(21., 31.));
                        icon.move_to(at(15., 25.));
                        icon.line_to(at(21., 31.));
                        icon.line_to(at(27., 25.));
                        if let Ok(path) = icon.build() {
                            window.paint_path(path, rgb(MUTED()));
                        }
                    },
                )
                .size(px(42.)),
            )
            .child(
                div()
                    .text_size(px(17.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("拖入音频文件"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED()))
                    .child(FORMAT),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_2()
                    .child(
                        button("选择文件…", true)
                            .id("v30-file-pick")
                            .when(busy, |b| b.opacity(0.5))
                            .when(!busy, |b| {
                                b.on_click(cx.listener(|view, _, _, cx| view.choose_file(cx)))
                            }),
                    )
                    .child(
                        button("录制音频", false)
                            .id("v30-file-record")
                            .when(busy, |b| b.opacity(0.5))
                            .when(!busy, |b| {
                                b.on_click(cx.listener(|view, _, _, cx| {
                                    view.open_page(Page::Home, cx);
                                    view.toggle_recording(RecordingStart::Button, cx);
                                }))
                            }),
                    ),
            );
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .text_color(rgb(INK()))
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("转录文件"),
            )
            .child(drop_zone);
        if state.importing {
            page = page.child("正在校验音频并保存至录音历史…");
        }
        if let Some(error) = &state.error {
            page = page.child(div().text_color(rgb(ERROR())).child(error.clone()));
        }
        if let Some((name, path)) = &state.last {
            let entry = self.archive.iter().find(|entry| entry.path == *path);
            let text = entry.and_then(|entry| match &entry.state {
                drafts::State::Transcribed { text } => Some(text),
                _ => None,
            });
            let mut latest = super::card()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(name.clone()));
            if let Some(text) = text {
                latest = latest
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED()))
                            .child("ASR 原文"),
                    )
                    .child(text.clone());
                if let Some(organized) = entry.and_then(|entry| entry.organized.as_ref()) {
                    latest = latest
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(MUTED()))
                                .child("整理稿"),
                        )
                        .child(organized.text.clone());
                }
            } else if self.transcribing {
                latest = latest.child("正在转写…");
            } else {
                let retry = path.clone();
                latest = latest.child("原始录音已保存在历史中。").child(
                    button("转写录音", false)
                        .id("v30-file-retry")
                        .when(busy, |b| b.opacity(0.5))
                        .when(!busy, |b| {
                            b.on_click(cx.listener(move |view, _, _, cx| {
                                view.redesign_files.error = None;
                                view.transcribe(retry.clone(), RecordingDestination::Archive, cx);
                            }))
                        }),
                );
            }
            page = page.child(latest);
        }
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tagged() -> Vec<u8> {
        let mut wav = Vec::from(*b"RIFF\0\0\0\0WAVE");
        wav.extend_from_slice(
            b"JUNK\x03\0\0\0abc\0fmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0",
        );
        wav.extend_from_slice(b"data\x04\0\0\0\x01\0\x02\0");
        let size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&size.to_le_bytes());
        wav
    }

    #[test]
    fn imports_tagged_wav_as_a_resumable_draft() {
        let wav = tagged();
        let clean = canonical_wav(&mut Cursor::new(&wav), wav.len() as u64).unwrap();
        assert_eq!(clean.len(), 48);
        assert_eq!(AudioRecorder::recorded_frames(&clean).unwrap(), [1, 2]);
    }

    #[test]
    fn rejects_invalid_riff_sizes_and_wrong_channels() {
        let mut wav = tagged();
        let size = wav.len() as u64;
        assert!(canonical_wav(&mut Cursor::new(&wav), size - 1).is_err());
        assert!(canonical_wav(&mut Cursor::new(&wav), MAX_BYTES + 1).is_err());
        wav[34] = 2;
        assert!(canonical_wav(&mut Cursor::new(&wav), size).is_err());
    }

    #[test]
    fn accepts_pcm_extensible_and_rejects_other_subtypes() {
        let mut wav = tagged();
        wav[28..32].copy_from_slice(&40u32.to_le_bytes());
        wav[32..34].copy_from_slice(&0xfffeu16.to_le_bytes());
        let extension = [
            22, 0, 16, 0, 4, 0, 0, 0, // extension size, valid bits, mono speaker mask
            1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113,
        ];
        wav.splice(48..48, extension);
        let size = wav.len() as u64;
        wav[4..8].copy_from_slice(&((size - 8) as u32).to_le_bytes());
        let clean = canonical_wav(&mut Cursor::new(&wav), size).unwrap();
        assert_eq!(AudioRecorder::recorded_frames(&clean).unwrap(), [1, 2]);
        wav[56] = 3; // IEEE float
        assert!(canonical_wav(&mut Cursor::new(&wav), size).is_err());
    }
}

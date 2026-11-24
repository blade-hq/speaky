use speaky_core::wave::{VoiceWave, COLORS};
use std::{ffi::c_void, mem::size_of, ptr::null};
use windows::{
    core::{w, Result},
    Win32::{
        Foundation::{COLORREF, HWND, POINT, SIZE},
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::*,
            Imaging::*,
        },
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
        UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
    },
};

pub struct Graphics {
    factory: ID2D1Factory,
    wic: IWICImagingFactory,
    text: IDWriteTextFormat,
    pub scale: f32,
}

pub struct Surface {
    bitmap: IWICBitmap,
    target: ID2D1RenderTarget,
    dc: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    size: SIZE,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.dib.into());
        }
    }
}

pub fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn color(rgb: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 255) as f32 / 255.,
        g: ((rgb >> 8) & 255) as f32 / 255.,
        b: (rgb & 255) as f32 / 255.,
        a,
    }
}

pub fn system_scale() -> f32 {
    unsafe {
        let dc = GetDC(None);
        let dpi = GetDeviceCaps(Some(dc), LOGPIXELSX);
        ReleaseDC(None, dc);
        dpi.max(96) as f32 / 96.
    }
}

impl Graphics {
    pub fn new(scale: f32) -> Result<Self> {
        unsafe {
            let factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let wic = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text = write.CreateTextFormat(
                w!("Microsoft YaHei"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                14.,
                w!("zh-CN"),
            )?;
            text.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let ellipsis = write.CreateEllipsisTrimmingSign(&text)?;
            text.SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    ..Default::default()
                },
                &ellipsis,
            )?;
            Ok(Self {
                factory,
                wic,
                text,
                scale,
            })
        }
    }

    pub fn surface(&self, width: i32, height: i32) -> Result<Surface> {
        let size = SIZE {
            cx: (width as f32 * self.scale).round() as i32,
            cy: (height as f32 * self.scale).round() as i32,
        };
        unsafe {
            let bitmap = self.wic.CreateBitmap(
                size.cx as u32,
                size.cy as u32,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnLoad,
            )?;
            let target = self.factory.CreateWicBitmapRenderTarget(
                &bitmap,
                &D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96. * self.scale,
                    dpiY: 96. * self.scale,
                    ..Default::default()
                },
            )?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let mut bits = std::ptr::null_mut::<c_void>();
            let dib = CreateDIBSection(
                None,
                &BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: size.cx,
                        biHeight: -size.cy,
                        biPlanes: 1,
                        biBitCount: 32,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )?;
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                let _ = DeleteObject(dib.into());
                return Err(windows::core::Error::from_win32());
            }
            SelectObject(dc, dib.into());
            Ok(Surface {
                bitmap,
                target,
                dc,
                dib,
                bits: bits.cast(),
                size,
            })
        }
    }

    pub fn text(&self, target: &ID2D1RenderTarget, text: &str, bounds: D2D_RECT_F) -> Result<()> {
        unsafe {
            let brush = target.CreateSolidColorBrush(&color(0xf5fbff, 1.), None)?;
            let text: Vec<_> = text.encode_utf16().collect();
            target.DrawText(
                &text,
                &self.text,
                &bounds,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }

    pub fn wave(
        &self,
        target: &ID2D1RenderTarget,
        wave: VoiceWave,
        width: f32,
        y: f32,
    ) -> Result<()> {
        for echo in [true, false] {
            let Some(curve) = wave.sample(width, 28., echo) else {
                continue;
            };
            unsafe {
                let geometry = self.factory.CreatePathGeometry()?;
                let sink = geometry.Open()?;
                let stops = std::array::from_fn::<_, 5, _>(|i| D2D1_GRADIENT_STOP {
                    position: i as f32 / 4.,
                    color: color(COLORS[i], curve.opacity),
                });
                let mut gradient = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default();
                gradient.startPoint.X = 1.;
                gradient.endPoint.X = width - 1.;
                let mut point = gradient.startPoint;
                let mut points = curve
                    .spans
                    .iter()
                    .enumerate()
                    .flat_map(|(i, span)| span.iter().skip(usize::from(i != 0)));
                if let Some(&(x, dy)) = points.next() {
                    point.X = x;
                    point.Y = y + dy;
                    sink.BeginFigure(point, D2D1_FIGURE_BEGIN_HOLLOW);
                    for &(x, dy) in points {
                        point.X = x;
                        point.Y = y + dy;
                        sink.AddLine(point);
                    }
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
                sink.Close()?;
                let stops = target.CreateGradientStopCollection(
                    &stops,
                    D2D1_GAMMA_2_2,
                    D2D1_EXTEND_MODE_CLAMP,
                )?;
                let brush = target.CreateLinearGradientBrush(&gradient, None, &stops)?;
                target.DrawGeometry(&geometry, &brush, curve.thickness, None);
            }
        }
        Ok(())
    }
}

pub fn fill(
    target: &ID2D1RenderTarget,
    bounds: D2D_RECT_F,
    radius: f32,
    rgb: u32,
    alpha: f32,
) -> Result<()> {
    unsafe {
        let brush = target.CreateSolidColorBrush(&color(rgb, alpha), None)?;
        target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: bounds,
                radiusX: radius,
                radiusY: radius,
            },
            &brush,
        );
    }
    Ok(())
}

impl Surface {
    pub fn draw(
        &mut self,
        hwnd: windows_sys::Win32::Foundation::HWND,
        draw: impl FnOnce(&ID2D1RenderTarget) -> Result<()>,
    ) -> Result<()> {
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&color(0, 0.)));
            let result = draw(&self.target);
            self.target.EndDraw(None, None)?;
            result?;
            self.bitmap.CopyPixels(
                null(),
                self.size.cx as u32 * 4,
                std::slice::from_raw_parts_mut(
                    self.bits,
                    (self.size.cx * self.size.cy * 4) as usize,
                ),
            )?;
            UpdateLayeredWindow(
                HWND(hwnd),
                None,
                None,
                Some(&self.size),
                Some(self.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                    ..Default::default()
                }),
                ULW_ALPHA,
            )
        }
    }
}

use super::*;
use windows_sys::Win32::Foundation::RECT;

struct Palette {
    background: HBRUSH,
    rail: HBRUSH,
    surface: HBRUSH,
    selected: HBRUSH,
    accent: HBRUSH,
    surface_color: u32,
    ink: u32,
}

thread_local! {
    static PALETTE: RefCell<Option<Palette>> = const { RefCell::new(None) };
}

pub(super) fn set_palette(theme: &str) {
    PALETTE.with(|palette| *palette.borrow_mut() = Some(Palette::new(theme)));
}

const fn color(rgb: u32) -> u32 {
    ((rgb & 0xff) << 16) | (rgb & 0xff00) | (rgb >> 16)
}

impl Palette {
    fn new(theme: &str) -> Self {
        let [background, rail, surface, selected, accent, ink] = if theme == "light" {
            [0xf5f5f2, 0xeceee9, 0xffffff, 0xdcebe1, 0x285848, 0x1e2722]
        } else {
            [0x161617, 0x222224, 0x27272a, 0x30374b, 0x4767d0, 0xf4f4f6]
        };
        let brush = |value| unsafe { CreateSolidBrush(color(value)) };
        Self {
            background: brush(background),
            rail: brush(rail),
            surface: brush(surface),
            selected: brush(selected),
            accent: brush(accent),
            surface_color: color(surface),
            ink: color(ink),
        }
    }
}

impl Drop for Palette {
    fn drop(&mut self) {
        for brush in [
            self.background,
            self.rail,
            self.surface,
            self.selected,
            self.accent,
        ] {
            unsafe {
                DeleteObject(brush);
            }
        }
    }
}

impl Window {
    fn selected_button(&self, id: usize) -> bool {
        match self.panel {
            Panel::Home => id == NAV_HOME,
            Panel::Files => id == NAV_FILES,
            Panel::History => id == NAV_HISTORY,
            Panel::Settings => id == NAV_MODELS,
            Panel::Terms => id == TERMS,
            Panel::Guide => id == NAV_GUIDE,
            Panel::Benchmark => id == NAV_BENCHMARK,
            Panel::Organizer | Panel::Diagnostics => id == NAV_DIAGNOSTICS,
        }
    }

    pub(super) fn paint(&self, message: u32, _w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        PALETTE.with(|palette| {
            let palette = palette.borrow();
            let palette = palette.as_ref()?;
            unsafe {
                match message {
                    WM_DRAWITEM if l != 0 => {
                        let item = &*(l as *const DRAWITEMSTRUCT);
                        let id = item.CtlID as usize;
                        let active = self.selected_button(id);
                        let brush = if id == RECORD || id == IMPORT {
                            palette.accent
                        } else if active || item.itemState & ODS_SELECTED != 0 {
                            palette.selected
                        } else {
                            palette.surface
                        };
                        FillRect(item.hDC, &item.rcItem, brush);
                        SetBkMode(item.hDC, TRANSPARENT as i32);
                        SetTextColor(
                            item.hDC,
                            if matches!(id, RECORD | IMPORT) {
                                0xffffff
                            } else {
                                palette.ink
                            },
                        );
                        let old = SelectObject(item.hDC, self.font);
                        let mut text = [0u16; 256];
                        GetWindowTextW(item.hwndItem, text.as_mut_ptr(), text.len() as i32);
                        let mut bounds = item.rcItem;
                        bounds.left += px(self.scale, 8);
                        bounds.right -= px(self.scale, 8);
                        DrawTextW(
                            item.hDC,
                            text.as_ptr(),
                            -1,
                            &mut bounds,
                            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                        );
                        SelectObject(item.hDC, old);
                        if item.itemState & ODS_FOCUS != 0 {
                            DrawFocusRect(item.hDC, &item.rcItem);
                        }
                        Some(1)
                    }
                    _ => None,
                }
            }
        })
    }
}

pub(super) fn paint_control(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
    PALETTE.with(|palette| {
        let palette = palette.borrow();
        let palette = palette.as_ref()?;
        let scale = d2d::system_scale();
        unsafe {
            match message {
                WM_ERASEBKGND => {
                    let mut bounds = RECT::default();
                    GetClientRect(hwnd, &mut bounds);
                    FillRect(w as HDC, &bounds, palette.background);
                    bounds.right = px(scale, RAIL_WIDTH - 4);
                    FillRect(w as HDC, &bounds, palette.rail);
                    Some(1)
                }
                WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                    SetTextColor(w as HDC, palette.ink);
                    SetBkMode(w as HDC, TRANSPARENT as i32);
                    let mut bounds = RECT::default();
                    let mut window = RECT::default();
                    GetWindowRect(l as HWND, &mut bounds);
                    GetWindowRect(hwnd, &mut window);
                    let brush = if bounds.left - window.left < px(scale, RAIL_WIDTH) {
                        palette.rail
                    } else if matches!(GetDlgCtrlID(l as HWND) as usize, BODY | LIVE_TEXT | LEARNED)
                    {
                        palette.surface
                    } else {
                        palette.background
                    };
                    Some(brush as LRESULT)
                }
                WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                    SetTextColor(w as HDC, palette.ink);
                    SetBkColor(w as HDC, palette.surface_color);
                    Some(palette.surface as LRESULT)
                }
                _ => None,
            }
        }
    })
}

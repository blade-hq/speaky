use gpui::WindowAppearance;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

// All windows read the same selection; painting never reads configuration from disk.
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub(crate) fn current() -> Theme {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Theme::Light,
        _ => Theme::Dark,
    }
}

pub(crate) fn set(theme: Theme) {
    CURRENT.store(u8::from(theme == Theme::Light), Ordering::Relaxed);
}

pub(crate) fn resolve(config: &str, system: WindowAppearance) -> Theme {
    match config.trim().to_ascii_lowercase().as_str() {
        "light" => Theme::Light,
        "auto" => match system {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::Dark,
        },
        _ => Theme::Dark,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) background: u32,
    pub(crate) rail: u32,
    pub(crate) surface: u32,
    pub(crate) sidebar: u32,
    pub(crate) ink: u32,
    pub(crate) muted: u32,
    pub(crate) border: u32,
    pub(crate) accent: u32,
    pub(crate) accent_ink: u32,
    pub(crate) selection: u32,
    pub(crate) error: u32,
    pub(crate) success: u32,
    pub(crate) warning: u32,
    pub(crate) heat: [u32; 5],
}

// Based on the desktop prototype's Typeless dark and Speaky green palettes.
const DARK: Palette = Palette {
    background: 0x161617,
    rail: 0x222224,
    surface: 0x27272a,
    sidebar: 0x2f2f33,
    ink: 0xf4f4f6,
    muted: 0x8e8e96,
    border: 0x3b3b40,
    accent: 0x6889ff,
    accent_ink: 0xffffff,
    selection: 0x30374b,
    error: 0xef6461,
    success: 0x46c88a,
    warning: 0xf0b24a,
    heat: [0x333337, 0x303f6b, 0x3f5aa8, 0x5778e8, 0x799aff],
};

const LIGHT: Palette = Palette {
    background: 0xf5f5f2,
    rail: 0xeceee9,
    surface: 0xffffff,
    sidebar: 0xe9eee9,
    ink: 0x1e2722,
    muted: 0x5d6c64,
    border: 0xd7dfd8,
    accent: 0x285848,
    accent_ink: 0xffffff,
    selection: 0xdcebe1,
    error: 0xb83c2f,
    success: 0x247a51,
    warning: 0xa06410,
    heat: [0xe7ece8, 0xbed8c9, 0x84b79e, 0x4c8c71, 0x285848],
};

pub(crate) fn palette() -> &'static Palette {
    match current() {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    }
}

pub(crate) fn choose(dark: u32, light: u32) -> u32 {
    match current() {
        Theme::Dark => dark,
        Theme::Light => light,
    }
}

pub(crate) const fn rgba(color: u32, alpha: u8) -> u32 {
    (color << 8) | alpha as u32
}

macro_rules! colors {
    ($($name:ident),+ $(,)?) => {
        $(pub(crate) fn $name() -> u32 { palette().$name })+
    };
}

colors!(
    background, rail, surface, sidebar, ink, muted, border, accent, accent_ink, selection, error,
    success, warning,
);

pub(crate) fn heat() -> [u32; 5] {
    palette().heat
}

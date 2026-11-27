#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod d2d;
#[cfg(windows)]
mod files;
#[cfg(windows)]
mod floating;
#[cfg(windows)]
mod hotkey_capture;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod window;

#[cfg(windows)]
fn main() {
    if let Err(error) = window::run() {
        window::alert(std::ptr::null_mut(), &error);
    }
}

#[cfg(not(windows))]
fn main() {}

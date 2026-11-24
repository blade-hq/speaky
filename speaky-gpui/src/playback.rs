#[cfg(not(windows))]
pub struct Playback(std::process::Child);

#[cfg(not(windows))]
impl Playback {
    pub fn start(path: &std::path::Path) -> Result<Self, String> {
        std::process::Command::new("/usr/bin/afplay")
            .arg(path)
            .spawn()
            .map(Self)
            .map_err(|err| err.to_string())
    }

    pub fn is_finished(&mut self) -> Result<bool, String> {
        self.0
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|err| err.to_string())
    }
}

#[cfg(not(windows))]
impl Drop for Playback {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(windows)]
pub use windows::Playback;

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Media::Multimedia::{
        mciGetErrorStringW, mciSendCommandW, MCI_CLOSE, MCI_MODE_STOP, MCI_OPEN, MCI_OPEN_ELEMENT,
        MCI_OPEN_PARMSW, MCI_OPEN_TYPE, MCI_PLAY, MCI_PLAY_PARMS, MCI_STATUS, MCI_STATUS_ITEM,
        MCI_STATUS_MODE, MCI_STATUS_PARMS, MCI_WAIT,
    };

    fn check(status: u32) -> Result<(), String> {
        if status == 0 {
            return Ok(());
        }
        let mut message = [0; 256];
        if unsafe { mciGetErrorStringW(status, message.as_mut_ptr(), message.len() as u32) } == 0 {
            return Err(format!("Windows 音频错误 {status}"));
        }
        let end = message.iter().position(|c| *c == 0).unwrap_or(message.len());
        Err(String::from_utf16_lossy(&message[..end]))
    }

    pub struct Playback(u32);

    impl Playback {
        pub fn start(path: &Path) -> Result<Self, String> {
            let filename: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut open = MCI_OPEN_PARMSW {
                lpstrDeviceType: windows_sys::w!("waveaudio"),
                lpstrElementName: filename.as_ptr(),
                ..Default::default()
            };
            check(unsafe {
                mciSendCommandW(
                    0,
                    MCI_OPEN,
                    (MCI_OPEN_TYPE | MCI_OPEN_ELEMENT | MCI_WAIT) as usize,
                    (&mut open as *mut MCI_OPEN_PARMSW) as usize,
                )
            })?;
            let player = Self(open.wDeviceID);
            let play = MCI_PLAY_PARMS::default();
            check(unsafe {
                mciSendCommandW(player.0, MCI_PLAY, 0, (&play as *const MCI_PLAY_PARMS) as usize)
            })?;
            Ok(player)
        }

        pub fn is_finished(&mut self) -> Result<bool, String> {
            let mut status = MCI_STATUS_PARMS {
                dwItem: MCI_STATUS_MODE as u32,
                ..Default::default()
            };
            check(unsafe {
                mciSendCommandW(
                    self.0,
                    MCI_STATUS,
                    (MCI_STATUS_ITEM | MCI_WAIT) as usize,
                    (&mut status as *mut MCI_STATUS_PARMS) as usize,
                )
            })?;
            Ok(status.dwReturn == MCI_MODE_STOP as usize)
        }
    }

    impl Drop for Playback {
        fn drop(&mut self) {
            // Closing an open MCI device also stops any playback still in progress.
            unsafe { mciSendCommandW(self.0, MCI_CLOSE, MCI_WAIT as usize, 0) };
        }
    }
}

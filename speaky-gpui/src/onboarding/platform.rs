#[cfg(windows)]
pub(super) const MICROPHONE_SETTINGS: &str = "ms-settings:privacy-microphone";

#[cfg(target_os = "macos")]
pub(super) const MICROPHONE_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

#[cfg(windows)]
pub(super) fn motion_allowed() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
    };
    let mut enabled = 0i32;
    // SPI_GETCLIENTAREAANIMATION writes a Win32 BOOL through pvParam.
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        ) != 0
            && enabled != 0
    }
}

#[cfg(windows)]
pub(super) async fn microphone_permission() -> bool {
    // Win32 microphone access is managed in Windows Settings. The input probe
    // checks what the recorder can actually receive under the current policy.
    true
}

#[cfg(target_os = "macos")]
mod apple {
    use block::ConcreteBlock;
    use objc::{
        class, msg_send,
        runtime::{Object, BOOL, YES},
        sel, sel_impl,
    };
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: *mut Object;
    }

    pub(crate) fn motion_allowed() -> bool {
        // NSWorkspace is available on the GPUI main thread.
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            let reduce: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
            reduce != YES
        }
    }

    pub(crate) async fn microphone_permission() -> bool {
        const NOT_DETERMINED: isize = 0;
        const AUTHORIZED: isize = 3;
        let response = unsafe {
            let device = class!(AVCaptureDevice);
            let status: isize =
                msg_send![device, authorizationStatusForMediaType: AVMediaTypeAudio];
            match status {
                AUTHORIZED => return true,
                NOT_DETERMINED => {
                    let (respond, response) = oneshot::channel();
                    let respond = Mutex::new(Some(respond));
                    // The system can invoke this on a background queue. Only send
                    // the result; the tour updates on GPUI's foreground executor.
                    let handler = ConcreteBlock::new(move |allowed: BOOL| {
                        if let Some(respond) = respond.lock().unwrap().take() {
                            let _ = respond.send(allowed == YES);
                        }
                    })
                    .copy();
                    let _: () = msg_send![device, requestAccessForMediaType: AVMediaTypeAudio completionHandler: &*handler];
                    response
                }
                _ => return false,
            }
        };
        response.await.unwrap_or(false)
    }
}

#[cfg(target_os = "macos")]
pub(super) use apple::{microphone_permission, motion_allowed};

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) const MICROPHONE_SETTINGS: &str = "";

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn motion_allowed() -> bool {
    false
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) async fn microphone_permission() -> bool {
    true
}

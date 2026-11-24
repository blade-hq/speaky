use std::ffi::c_void;

type Ref = *const c_void;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> Ref;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> Ref;
    fn TISGetInputSourceProperty(source: Ref, property: Ref) -> Ref;
    static kTISPropertyUnicodeKeyLayoutData: Ref;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: Ref,
        key: u16,
        action: u16,
        modifiers: u32,
        keyboard: u32,
        options: u32,
        dead: *mut u32,
        capacity: usize,
        count: *mut usize,
        chars: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: Ref) -> *const u8;
    fn CFRelease(value: Ref);
}

struct InputSource(Ref);

impl Drop for InputSource {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn translated_char(layout: Ref, key: u16, modifiers: u32, keyboard: u32) -> Option<char> {
    let mut chars = [0u16; 4];
    let mut count = 0;
    let mut dead = 0;
    let status = unsafe {
        UCKeyTranslate(
            layout,
            key,
            0,
            modifiers,
            keyboard,
            1,
            &mut dead,
            chars.len(),
            &mut count,
            chars.as_mut_ptr(),
        )
    };
    if status != 0 || !(1..=2).contains(&count) {
        return None;
    }
    let mut sequence = char::decode_utf16(chars[..count].iter().copied());
    let c = sequence.next()?.ok()?;
    sequence.next().is_none().then_some(c.to_ascii_lowercase())
}

// Interpret GPUI's printed key using the active layout. GPUI can represent a
// shifted punctuation key as a character with the Shift modifier folded in.
pub(super) fn character_key(c: char, command: bool) -> Result<(u16, char, bool), String> {
    let keyboard = u32::from(unsafe { LMGetKbdType() });
    for get_source in [
        TISCopyCurrentKeyboardLayoutInputSource,
        TISCopyCurrentASCIICapableKeyboardLayoutInputSource,
    ] {
        let source = InputSource(unsafe { get_source() });
        if source.0.is_null() {
            continue;
        }
        let data = unsafe { TISGetInputSourceProperty(source.0, kTISPropertyUnicodeKeyLayoutData) };
        if data.is_null() {
            continue;
        }
        let layout: Ref = unsafe { CFDataGetBytePtr(data) }.cast();
        if layout.is_null() {
            continue;
        }
        // The printable section precedes the modifier, keypad and function keys.
        for shifted in [false, true] {
            let modifiers = u32::from(command) | u32::from(shifted) << 1;
            for key in 0..=50 {
                if translated_char(layout, key, modifiers, keyboard) == Some(c) {
                    let base =
                        translated_char(layout, key, u32::from(command), keyboard).unwrap_or(c);
                    return Ok((key, base, shifted));
                }
            }
        }
    }
    Err(format!("当前键盘布局无法注册字符 {c}。"))
}

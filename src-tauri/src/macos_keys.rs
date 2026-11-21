use crate::settings::Key;
use core_foundation::{
    base::TCFType,
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryRef},
    string::{CFString, CFStringRef},
};
use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use std::process::Command;
use tauri::WebviewWindow;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

pub fn prompt() {
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
    }
}

pub fn open_settings() {
    let _ = Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status();
}

pub type Stroke = u16;

pub fn map(key: &Key) -> Option<Stroke> {
    // Indexed by DOM letter, digit, or keypad digit, in ANSI physical positions.
    const LETTERS: [u16; 26] = [
        0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46,
        45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,
    ];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    const KEYPAD: [u16; 10] = [82, 83, 84, 85, 86, 87, 88, 89, 91, 92];

    match key.code.as_bytes() {
        [b'K', b'e', b'y', c @ b'A'..=b'Z'] => return Some(LETTERS[(*c - b'A') as usize]),
        [b'D', b'i', b'g', b'i', b't', c @ b'0'..=b'9'] => {
            return Some(DIGITS[(*c - b'0') as usize]);
        }
        [b'N', b'u', b'm', b'p', b'a', b'd', c @ b'0'..=b'9'] => {
            return Some(KEYPAD[(*c - b'0') as usize]);
        }
        _ => {}
    }

    Some(match key.code.as_str() {
        "F1" => 122, "F2" => 120, "F3" => 99, "F4" => 118,
        "F5" => 96, "F6" => 97, "F7" => 98, "F8" => 100,
        "F9" => 101, "F10" => 109, "F11" => 103, "F12" => 111,
        "F13" => 105, "F14" => 107, "F15" => 113, "F16" => 106,
        "F17" => 64, "F18" => 79, "F19" => 80, "F20" => 90,
        "ArrowLeft" => 123, "ArrowRight" => 124, "ArrowDown" => 125, "ArrowUp" => 126,
        "Space" => 49, "Enter" => 36, "Tab" => 48, "Escape" => 53,
        "Backspace" => 51, "Delete" => 117, "Insert" => 114,
        "Home" => 115, "End" => 119, "PageUp" => 116, "PageDown" => 121,
        "NumpadAdd" => 69, "NumpadSubtract" => 78, "NumpadMultiply" => 67,
        "NumpadDivide" => 75, "NumpadDecimal" => 65, "NumpadEnter" => 76,
        "MetaLeft" => 55, "MetaRight" => 54, "ShiftLeft" => 56, "ShiftRight" => 60,
        "AltLeft" => 58, "AltRight" => 61, "ControlLeft" => 59, "ControlRight" => 62,
        "CapsLock" => 57, "Minus" => 27, "Equal" => 24, "BracketLeft" => 33,
        "BracketRight" => 30, "Backslash" => 42, "Semicolon" => 41, "Quote" => 39,
        "Comma" => 43, "Period" => 47, "Slash" => 44, "Backquote" => 50,
        "IntlBackslash" => 10, "ContextMenu" => 110,
        _ => return None,
    })
}

// The low bits are Apple's device-dependent left/right modifier flags.
const MODIFIERS: [(Stroke, CGEventFlags, u64); 9] = [
    (56, CGEventFlags::CGEventFlagShift, 0x0002),
    (60, CGEventFlags::CGEventFlagShift, 0x0004),
    (59, CGEventFlags::CGEventFlagControl, 0x0001),
    (62, CGEventFlags::CGEventFlagControl, 0x2000),
    (58, CGEventFlags::CGEventFlagAlternate, 0x0020),
    (61, CGEventFlags::CGEventFlagAlternate, 0x0040),
    (55, CGEventFlags::CGEventFlagCommand, 0x0008),
    (54, CGEventFlags::CGEventFlagCommand, 0x0010),
    (57, CGEventFlags::CGEventFlagAlphaShift, 0),
];

pub struct Keyboard {
    held: [usize; MODIFIERS.len()],
}

impl Keyboard {
    pub fn new(_: &WebviewWindow) -> Self {
        Self { held: [0; MODIFIERS.len()] }
    }

    pub fn send(&mut self, stroke: Stroke, down: bool) -> bool {
        if !trusted() {
            return false;
        }
        let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            return false;
        };
        let Ok(event) = CGEvent::new_keyboard_event(source, stroke, down) else {
            return false;
        };

        let mut bits = event.get_flags().bits();
        if let Some(i) = MODIFIERS.iter().position(|m| m.0 == stroke) {
            let (_, flag, side) = MODIFIERS[i];
            self.held[i] = if down {
                self.held[i].saturating_add(1)
            } else {
                self.held[i].saturating_sub(1)
            };
            bits &= !(flag.bits() | side);
            for &(_, other_flag, other_side) in &MODIFIERS {
                if other_flag == flag && bits & other_side != 0 {
                    bits |= flag.bits();
                }
            }
            event.set_type(CGEventType::FlagsChanged);
        }
        for (i, &(_, flag, side)) in MODIFIERS.iter().enumerate() {
            if self.held[i] != 0 {
                bits |= flag.bits() | side;
            }
        }
        event.set_flags(CGEventFlags::from_bits_retain(bits));
        event.post(CGEventTapLocation::HID);
        true
    }
}

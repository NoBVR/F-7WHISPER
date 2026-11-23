//! STAND-IN for Blur's src-tauri/src/hotkeys.rs (v3.10.2), made by F-7WHISPER on 2026-11-22.
//!
//! Blur's file installs low-level keyboard/mouse hooks and drives its hotkey
//! listener, none of which F-7WHISPER uses. The engine only needs:
//! - `note_injected_key`: Blur uses it to tell its own hook which keys it
//!   injected. F-7 has no Blur hook to inform, so it is a no-op here.
//! - `parse_hotkey_main_key` (used by `build_config`, which F-7 does not call,
//!   and by Blur's tests): copied unchanged from Blur, with its helpers.
//! - `HotkeyBinding` (a field type of `ClickerState`): copied unchanged.

use crate::error::AppError;
use crate::error::AppResult;
#[cfg(windows)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

// macOS: Blur's parser returns Windows VK values on both platforms.
#[cfg(target_os = "macos")]
mod vk {
    pub(super) const VK_LBUTTON: u16 = 0x01;
    pub(super) const VK_RBUTTON: u16 = 0x02;
    pub(super) const VK_MBUTTON: u16 = 0x04;
    pub(super) const VK_XBUTTON1: u16 = 0x05;
    pub(super) const VK_XBUTTON2: u16 = 0x06;
    pub(super) const VK_BACK: u16 = 0x08;
    pub(super) const VK_TAB: u16 = 0x09;
    pub(super) const VK_RETURN: u16 = 0x0d;
    pub(super) const VK_PAUSE: u16 = 0x13;
    pub(super) const VK_CAPITAL: u16 = 0x14;
    pub(super) const VK_ESCAPE: u16 = 0x1b;
    pub(super) const VK_SPACE: u16 = 0x20;
    pub(super) const VK_PRIOR: u16 = 0x21;
    pub(super) const VK_NEXT: u16 = 0x22;
    pub(super) const VK_END: u16 = 0x23;
    pub(super) const VK_HOME: u16 = 0x24;
    pub(super) const VK_LEFT: u16 = 0x25;
    pub(super) const VK_UP: u16 = 0x26;
    pub(super) const VK_RIGHT: u16 = 0x27;
    pub(super) const VK_DOWN: u16 = 0x28;
    pub(super) const VK_SNAPSHOT: u16 = 0x2c;
    pub(super) const VK_INSERT: u16 = 0x2d;
    pub(super) const VK_DELETE: u16 = 0x2e;
    pub(super) const VK_LWIN: u16 = 0x5b;
    pub(super) const VK_RWIN: u16 = 0x5c;
    pub(super) const VK_APPS: u16 = 0x5d;
    pub(super) const VK_NUMPAD0: u16 = 0x60;
    pub(super) const VK_NUMPAD1: u16 = 0x61;
    pub(super) const VK_NUMPAD2: u16 = 0x62;
    pub(super) const VK_NUMPAD3: u16 = 0x63;
    pub(super) const VK_NUMPAD4: u16 = 0x64;
    pub(super) const VK_NUMPAD5: u16 = 0x65;
    pub(super) const VK_NUMPAD6: u16 = 0x66;
    pub(super) const VK_NUMPAD7: u16 = 0x67;
    pub(super) const VK_NUMPAD8: u16 = 0x68;
    pub(super) const VK_NUMPAD9: u16 = 0x69;
    pub(super) const VK_MULTIPLY: u16 = 0x6a;
    pub(super) const VK_ADD: u16 = 0x6b;
    pub(super) const VK_SUBTRACT: u16 = 0x6d;
    pub(super) const VK_DECIMAL: u16 = 0x6e;
    pub(super) const VK_DIVIDE: u16 = 0x6f;
    pub(super) const VK_F1: u16 = 0x70;
    pub(super) const VK_NUMLOCK: u16 = 0x90;
    pub(super) const VK_SCROLL: u16 = 0x91;
    pub(super) const VK_LSHIFT: u16 = 0xa0;
    pub(super) const VK_RSHIFT: u16 = 0xa1;
    pub(super) const VK_LCONTROL: u16 = 0xa2;
    pub(super) const VK_RCONTROL: u16 = 0xa3;
    pub(super) const VK_LMENU: u16 = 0xa4;
    pub(super) const VK_RMENU: u16 = 0xa5;
    pub(super) const VK_OEM_1: u16 = 0xba;
    pub(super) const VK_OEM_PLUS: u16 = 0xbb;
    pub(super) const VK_OEM_COMMA: u16 = 0xbc;
    pub(super) const VK_OEM_MINUS: u16 = 0xbd;
    pub(super) const VK_OEM_PERIOD: u16 = 0xbe;
    pub(super) const VK_OEM_2: u16 = 0xbf;
    pub(super) const VK_OEM_3: u16 = 0xc0;
    pub(super) const VK_OEM_4: u16 = 0xdb;
    pub(super) const VK_OEM_5: u16 = 0xdc;
    pub(super) const VK_OEM_6: u16 = 0xdd;
    pub(super) const VK_OEM_7: u16 = 0xde;
    pub(super) const VK_OEM_102: u16 = 0xe2;
}
#[cfg(target_os = "macos")]
use vk::*;

const PHYSICAL_CODE_KEYS: [(&str, u16, i32); 12] = [
    ("Backquote", 0x29, VK_OEM_3 as i32),
    ("Minus", 0x0C, VK_OEM_MINUS as i32),
    ("Equal", 0x0D, VK_OEM_PLUS as i32),
    ("BracketLeft", 0x1A, VK_OEM_4 as i32),
    ("BracketRight", 0x1B, VK_OEM_6 as i32),
    ("Backslash", 0x2B, VK_OEM_5 as i32),
    ("Semicolon", 0x27, VK_OEM_1 as i32),
    ("Quote", 0x28, VK_OEM_7 as i32),
    ("Comma", 0x33, VK_OEM_COMMA as i32),
    ("Period", 0x34, VK_OEM_PERIOD as i32),
    ("Slash", 0x35, VK_OEM_2 as i32),
    ("IntlBackslash", 0x56, VK_OEM_102 as i32),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    pub left_ctrl: bool,
    pub right_ctrl: bool,
    pub left_alt: bool,
    pub right_alt: bool,
    pub left_shift: bool,
    pub right_shift: bool,
    pub left_super: bool,
    pub right_super: bool,
    pub main_vks: Vec<i32>,
    pub key_tokens: Vec<String>,
}

impl HotkeyBinding {
    pub fn main_vk(&self) -> Option<i32> {
        self.main_vks.first().copied()
    }
    pub fn key_token(&self) -> &str {
        self.key_tokens.first().map(|s| s.as_str()).unwrap_or("")
    }
}

/// No-op: F-7WHISPER has no Blur keyboard hook that needs to skip injected keys.
pub fn note_injected_key(_vk: i32) {}

pub fn parse_hotkey_main_key(token: &str, original_hotkey: &str) -> AppResult<(i32, String)> {
    let lower = token.trim().to_ascii_lowercase();

    if let Some(binding) = parse_physical_code_token(&lower) {
        return Ok(binding);
    }

    if let Some(binding) = parse_named_key_token(&lower) {
        return Ok(binding);
    }

    if let Some(binding) = parse_mouse_button_token(&lower) {
        return Ok(binding);
    }

    if let Some(binding) = parse_numpad_token(&lower) {
        return Ok(binding);
    }

    if let Some(binding) = parse_function_key_token(&lower) {
        return Ok(binding);
    }

    if let Some(letter) = lower.strip_prefix("key") {
        if letter.len() == 1 {
            return parse_hotkey_main_key(letter, original_hotkey);
        }
    }

    if let Some(digit) = lower.strip_prefix("digit") {
        if digit.len() == 1 {
            return parse_hotkey_main_key(digit, original_hotkey);
        }
    }

    let mut chars = lower.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        if ch.is_ascii_lowercase() {
            return Ok((ch.to_ascii_uppercase() as i32, lower));
        }
        if ch.is_ascii_digit() {
            return Ok((ch as i32, lower));
        }
        // macOS: the named and physical token tables above cover the supported keys.
        #[cfg(windows)]
        if let Ok(wide) = u16::try_from(ch as u32) {
            let scan = unsafe { VkKeyScanW(wide) };
            if scan != -1 {
                let raw = scan as u16;
                if raw >> 8 != 0 {
                    return Err(AppError::Hotkey(format!(
                        "Invalid hotkey '{original_hotkey}': '{token}' is only reachable with a modifier on this keyboard layout"
                    )));
                }
                return Ok(((raw & 0xFF) as i32, lower));
            }
        }
        return Err(AppError::Hotkey(format!(
            "Invalid hotkey '{original_hotkey}': '{token}' isn't reachable on the current keyboard layout"
        )));
    }

    Err(AppError::Hotkey(format!(
        "Couldn't recognize '{token}' as a valid key in '{original_hotkey}'"
    )))
}

fn binding(vk: i32, token: &str) -> (i32, String) {
    (vk, token.to_string())
}

fn parse_physical_code_token(token: &str) -> Option<(i32, String)> {
    let (name, scan, fallback_vk) = *PHYSICAL_CODE_KEYS
        .iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(token))?;
    #[cfg(windows)]
    let mapped = unsafe { MapVirtualKeyW(scan as u32, MAPVK_VSC_TO_VK_EX) } as i32;
    // macOS: physical OEM tokens carry their VK in PHYSICAL_CODE_KEYS.
    #[cfg(target_os = "macos")]
    let mapped = fallback_vk;
    let vk = if mapped == 0 { fallback_vk } else { mapped };
    Some(binding(vk, name))
}

fn parse_named_key_token(token: &str) -> Option<(i32, String)> {
    match token {
        "<" | ">" | "intlbackslash" | "oem102" | "nonusbackslash" => {
            Some(binding(VK_OEM_102 as i32, "IntlBackslash"))
        }
        "space" | "spacebar" => Some(binding(VK_SPACE as i32, "space")),
        "tab" => Some(binding(VK_TAB as i32, "tab")),
        "enter" | "return" => Some(binding(VK_RETURN as i32, "enter")),
        "backspace" => Some(binding(VK_BACK as i32, "backspace")),
        "delete" | "del" => Some(binding(VK_DELETE as i32, "delete")),
        "insert" | "ins" => Some(binding(VK_INSERT as i32, "insert")),
        "home" => Some(binding(VK_HOME as i32, "home")),
        "end" => Some(binding(VK_END as i32, "end")),
        "pageup" | "pgup" => Some(binding(VK_PRIOR as i32, "pageup")),
        "pagedown" | "pgdn" => Some(binding(VK_NEXT as i32, "pagedown")),
        "up" | "arrowup" => Some(binding(VK_UP as i32, "up")),
        "down" | "arrowdown" => Some(binding(VK_DOWN as i32, "down")),
        "left" | "arrowleft" => Some(binding(VK_LEFT as i32, "left")),
        "right" | "arrowright" => Some(binding(VK_RIGHT as i32, "right")),
        "esc" | "escape" => Some(binding(VK_ESCAPE as i32, "escape")),
        "leftctrl" | "ctrlleft" | "lctrl" => Some(binding(VK_LCONTROL as i32, "leftctrl")),
        "rightctrl" | "ctrlright" | "rctrl" => Some(binding(VK_RCONTROL as i32, "rightctrl")),
        "leftshift" | "shiftleft" | "lshift" => Some(binding(VK_LSHIFT as i32, "leftshift")),
        "rightshift" | "shiftright" | "rshift" => Some(binding(VK_RSHIFT as i32, "rightshift")),
        "leftalt" | "altleft" | "lalt" => Some(binding(VK_LMENU as i32, "leftalt")),
        "rightalt" | "altright" | "ralt" | "altgr" => Some(binding(VK_RMENU as i32, "rightalt")),
        "leftsuper" | "superleft" | "leftwin" | "winleft" | "lwin" => {
            Some(binding(VK_LWIN as i32, "leftsuper"))
        }
        "rightsuper" | "superright" | "rightwin" | "winright" | "rwin" => {
            Some(binding(VK_RWIN as i32, "rightsuper"))
        }
        "capslock" => Some(binding(VK_CAPITAL as i32, "capslock")),
        "numlock" => Some(binding(VK_NUMLOCK as i32, "numlock")),
        "scrolllock" => Some(binding(VK_SCROLL as i32, "scrolllock")),
        "menu" | "apps" | "contextmenu" => Some(binding(VK_APPS as i32, "menu")),
        "printscreen" | "prtsc" | "snapshot" => Some(binding(VK_SNAPSHOT as i32, "printscreen")),
        "pause" | "break" => Some(binding(VK_PAUSE as i32, "pause")),
        "/" | "slash" => Some(binding(VK_OEM_2 as i32, "/")),
        "\\" | "backslash" => Some(binding(VK_OEM_5 as i32, "\\")),
        ";" | "semicolon" => Some(binding(VK_OEM_1 as i32, ";")),
        "'" | "quote" | "apostrophe" => Some(binding(VK_OEM_7 as i32, "'")),
        "[" | "bracketleft" => Some(binding(VK_OEM_4 as i32, "[")),
        "]" | "bracketright" => Some(binding(VK_OEM_6 as i32, "]")),
        "-" | "minus" => Some(binding(VK_OEM_MINUS as i32, "-")),
        "=" | "equal" => Some(binding(VK_OEM_PLUS as i32, "=")),
        "`" | "backquote" | "grave" => Some(binding(VK_OEM_3 as i32, "`")),
        "," | "comma" => Some(binding(VK_OEM_COMMA as i32, ",")),
        "." | "period" | "dot" => Some(binding(VK_OEM_PERIOD as i32, ".")),
        _ => None,
    }
}

fn parse_mouse_button_token(token: &str) -> Option<(i32, String)> {
    match token {
        "mouseleft" | "leftmouse" | "leftbutton" | "mouse1" | "lmb" => {
            Some(binding(VK_LBUTTON as i32, "mouseleft"))
        }
        "mouseright" | "rightmouse" | "rightbutton" | "mouse2" | "rmb" => {
            Some(binding(VK_RBUTTON as i32, "mouseright"))
        }
        "mousemiddle" | "middlemouse" | "middlebutton" | "mouse3" | "mmb" | "scrollbutton"
        | "middleclick" => Some(binding(VK_MBUTTON as i32, "mousemiddle")),
        "mouse4" | "xbutton1" | "mouseback" | "browserback" | "backbutton" => {
            Some(binding(VK_XBUTTON1 as i32, "mouse4"))
        }
        "mouse5" | "xbutton2" | "mouseforward" | "browserforward" | "forwardbutton" => {
            Some(binding(VK_XBUTTON2 as i32, "mouse5"))
        }
        _ => None,
    }
}

fn parse_numpad_token(token: &str) -> Option<(i32, String)> {
    match token {
        "numpad0" | "num0" => Some(binding(VK_NUMPAD0 as i32, "numpad0")),
        "numpad1" | "num1" => Some(binding(VK_NUMPAD1 as i32, "numpad1")),
        "numpad2" | "num2" => Some(binding(VK_NUMPAD2 as i32, "numpad2")),
        "numpad3" | "num3" => Some(binding(VK_NUMPAD3 as i32, "numpad3")),
        "numpad4" | "num4" => Some(binding(VK_NUMPAD4 as i32, "numpad4")),
        "numpad5" | "num5" => Some(binding(VK_NUMPAD5 as i32, "numpad5")),
        "numpad6" | "num6" => Some(binding(VK_NUMPAD6 as i32, "numpad6")),
        "numpad7" | "num7" => Some(binding(VK_NUMPAD7 as i32, "numpad7")),
        "numpad8" | "num8" => Some(binding(VK_NUMPAD8 as i32, "numpad8")),
        "numpad9" | "num9" => Some(binding(VK_NUMPAD9 as i32, "numpad9")),
        "numpadadd" | "numadd" | "numpadplus" | "numplus" => {
            Some(binding(VK_ADD as i32, "numpadadd"))
        }
        "numpadsubtract" | "numsubtract" | "numsub" | "numpadminus" | "numminus" => {
            Some(binding(VK_SUBTRACT as i32, "numpadsubtract"))
        }
        "numpadmultiply" | "nummultiply" | "nummul" | "numpadmul" => {
            Some(binding(VK_MULTIPLY as i32, "numpadmultiply"))
        }
        "numpaddivide" | "numdivide" | "numdiv" | "numpaddiv" => {
            Some(binding(VK_DIVIDE as i32, "numpaddivide"))
        }
        "numpaddecimal" | "numdecimal" | "numdot" | "numdel" | "numpadpoint" => {
            Some(binding(VK_DECIMAL as i32, "numpaddecimal"))
        }
        _ => None,
    }
}

fn parse_function_key_token(token: &str) -> Option<(i32, String)> {
    if !token.starts_with('f') || token.len() > 3 {
        return None;
    }

    let number = token[1..].parse::<i32>().ok()?;
    let vk = match number {
        1..=24 => VK_F1 as i32 + (number - 1),
        _ => return None,
    };

    Some(binding(vk, token))
}

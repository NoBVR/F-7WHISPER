use crate::settings::Key;
use std::mem::size_of;
use tauri::WebviewWindow;
use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC_EX,
        VIRTUAL_KEY,
    },
    WindowsAndMessaging::{GetAncestor, GetForegroundWindow, GA_ROOTOWNER},
};

#[derive(Clone, Copy)]
pub struct Stroke {
    vk: u16,
    scan: u16,
    extended: bool,
}

pub fn map(key: &Key) -> Option<Stroke> {
    let code = key.code.as_str();
    let vk = match code {
        "ShiftLeft" => 0xa0,
        "ShiftRight" => 0xa1,
        "ControlLeft" => 0xa2,
        "ControlRight" => 0xa3,
        "AltLeft" => 0xa4,
        "AltRight" => 0xa5,
        "MetaLeft" => 0x5b,
        "MetaRight" => 0x5c,
        "NumpadEnter" => 0x0d,
        "NumpadDivide" => 0x6f,
        "NumpadDecimal" => 0x6e,
        _ => match code.strip_prefix("Numpad").map(str::as_bytes) {
            Some([digit @ b'0'..=b'9']) => 0x60 + u16::from(*digit - b'0'),
            _ => key.vk,
        },
    };

    let mapped = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX) };
    let extended = matches!(mapped >> 8, 0xe0 | 0xe1)
        || matches!(
            code,
            "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Insert" | "Delete"
                | "Home" | "End" | "PageUp" | "PageDown" | "NumpadEnter" | "NumpadDivide"
                | "ControlRight" | "AltRight" | "MetaLeft" | "MetaRight" | "ContextMenu"
        );
    let scan = (mapped & 0xff) as u16;
    let raw_vk = code.strip_prefix("Vk").and_then(|n| n.parse::<u16>().ok()) == Some(vk);
    (scan != 0 || raw_vk).then_some(Stroke { vk, scan, extended })
}

pub struct Keyboard {
    owner: usize,
}

impl Keyboard {
    pub fn new(window: &WebviewWindow) -> Self {
        Self {
            owner: window.hwnd().map(|hwnd| hwnd.0 as usize).unwrap_or(0),
        }
    }

    pub fn send(&mut self, stroke: Stroke, down: bool) -> bool {
        unsafe {
            if down {
                let foreground = GetForegroundWindow();
                let root = GetAncestor(foreground, GA_ROOTOWNER);
                if self.owner == 0
                    || foreground.0.is_null()
                    || root.0.is_null()
                    || foreground.0 as usize == self.owner
                    || root.0 as usize == self.owner
                {
                    return false;
                }
            }
            let mut flags = KEYBD_EVENT_FLAGS::default();
            if stroke.scan != 0 {
                flags |= KEYEVENTF_SCANCODE;
            }
            if stroke.extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(stroke.vk),
                        wScan: stroke.scan,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            };
            SendInput(&[input], size_of::<INPUT>() as i32) == 1
        }
    }
}

pub fn trusted() -> bool {
    true
}

pub fn prompt() {}

pub fn open_settings() {}

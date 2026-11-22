use crate::settings::{Button, Point};
use std::{collections::BTreeSet, mem::size_of};
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::{CloseHandle, BOOL, HWND, LPARAM, POINT},
        Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE,
                MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
                MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
                MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
                MOUSE_EVENT_FLAGS, VK_LBUTTON,
            },
            WindowsAndMessaging::{
                EnumWindows, GetAncestor, GetCursorPos, GetForegroundWindow, GetSystemMetrics,
                GetWindowLongW, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
                WindowFromPoint, GA_ROOTOWNER, GWL_EXSTYLE, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
                SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WS_EX_TOOLWINDOW, XBUTTON1, XBUTTON2,
            },
        },
    },
};

fn pid(hwnd: HWND) -> Option<u32> {
    if hwnd.is_invalid() {
        return None;
    }
    let mut id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut id)) };
    (id != 0).then_some(id)
}

fn owner(hwnd: HWND) -> Option<u32> {
    let id = pid(hwnd)?;
    let root = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
    if pid(root) == Some(std::process::id()) {
        Some(std::process::id())
    } else {
        Some(id)
    }
}

pub fn foreground_pid() -> Option<u32> {
    owner(unsafe { GetForegroundWindow() })
}

pub fn process_name(id: u32) -> Option<String> {
    let mut path = vec![0u16; 32_768];
    let mut length = path.len() as u32;
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id).ok()?;
        let result = QueryFullProcessImageNameW(
            process, PROCESS_NAME_WIN32, PWSTR(path.as_mut_ptr()), &mut length,
        );
        let _ = CloseHandle(process);
        result.ok()?;
    }
    let name = path[..length as usize]
        .rsplit(|&c| c == b'\\' as u16 || c == b'/' as u16).next()?;
    Some(String::from_utf16_lossy(name))
}

pub fn apps() -> Vec<String> {
    unsafe extern "system" fn visit(hwnd: HWND, context: LPARAM) -> BOOL {
        if IsWindowVisible(hwnd).as_bool()
            && GetWindowTextLengthW(hwnd) > 0
            && (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) == 0
        {
            let mut cloaked = 0u32;
            let result = DwmGetWindowAttribute(
                hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), size_of::<u32>() as u32,
            );
            if result.is_ok() && cloaked == 0 {
                if let Some(id) = owner(hwnd).filter(|id| *id != std::process::id()) {
                    (*(context.0 as *mut BTreeSet<u32>)).insert(id);
                }
            }
        }
        BOOL(1)
    }

    let mut ids = BTreeSet::<u32>::new();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM((&mut ids as *mut BTreeSet<u32>) as isize));
    }
    ids.into_iter().filter_map(process_name).collect()
}

pub fn left_down() -> bool {
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 }
}

pub fn outside_point() -> Option<Point> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok()?;
    let id = owner(unsafe { WindowFromPoint(p) })?;
    (id != std::process::id()).then_some(Point { x: p.x, y: p.y })
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: u32, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, ..Default::default() },
        },
    }
}

fn absolute(pixel: i32, origin: i32, extent: i32) -> Option<i32> {
    if extent <= 0 {
        return None;
    }
    let extent = i64::from(extent);
    let offset = (i64::from(pixel) - i64::from(origin)).clamp(0, extent - 1);
    Some(((2 * offset + 1) * 65_536 / (2 * extent)).min(65_535) as i32)
}

pub fn click(button: Button, at: Option<Point>, down: bool) -> bool {
    let (press, release, data) = match button {
        Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        Button::Back => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, u32::from(XBUTTON1)),
        Button::Forward => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, u32::from(XBUTTON2)),
    };
    let button_input = mouse(if down { press } else { release }, data, 0, 0);
    let size = size_of::<INPUT>() as i32;
    unsafe {
        if let Some(p) = at.filter(|_| down) {
            let Some(x) = absolute(p.x, GetSystemMetrics(SM_XVIRTUALSCREEN),
                                   GetSystemMetrics(SM_CXVIRTUALSCREEN)) else { return false };
            let Some(y) = absolute(p.y, GetSystemMetrics(SM_YVIRTUALSCREEN),
                                   GetSystemMetrics(SM_CYVIRTUALSCREEN)) else { return false };
            let movement = mouse(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, 0, x, y,
            );
            return SendInput(&[movement, button_input], size) == 2;
        }
        SendInput(&[button_input], size) == 1
    }
}

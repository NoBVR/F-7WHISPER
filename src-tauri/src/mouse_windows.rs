use super::{modifiers, Tracker};
use std::{
    cell::RefCell,
    sync::mpsc::Sender,
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
            },
            WindowsAndMessaging::{
                CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
                SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, LLMHF_INJECTED,
                MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, WH_MOUSE_LL, WM_APP, WM_XBUTTONDOWN,
                WM_XBUTTONUP, XBUTTON1, XBUTTON2,
            },
        },
    },
};

const STOP: u32 = WM_APP + 0x3f7;

thread_local! {
    static ACTIVE: RefCell<Option<Tracker>> = const { RefCell::new(None) };
}

fn pressed(key: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key.0)) < 0 }
}

unsafe extern "system" fn callback(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let message = wp.0 as u32;
    if code == HC_ACTION as i32 && matches!(message, WM_XBUTTONDOWN | WM_XBUTTONUP) {
        let event = &*(lp.0 as *const MSLLHOOKSTRUCT);
        if event.flags & LLMHF_INJECTED == 0 {
            let side = match (event.mouseData >> 16) as u16 {
                XBUTTON1 => Some(0),
                XBUTTON2 => Some(1),
                _ => None,
            };
            if let Some(side) = side {
                let down = message == WM_XBUTTONDOWN;
                let mods = if down {
                    modifiers(
                        pressed(VK_CONTROL),
                        pressed(VK_MENU),
                        pressed(VK_SHIFT),
                        pressed(VK_LWIN) || pressed(VK_RWIN),
                    )
                } else {
                    0
                };
                let claimed = ACTIVE.with(|cell| {
                    cell.borrow().as_ref().is_some_and(|tracker| tracker.edge(side, mods, down))
                });
                if claimed {
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(HHOOK::default(), code, wp, lp)
}

pub(super) struct Stop(u32);

impl Stop {
    pub(super) fn signal(&self, thread: &JoinHandle<()>) {
        // The queue exists before the startup acknowledgement. A full queue
        // can temporarily reject a post, so retry while the worker is alive.
        while !thread.is_finished() {
            if unsafe { PostThreadMessageW(self.0, STOP, WPARAM(0), LPARAM(0)) }.is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

pub(super) fn run(tracker: Tracker, ready: Sender<Stop>) {
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return };
        let mut message = MSG::default();
        let _ = PeekMessageW(&mut message, HWND::default(), 0, 0, PM_NOREMOVE);
        ACTIVE.with(|cell| *cell.borrow_mut() = Some(tracker));

        let Ok(hook) = SetWindowsHookExW(WH_MOUSE_LL, Some(callback), module, 0) else {
            return;
        };

        if ready.send(Stop(GetCurrentThreadId())).is_ok() {
            while GetMessageW(&mut message, HWND::default(), 0, 0).0 > 0 {
                if message.hwnd.is_invalid() && message.message == STOP {
                    break;
                }
                DispatchMessageW(&message);
            }
        }
        let _ = UnhookWindowsHookEx(hook);
        ACTIVE.with(|cell| {
            cell.borrow_mut().take();
        });
    }
}

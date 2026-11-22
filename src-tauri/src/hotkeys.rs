use crate::{
    settings::{Hotkey, Hotkeys},
    Inbox, Message,
};
use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[derive(Clone, Copy)]
enum Registration {
    Plugin(Shortcut),
    #[cfg(windows)]
    Native(u32),
}

pub struct Bindings {
    plugin: bool,
    generation: u64,
    registered: [Option<Registration>; 2],
    pub ok: [bool; 2],
    #[cfg(windows)]
    hook: Option<native::Hook>,
}

fn plugin_shortcut(h: &Hotkey) -> Option<Shortcut> {
    #[cfg(target_os = "macos")]
    let code: Code = h.key.code.parse().ok()?;

    // The plugin maps DOM codes through a fixed VK table. Select by the
    // captured VK on Windows so alternate keyboard layouts keep their binding.
    #[cfg(windows)]
    let code: Code = match h.key.vk {
        0x30..=0x39 => format!("Digit{}", char::from(h.key.vk as u8)),
        0x41..=0x5a => format!("Key{}", char::from(h.key.vk as u8)),
        0x60..=0x69 => format!("Numpad{}", h.key.vk - 0x60),
        0x70..=0x87 => format!("F{}", h.key.vk - 0x70 + 1),
        _ => return None,
    }.parse().ok()?;

    let mut mods = Modifiers::empty();
    if h.ctrl { mods |= Modifiers::CONTROL; }
    if h.alt { mods |= Modifiers::ALT; }
    if h.shift { mods |= Modifiers::SHIFT; }
    if h.meta { mods |= Modifiers::SUPER; }
    Some(Shortcut::new(Some(mods), code))
}

impl Bindings {
    pub fn new(window: &WebviewWindow) -> Self {
        let app = window.app_handle();
        Self {
            plugin: app.plugin(tauri_plugin_global_shortcut::Builder::new().build()).is_ok(),
            generation: 0,
            registered: [None; 2],
            ok: [false; 2],
            #[cfg(windows)]
            hook: native::Hook::install(window),
        }
    }

    fn clear(&mut self, app: &AppHandle) {
        self.generation = self.generation.wrapping_add(1);
        self.ok = [false; 2];
        for (slot, entry) in self.registered.iter_mut().enumerate() {
            let removed = match *entry {
                None => true,
                Some(Registration::Plugin(shortcut)) => {
                    app.global_shortcut().unregister(shortcut).is_ok()
                }
                #[cfg(windows)]
                Some(Registration::Native(_)) => {
                    self.hook.as_ref().is_some_and(|hook| hook.unregister(slot))
                }
            };
            if removed {
                *entry = None;
            }
            #[cfg(target_os = "macos")]
            let _ = slot;
        }
    }

    pub fn apply(&mut self, app: &AppHandle, keys: &Hotkeys, paused: bool) {
        // Clear both before installing either, including when the slots are swapped.
        self.clear(app);
        if paused {
            return;
        }
        let generation = self.generation;
        for (slot, key) in [keys.start.as_ref(), keys.stop.as_ref()].into_iter().enumerate() {
            let Some(key) = key else { continue };
            if self.registered[slot].is_some() {
                continue;
            }
            if let Some(shortcut) = plugin_shortcut(key).filter(|_| self.plugin) {
                let result = app.global_shortcut().on_shortcut(shortcut, move |app, _, event| {
                    if event.state == ShortcutState::Pressed {
                        app.state::<Inbox>().post(Message::PluginHotkey { slot, generation });
                    }
                });
                if result.is_ok() {
                    self.registered[slot] = Some(Registration::Plugin(shortcut));
                }
            } else {
                #[cfg(windows)]
                if let Some(hook) = &self.hook {
                    self.registered[slot] = hook.register(slot, key).map(Registration::Native);
                }
            }
            self.ok[slot] = self.registered[slot].is_some();
        }
    }

    pub fn accepts_plugin(&self, slot: usize, generation: u64) -> bool {
        generation == self.generation && self.ok.get(slot) == Some(&true)
            && matches!(self.registered[slot], Some(Registration::Plugin(_)))
    }

    #[cfg(windows)]
    pub fn accepts_native(&self, slot: usize, signature: u32) -> bool {
        self.ok.get(slot) == Some(&true)
            && matches!(self.registered[slot], Some(Registration::Native(expected)) if expected == signature)
    }
}

#[cfg(windows)]
mod native {
    use super::{Hotkey, Inbox, Message};
    use std::cell::RefCell;
    use tauri::{Manager, WebviewWindow};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{
                RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
                MOD_WIN,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                SC_MAXIMIZE, SC_SIZE, WM_HOTKEY, WM_NCDESTROY, WM_SYSCOMMAND,
            },
        },
    };

    const SUBCLASS: usize = 0xf701;
    const FIRST: usize = 0x3700;

    thread_local! {
        static MESSAGES: RefCell<Option<Inbox>> = const { RefCell::new(None) };
    }

    pub struct Hook(HWND);

    unsafe extern "system" fn procedure(
        hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM, subclass: usize, _: usize,
    ) -> LRESULT {
        if message == WM_HOTKEY && (FIRST..FIRST + 2).contains(&wp.0) {
            MESSAGES.with(|cell| {
                if let Some(inbox) = cell.borrow().as_ref() {
                    inbox.post(Message::NativeHotkey {
                        slot: wp.0 - FIRST,
                        signature: lp.0 as u32 & 0xffff_000f,
                    });
                }
            });
            return LRESULT(0);
        }
        if message == WM_SYSCOMMAND && matches!(wp.0 as u32 & 0xfff0, SC_MAXIMIZE | SC_SIZE) {
            return LRESULT(0);
        }
        if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(procedure), subclass);
            MESSAGES.with(|cell| { cell.borrow_mut().take(); });
        }
        DefSubclassProc(hwnd, message, wp, lp)
    }

    impl Hook {
        pub fn install(window: &WebviewWindow) -> Option<Self> {
            let hwnd = HWND(window.hwnd().ok()?.0.cast());
            if !unsafe { SetWindowSubclass(hwnd, Some(procedure), SUBCLASS, 0) }.as_bool() {
                return None;
            }
            let inbox = window.app_handle().state::<Inbox>().inner().clone();
            MESSAGES.with(|cell| { *cell.borrow_mut() = Some(inbox); });
            Some(Self(hwnd))
        }

        pub fn register(&self, slot: usize, key: &Hotkey) -> Option<u32> {
            let mut flags = MOD_NOREPEAT;
            if key.ctrl { flags |= MOD_CONTROL; }
            if key.alt { flags |= MOD_ALT; }
            if key.shift { flags |= MOD_SHIFT; }
            if key.meta { flags |= MOD_WIN; }
            let vk = u32::from(key.key.vk);
            unsafe { RegisterHotKey(self.0, (FIRST + slot) as i32, flags, vk) }.ok()?;
            Some((vk << 16) | (flags.0 & 0x000f))
        }

        pub fn unregister(&self, slot: usize) -> bool {
            unsafe { UnregisterHotKey(self.0, (FIRST + slot) as i32) }.is_ok()
        }
    }
}

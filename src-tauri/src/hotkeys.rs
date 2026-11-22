use crate::{
    settings::{Hotkey, Hotkeys},
    Inbox, Message,
};
use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Most distinct activation keys registered at once.
const MAX_TRIGGERS: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Start,
    Stop,
    Trigger,
}

#[derive(Clone, Copy)]
enum Registration {
    Plugin(Shortcut),
    #[cfg(windows)]
    Native { id: usize, signature: u32 },
}

impl Registration {
    fn plugin(&self) -> bool {
        match self {
            Self::Plugin(_) => true,
            #[cfg(windows)]
            Self::Native { .. } => false,
        }
    }
}

struct Entry {
    key: Hotkey,
    /// None after a failed unregistration: the key may still be held by us, but does nothing.
    role: Option<Role>,
    /// Unique per registration, so events from an older registration are ignored.
    token: u64,
    registration: Registration,
    /// A trigger press was seen and its release has not been.
    down: bool,
}

pub enum Hit {
    StartAll,
    StopAll,
    Down(Hotkey),
    Up(Hotkey),
}

pub struct Bindings {
    plugin: bool,
    tokens: u64,
    entries: Vec<Entry>,
    #[cfg(windows)]
    hook: Option<native::Hook>,
    #[cfg(windows)]
    inbox: Inbox,
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
            tokens: 0,
            entries: Vec::new(),
            #[cfg(windows)]
            hook: native::Hook::install(window),
            #[cfg(windows)]
            inbox: app.state::<Inbox>().inner().clone(),
        }
    }

    /// Registers Start all, Stop all and each distinct trigger (none while paused).
    /// Unchanged registrations are kept, so a held trigger still sees its release.
    /// Returns the triggers that were held down when they stopped being registered.
    pub fn apply(
        &mut self, app: &AppHandle, keys: &Hotkeys, triggers: &[Hotkey], paused: bool,
    ) -> Vec<Hotkey> {
        let mut wanted: Vec<(Role, &Hotkey)> = Vec::new();
        if !paused {
            wanted.extend(keys.start.iter().map(|key| (Role::Start, key)));
            wanted.extend(keys.stop.iter().map(|key| (Role::Stop, key)));
            let mut count = 0;
            for key in triggers {
                // A trigger equal to Start all or Stop all is not registered.
                if count < MAX_TRIGGERS && !wanted.iter().any(|(_, k)| k.same_binding(key)) {
                    wanted.push((Role::Trigger, key));
                    count += 1;
                }
            }
        }

        // Remove first, so a key can move between roles.
        let mut released = Vec::new();
        let mut kept = Vec::with_capacity(self.entries.len());
        for mut entry in std::mem::take(&mut self.entries) {
            let same = wanted.iter().find(|(role, key)| {
                entry.role == Some(*role) && key.same_binding(&entry.key)
            });
            if let Some((_, key)) = same {
                entry.key = (*key).clone();
                kept.push(entry);
                continue;
            }
            if entry.down && entry.role == Some(Role::Trigger) {
                released.push(entry.key.clone());
            }
            if !self.unregister(app, &entry) {
                entry.role = None;
                entry.down = false;
                kept.push(entry);
            }
        }
        self.entries = kept;

        for (role, key) in wanted {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.key.same_binding(key)) {
                // Kept with this role, or a registration that would not go away: reuse it.
                entry.role.get_or_insert(role);
                continue;
            }
            if let Some(entry) = self.register(app, role, key) {
                self.entries.push(entry);
            }
        }
        released
    }

    fn register(&mut self, app: &AppHandle, role: Role, key: &Hotkey) -> Option<Entry> {
        self.tokens = self.tokens.wrapping_add(1);
        let token = self.tokens;
        let registration = match plugin_shortcut(key).filter(|_| self.plugin) {
            Some(shortcut) => {
                app.global_shortcut().on_shortcut(shortcut, move |app, _, event| {
                    app.state::<Inbox>().post(Message::PluginHotkey {
                        token,
                        pressed: event.state == ShortcutState::Pressed,
                    });
                }).ok()?;
                Registration::Plugin(shortcut)
            }
            None => self.register_native(key)?,
        };
        Some(Entry { key: key.clone(), role: Some(role), token, registration, down: false })
    }

    #[cfg(windows)]
    fn register_native(&self, key: &Hotkey) -> Option<Registration> {
        let hook = self.hook.as_ref()?;
        // Walk the id range so a just-released id is not reused at once.
        let used = |id: usize| self.entries.iter().any(|e| {
            matches!(e.registration, Registration::Native { id: taken, .. } if taken == id)
        });
        let start = self.tokens as usize;
        let id = (0..native::SLOTS)
            .map(|i| native::FIRST + start.wrapping_add(i) % native::SLOTS)
            .find(|id| !used(*id))?;
        let signature = hook.register(id, key)?;
        Some(Registration::Native { id, signature })
    }

    #[cfg(not(windows))]
    fn register_native(&self, _: &Hotkey) -> Option<Registration> {
        None
    }

    fn unregister(&self, app: &AppHandle, entry: &Entry) -> bool {
        match entry.registration {
            Registration::Plugin(shortcut) => app.global_shortcut().unregister(shortcut).is_ok(),
            #[cfg(windows)]
            Registration::Native { id, .. } => {
                self.hook.as_ref().is_some_and(|hook| hook.unregister(id))
            }
        }
    }

    fn registered(&self, role: Role) -> bool {
        self.entries.iter().any(|e| e.role == Some(role))
    }

    /// Whether Start all and Stop all are registered.
    pub fn ok(&self) -> (bool, bool) {
        (self.registered(Role::Start), self.registered(Role::Stop))
    }

    pub fn trigger_ok(&self, key: &Hotkey) -> bool {
        self.entries.iter().any(|e| e.role == Some(Role::Trigger) && e.key.same_binding(key))
    }

    pub fn plugin_event(&mut self, token: u64, pressed: bool) -> Option<Hit> {
        let index = self.entries.iter()
            .position(|e| e.token == token && e.registration.plugin())?;
        if pressed {
            self.press(index)
        } else if cfg!(windows) {
            // Windows watches the release itself (see `native::watch_release`).
            None
        } else {
            self.release(index)
        }
    }

    #[cfg(windows)]
    pub fn native_event(&mut self, id: usize, signature: u32) -> Option<Hit> {
        let index = self.entries.iter().position(|e| matches!(
            e.registration,
            Registration::Native { id: i, signature: s } if i == id && s == signature
        ))?;
        self.press(index)
    }

    #[cfg(windows)]
    pub fn released(&mut self, token: u64) -> Option<Hit> {
        let index = self.entries.iter().position(|e| e.token == token)?;
        self.release(index)
    }

    fn press(&mut self, index: usize) -> Option<Hit> {
        let entry = &mut self.entries[index];
        match entry.role? {
            Role::Start => Some(Hit::StartAll),
            Role::Stop => Some(Hit::StopAll),
            // Auto-repeat, or a press whose release has not been seen yet.
            Role::Trigger if entry.down => None,
            Role::Trigger => {
                entry.down = true;
                #[cfg(windows)]
                native::watch_release(self.inbox.clone(), entry.key.key.vk, entry.token);
                Some(Hit::Down(entry.key.clone()))
            }
        }
    }

    fn release(&mut self, index: usize) -> Option<Hit> {
        let entry = &mut self.entries[index];
        (entry.role == Some(Role::Trigger) && std::mem::take(&mut entry.down))
            .then(|| Hit::Up(entry.key.clone()))
    }
}

#[cfg(windows)]
mod native {
    use super::{Hotkey, Inbox, Message};
    use std::{cell::RefCell, thread, time::Duration};
    use tauri::{Manager, WebviewWindow};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL,
                MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                SC_MAXIMIZE, SC_SIZE, WM_HOTKEY, WM_NCDESTROY, WM_SYSCOMMAND,
            },
        },
    };

    const SUBCLASS: usize = 0xf701;
    /// Hotkey ids FIRST..FIRST + SLOTS belong to us (application ids end at 0xBFFF).
    pub const FIRST: usize = 0x3700;
    pub const SLOTS: usize = 0x400;
    const RELEASE_POLL: Duration = Duration::from_millis(10);

    thread_local! {
        static MESSAGES: RefCell<Option<Inbox>> = const { RefCell::new(None) };
    }

    pub struct Hook(HWND);

    unsafe extern "system" fn procedure(
        hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM, subclass: usize, _: usize,
    ) -> LRESULT {
        if message == WM_HOTKEY && (FIRST..FIRST + SLOTS).contains(&wp.0) {
            MESSAGES.with(|cell| {
                if let Some(inbox) = cell.borrow().as_ref() {
                    inbox.post(Message::NativeHotkey {
                        id: wp.0,
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

        pub fn register(&self, id: usize, key: &Hotkey) -> Option<u32> {
            let mut flags = MOD_NOREPEAT;
            if key.ctrl { flags |= MOD_CONTROL; }
            if key.alt { flags |= MOD_ALT; }
            if key.shift { flags |= MOD_SHIFT; }
            if key.meta { flags |= MOD_WIN; }
            let vk = u32::from(key.key.vk);
            unsafe { RegisterHotKey(self.0, id as i32, flags, vk) }.ok()?;
            Some((vk << 16) | (flags.0 & 0x000f))
        }

        pub fn unregister(&self, id: usize) -> bool {
            unsafe { UnregisterHotKey(self.0, id as i32) }.is_ok()
        }
    }

    /// RegisterHotKey swallows the press but not the async key state, which also reads
    /// as up when the user leaves the input desktop, so a hold cannot get stuck.
    /// Only the main key counts: releasing it ends the hold even if modifiers stay down.
    pub fn watch_release(inbox: Inbox, vk: u16, token: u64) {
        let watcher = inbox.clone();
        let spawned = thread::Builder::new().name("trigger-release".into()).spawn(move || loop {
            thread::sleep(RELEASE_POLL);
            if unsafe { GetAsyncKeyState(i32::from(vk)) } >= 0 {
                watcher.post(Message::Released { token });
                return;
            }
        });
        if spawned.is_err() {
            // Never leave a hold running without a way to end it.
            inbox.post(Message::Released { token });
        }
    }
}

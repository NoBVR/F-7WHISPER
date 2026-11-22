use crate::{
    mouse,
    settings::{Hotkey, Hotkeys},
    Inbox, Message,
};
use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Start,
    Stop,
    Trigger,
}

#[derive(Clone, Copy)]
enum Registration {
    Plugin(Shortcut),
    Mouse(usize),
    #[cfg(target_os = "macos")]
    Combo(u64),
    #[cfg(windows)]
    Native { id: usize, signature: u32 },
}

struct Entry {
    key: Hotkey,
    role: Option<Role>,
    token: u64,
    registration: Registration,
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
    mouse: Option<mouse::Listener>,
    inbox: Inbox,
    #[cfg(windows)]
    hook: Option<native::Hook>,
}

fn plugin_shortcut(h: &Hotkey) -> Option<Shortcut> {
    // Windows registers raw virtual keys through the native path. On macOS
    // these codes have no physical key mapping.
    if h.with.is_some() || h.key.code.starts_with("Vk") {
        return None;
    }

    #[cfg(target_os = "macos")]
    let code: Code = h.key.code.parse().ok()?;

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

fn install_plugin(app: &AppHandle, shortcut: Shortcut, token: u64) -> bool {
    app.global_shortcut().on_shortcut(shortcut, move |app, _, event| {
        app.state::<Inbox>().post(Message::ShortcutEdge {
            token,
            pressed: event.state == ShortcutState::Pressed,
        });
    }).is_ok()
}

impl Bindings {
    pub fn new(window: &WebviewWindow) -> Self {
        let app = window.app_handle();
        Self {
            plugin: app.plugin(tauri_plugin_global_shortcut::Builder::new().build()).is_ok(),
            tokens: 0,
            entries: Vec::new(),
            mouse: None,
            inbox: app.state::<Inbox>().inner().clone(),
            #[cfg(windows)]
            hook: native::Hook::install(window),
        }
    }

    fn next_token(&mut self) -> u64 {
        self.tokens = self.tokens.wrapping_add(1).max(1);
        self.tokens
    }

    pub fn apply(
        &mut self,
        app: &AppHandle,
        keys: &Hotkeys,
        triggers: &[Hotkey],
        two_key: bool,
        paused: bool,
        access_ok: bool,
        refresh_plugin: bool,
    ) -> Vec<Hotkey> {
        let mut wanted: Vec<(Role, &Hotkey)> = Vec::new();
        if !paused {
            wanted.extend(keys.start.iter().map(|key| (Role::Start, key)));
            wanted.extend(keys.stop.iter().map(|key| (Role::Stop, key)));
            for key in triggers {
                if !wanted.iter().any(|(_, existing)| existing.same_binding(key)) {
                    wanted.push((Role::Trigger, key));
                }
            }
        }
        wanted.retain(|(_, key)| {
            if key.with.is_some() {
                cfg!(target_os = "macos") && two_key && access_ok
            } else {
                access_ok || mouse::slot(key).is_none()
            }
        });
        // A combo that shares a key with a one-key binding (Start all, Stop all
        // or another row's activate key) stays unregistered.
        let singles: Vec<Hotkey> = wanted.iter()
            .filter(|(_, key)| key.with.is_none())
            .map(|(_, key)| (*key).clone())
            .collect();
        wanted.retain(|(_, key)| !singles.iter().any(|single| key.overlaps(single)));

        let mut released = Vec::new();
        let mut kept = Vec::new();
        for mut entry in std::mem::take(&mut self.entries) {
            let same = wanted.iter().find(|(role, key)| {
                entry.role == Some(*role) && key.same_binding(&entry.key)
            });
            if let Some((_, key)) = same {
                entry.key = (*key).clone();
                let keep = match entry.registration {
                    // A lost worker took its registrations with it.
                    Registration::Mouse(_) => self.mouse.is_some(),
                    #[cfg(target_os = "macos")]
                    Registration::Combo(_) => self.mouse.is_some(),
                    Registration::Plugin(shortcut) if refresh_plugin => {
                        // Preserve the token and down state across a successful
                        // refresh so a physically held trigger keeps its release.
                        app.global_shortcut().unregister(shortcut).is_err()
                            || install_plugin(app, shortcut, entry.token)
                    }
                    _ => true,
                };
                if keep {
                    kept.push(entry);
                    continue;
                }
                if entry.down && entry.role == Some(Role::Trigger) {
                    released.push(entry.key);
                }
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

        if wanted.iter().any(|(_, key)| key.with.is_some() || mouse::slot(key).is_some()) {
            if self.mouse.is_none() {
                let generation = self.next_token();
                self.mouse = mouse::Listener::start(self.inbox.clone(), generation);
            }
        } else {
            self.mouse.take();
        }

        for (role, key) in wanted {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.key.same_binding(key)) {
                entry.role.get_or_insert(role);
            } else if let Some(entry) = self.register(app, role, key) {
                self.entries.push(entry);
            }
        }
        released
    }

    fn register(&mut self, app: &AppHandle, role: Role, key: &Hotkey) -> Option<Entry> {
        let token = self.next_token();
        let registration = if key.with.is_some() {
            #[cfg(target_os = "macos")]
            {
                self.mouse.as_ref()?.add_combo(key, token)?;
                Registration::Combo(token)
            }
            #[cfg(not(target_os = "macos"))]
            return None;
        } else if let Some(slot) = mouse::slot(key) {
            self.mouse.as_ref()?.set(slot, token);
            Registration::Mouse(slot)
        } else if let Some(shortcut) = plugin_shortcut(key).filter(|_| self.plugin) {
            if !install_plugin(app, shortcut, token) {
                return None;
            }
            Registration::Plugin(shortcut)
        } else {
            self.register_native(key)?
        };
        Some(Entry { key: key.clone(), role: Some(role), token, registration, down: false })
    }

    #[cfg(windows)]
    fn register_native(&self, key: &Hotkey) -> Option<Registration> {
        let hook = self.hook.as_ref()?;
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
            Registration::Mouse(slot) => {
                if let Some(mouse) = &self.mouse {
                    mouse.set(slot, 0);
                }
                true
            }
            #[cfg(target_os = "macos")]
            Registration::Combo(token) => {
                if let Some(mouse) = &self.mouse {
                    mouse.remove_combo(token);
                }
                true
            }
            Registration::Plugin(shortcut) => app.global_shortcut().unregister(shortcut).is_ok(),
            #[cfg(windows)]
            Registration::Native { id, .. } => {
                self.hook.as_ref().is_some_and(|hook| hook.unregister(id))
            }
        }
    }

    pub fn mouse_offline(&mut self, generation: u64) -> bool {
        if self.mouse.as_ref().is_some_and(|mouse| mouse.generation == generation) {
            self.mouse.take();
            return true;
        }
        false
    }

    fn registered(&self, role: Role) -> bool {
        self.entries.iter().any(|e| e.role == Some(role))
    }

    pub fn ok(&self) -> (bool, bool) {
        (self.registered(Role::Start), self.registered(Role::Stop))
    }

    pub fn trigger_ok(&self, key: &Hotkey) -> bool {
        self.entries.iter().any(|e| e.role == Some(Role::Trigger) && e.key.same_binding(key))
    }

    pub fn edge(&mut self, token: u64, pressed: bool) -> Option<Hit> {
        let index = self.entries.iter().position(|e| e.token == token)?;
        if pressed {
            self.press(index)
        } else {
            #[cfg(windows)]
            if matches!(self.entries[index].registration, Registration::Plugin(_)) {
                return None;
            }
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
        let role = entry.role?;
        if entry.down {
            return None;
        }

        let mouse = matches!(entry.registration, Registration::Mouse(_));
        // Windows keyboard Start/Stop already use MOD_NOREPEAT. Mouse and
        // macOS registrations deliver both edges for every role.
        entry.down = mouse || !cfg!(windows) || role == Role::Trigger;
        #[cfg(windows)]
        if !mouse && role == Role::Trigger {
            native::watch_release(self.inbox.clone(), entry.key.key.vk, entry.token);
        }

        Some(match role {
            Role::Start => Hit::StartAll,
            Role::Stop => Hit::StopAll,
            Role::Trigger => Hit::Down(entry.key.clone()),
        })
    }

    fn release(&mut self, index: usize) -> Option<Hit> {
        let entry = &mut self.entries[index];
        let was_down = std::mem::take(&mut entry.down);
        (was_down && entry.role == Some(Role::Trigger)).then(|| Hit::Up(entry.key.clone()))
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
            inbox.post(Message::Released { token });
        }
    }
}

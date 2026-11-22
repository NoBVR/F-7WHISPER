use crate::{settings::Hotkey, Inbox, Message};
use std::{
    cell::Cell,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
};

#[cfg_attr(windows, path = "mouse_windows.rs")]
#[cfg_attr(target_os = "macos", path = "mouse_macos.rs")]
mod platform;

const MODIFIER_COMBINATIONS: usize = 16;
#[derive(Default)]
struct Table {
    slots: [AtomicU64; 2 * MODIFIER_COMBINATIONS],
    #[cfg(target_os = "macos")]
    combos: std::sync::Mutex<Vec<Combo>>,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct Combo {
    keys: u128,
    mods: usize,
    token: u64,
}

fn modifiers(ctrl: bool, alt: bool, shift: bool, meta: bool) -> usize {
    (ctrl as usize)
        | ((alt as usize) << 1)
        | ((shift as usize) << 2)
        | ((meta as usize) << 3)
}

pub fn slot(key: &Hotkey) -> Option<usize> {
    #[cfg(windows)]
    let side = match key.key.vk {
        5 => 0,
        6 => 1,
        _ => return None,
    };

    #[cfg(target_os = "macos")]
    let side = match key.key.code.as_str() {
        "MouseBack" => 0,
        "MouseForward" => 1,
        _ => return None,
    };

    Some(side * MODIFIER_COMBINATIONS + modifiers(key.ctrl, key.alt, key.shift, key.meta))
}

// Only the native callback thread accesses `held`. The main thread publishes
// registration tokens through the table. Zero means the event passes through.
struct Tracker {
    table: Arc<Table>,
    held: [Cell<u64>; 2],
    inbox: Inbox,
}

impl Tracker {
    fn edge(&self, side: usize, mods: usize, down: bool) -> bool {
        let held = &self.held[side];
        let token = if down {
            if held.get() != 0 {
                return true;
            }
            let token = self.table.slots[side * MODIFIER_COMBINATIONS + mods].load(Ordering::Acquire);
            held.set(token);
            token
        } else {
            // A captured down owns its up even if modifiers or bindings changed.
            held.replace(0)
        };
        if token == 0 {
            return false;
        }
        self.inbox.post(Message::ShortcutEdge { token, pressed: down });
        true
    }
}

pub struct Listener {
    pub generation: u64,
    table: Arc<Table>,
    stop: platform::Stop,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    pub fn start(inbox: Inbox, generation: u64) -> Option<Self> {
        let table = Arc::new(Table::default());
        let tracker = Tracker {
            table: table.clone(),
            held: [Cell::new(0), Cell::new(0)],
            inbox: inbox.clone(),
        };
        let (ready, result) = mpsc::channel();
        let thread = thread::Builder::new().name("mouse-hotkeys".into()).spawn(move || {
            platform::run(tracker, ready);
            inbox.post(Message::MouseOffline { generation });
        }).ok()?;
        match result.recv() {
            Ok(stop) => Some(Self { generation, table, stop, thread: Some(thread) }),
            Err(_) => {
                let _ = thread.join();
                None
            }
        }
    }

    pub fn set(&self, slot: usize, token: u64) {
        self.table.slots[slot].store(token, Ordering::Release);
    }

    #[cfg(target_os = "macos")]
    pub fn add_combo(&self, key: &Hotkey, token: u64) -> Option<()> {
        let a = crate::keys::map(&key.key)?;
        let b = crate::keys::map(key.with.as_ref()?)?;
        if a >= 128 || b >= 128 || a == b {
            return None;
        }
        self.table.combos.lock().ok()?.push(Combo {
            keys: (1u128 << a) | (1u128 << b),
            mods: modifiers(key.ctrl, key.alt, key.shift, key.meta),
            token,
        });
        Some(())
    }

    #[cfg(target_os = "macos")]
    pub fn remove_combo(&self, token: u64) {
        if let Ok(mut combos) = self.table.combos.lock() {
            combos.retain(|combo| combo.token != token);
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            if !thread.is_finished() {
                self.stop.signal(&thread);
            }
            let _ = thread.join();
        }
    }
}

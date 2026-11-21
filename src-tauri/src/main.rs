#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod settings;
mod wait;
mod window;

#[cfg(windows)]
#[path = "windows_keys.rs"]
mod keys;

#[cfg(target_os = "macos")]
#[path = "macos_keys.rs"]
mod keys;

use engine::Engine;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use settings::{Bounds, Key, Settings};
use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewWindow, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const COUNTS: Duration = Duration::from_millis(200);
const SAVE: Duration = Duration::from_millis(500);
const ACCESS: Duration = Duration::from_millis(1500);

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Message {
    Ready,
    Window { action: String, edge: Option<String> },
    Pin { on: bool },
    Toggle { id: String },
    StartAll,
    StopAll,
    Add {
        key: Key,
        #[serde(rename = "intervalMs")]
        interval_ms: i64,
    },
    Edit {
        id: String,
        #[serde(default, deserialize_with = "present")]
        key: Option<Key>,
        #[serde(default, rename = "intervalMs", deserialize_with = "present")]
        interval_ms: Option<i64>,
    },
    Remove { id: String },
    OpenAccessSettings,
    #[serde(skip)]
    Activate,
}

struct Inbox {
    messages: Sender<Message>,
    wake: Sender<()>,
}

fn enqueue(app: &AppHandle, message: Message) {
    let inbox = app.state::<Inbox>();
    if inbox.messages.send(message).is_ok() {
        // Commands, the F7 hotkey and single-instance callbacks run on the main thread, where
        // run_on_main_thread runs inline and does not wake the event loop; a helper thread
        // posts the wake-up instead (see main).
        let _ = inbox.wake.send(());
    }
}

#[tauri::command]
fn page_message(app: AppHandle, msg: Value) {
    if let Ok(message) = serde_json::from_value(msg) {
        enqueue(&app, message);
    }
}

// Wake the UI event loop at its next deadline, including when the window is minimized.
struct Alarm {
    sender: Sender<Instant>,
    armed: Option<Instant>,
}

impl Alarm {
    fn new(app: AppHandle) -> Self {
        let (sender, receiver) = mpsc::channel::<Instant>();
        thread::spawn(move || {
            while let Ok(mut due) = receiver.recv() {
                loop {
                    match receiver.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(replacement) => due = replacement,
                        Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => {
                            if app.run_on_main_thread(|| {}).is_err() {
                                return;
                            }
                            break;
                        }
                    }
                }
            }
        });
        Self { sender, armed: None }
    }

    fn arm(&mut self, due: Option<Instant>) {
        if self.armed != due {
            self.armed = due;
            if let Some(due) = due {
                let _ = self.sender.send(due);
            }
        }
    }
}

struct Host {
    window: WebviewWindow,
    engine: Engine,
    alarm: Alarm,
    bounds: Option<Bounds>,
    pinned: bool,
    maximized: bool,
    hotkey_ok: bool,
    access_ok: bool,
    prompted: bool,
    ready: bool,
    shown: bool,
    closed: bool,
    geometry_changed: bool,
    show_at: Option<Instant>,
    counts_at: Option<Instant>,
    save_at: Option<Instant>,
    access_at: Option<Instant>,
}

impl Host {
    fn new(app: &AppHandle) -> Self {
        let window = app.get_webview_window("main").expect("configured main window");
        let settings = Settings::load();
        let restored = settings.window.and_then(|b| window::restore(&window, b));
        if restored.is_none() {
            let _ = window.center();
        }
        let maximized = restored.is_some_and(|b| b.maximized);
        let bounds = restored.or_else(|| window::normal_bounds(&window));
        let _ = window.set_always_on_top(settings.pinned);
        let pinned = window.is_always_on_top().unwrap_or(settings.pinned);
        let engine = Engine::new(
            settings.rows,
            keys::Keyboard::new(&window),
            window.is_focused().unwrap_or(true),
        );

        let plugin = tauri_plugin_global_shortcut::Builder::new().build();
        let hotkey_ok = app.plugin(plugin).is_ok()
            && app.global_shortcut().on_shortcut("F7", |app, _, event| {
                if event.state == ShortcutState::Pressed {
                    enqueue(app, Message::StopAll);
                }
            }).is_ok();

        let now = Instant::now();
        Self {
            window,
            engine,
            alarm: Alarm::new(app.clone()),
            bounds,
            pinned,
            maximized,
            hotkey_ok,
            access_ok: keys::trusted(),
            prompted: false,
            ready: false,
            shown: false,
            closed: false,
            geometry_changed: true,
            show_at: Some(now + Duration::from_secs(3)),
            counts_at: None,
            save_at: None,
            access_at: cfg!(target_os = "macos").then_some(now + ACCESS),
        }
    }

    fn reveal(&mut self) {
        if !self.shown {
            if self.window.show().is_err() {
                self.show_at = Some(Instant::now() + Duration::from_millis(250));
                return;
            }
            self.shown = true;
            self.show_at = None;
            if self.maximized {
                let _ = self.window.maximize();
            }
            let _ = self.window.set_focus();
            self.engine.focus(self.window.is_focused().unwrap_or(true));
            self.geometry_changed = true;
        }
    }

    fn dirty(&mut self) {
        self.save_at = Some(Instant::now() + SAVE);
    }

    fn capture(&mut self) -> bool {
        if !self.shown {
            return false;
        }
        let Ok(minimized) = self.window.is_minimized() else { return false };
        let Ok(zoomed) = self.window.is_maximized() else { return false };
        let fullscreen = self.window.is_fullscreen().unwrap_or(false);
        let maximized = !minimized && (zoomed || fullscreen);
        let old = self.bounds;

        if !minimized && !fullscreen {
            if zoomed {
                if let Some(bounds) = &mut self.bounds {
                    bounds.maximized = true;
                }
            } else if let Some(bounds) = window::normal_bounds(&self.window) {
                self.bounds = Some(bounds);
            }
        }
        if self.bounds != old {
            self.dirty();
        }
        let changed = self.maximized != maximized;
        self.maximized = maximized;
        changed
    }

    fn state(&mut self) {
        if !self.ready || self.closed {
            return;
        }
        let (rows, running) = {
            let mut rows = self.engine.rows();
            (rows.snapshot(), rows.any_running())
        };
        if running {
            self.counts_at.get_or_insert_with(|| Instant::now() + COUNTS);
        } else {
            self.counts_at = None;
        }
        let _ = self.window.emit_to("main", "host", json!({
            "type": "state",
            "rows": rows,
            "pinned": self.pinned,
            "maximized": self.maximized,
            "hotkey": "F7",
            "hotkeyOk": self.hotkey_ok,
            "platform": std::env::consts::OS,
            "accessOk": self.access_ok
        }));
    }

    fn refresh_access(&mut self) -> bool {
        let trusted = keys::trusted();
        if self.access_ok == trusted {
            return false;
        }
        self.access_ok = trusted;
        if !trusted {
            self.engine.change(|rows| rows.set_running(None, false));
        }
        true
    }

    fn allow_start(&mut self) -> bool {
        self.refresh_access();
        if !self.access_ok && !self.prompted {
            self.prompted = true;
            keys::prompt();
        }
        self.access_ok
    }

    fn save(&self) -> bool {
        let settings = Settings {
            rows: self.engine.rows().configuration(),
            pinned: self.pinned,
            window: self.bounds,
        };
        if let Err(error) = settings.save() {
            eprintln!("Could not save F-7WHISPER settings: {error}");
            return false;
        }
        true
    }

    fn finish(&mut self, capture: bool) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.engine.shutdown();
        if capture {
            self.capture();
        }
        self.save();
    }

    fn close(&mut self) {
        self.finish(true);
        self.window.app_handle().exit(0);
    }

    fn message(&mut self, message: Message) {
        match message {
            Message::Ready => {
                self.ready = true;
                self.reveal();
                self.capture();
                self.refresh_access();
                self.state();
                return;
            }
            Message::Activate => {
                self.reveal();
                if self.window.is_minimized().unwrap_or(false) {
                    let _ = self.window.unminimize();
                }
                let _ = self.window.show();
                let _ = self.window.set_focus();
                return;
            }
            Message::Window { action, edge } => {
                match action.as_str() {
                    "minimize" => { let _ = self.window.minimize(); }
                    "maximize" => {
                        if self.window.is_maximized().unwrap_or(false) {
                            let _ = self.window.unmaximize();
                        } else {
                            let _ = self.window.maximize();
                        }
                    }
                    "close" => self.close(),
                    "drag" => { let _ = self.window.start_dragging(); }
                    "resize" => window::resize(&self.window, edge.as_deref()),
                    _ => return,
                }
                self.geometry_changed = true;
                return;
            }
            Message::Pin { on } => {
                if self.window.set_always_on_top(on).is_ok() {
                    self.pinned = on;
                }
            }
            Message::Toggle { id } => {
                let running = self.engine.rows().running(&id);
                let Some(running) = running else { return };
                if running || self.allow_start() {
                    self.engine.change(|rows| rows.set_running(Some(&id), !running));
                }
            }
            Message::StartAll => {
                if self.allow_start() {
                    self.engine.change(|rows| rows.set_running(None, true));
                }
            }
            Message::StopAll => self.engine.change(|rows| rows.set_running(None, false)),
            Message::Add { key, interval_ms } => {
                if !key.valid() {
                    return;
                }
                self.engine.change(|rows| rows.add(key, interval_ms));
            }
            Message::Edit { id, key, interval_ms } => {
                if key.as_ref().is_some_and(|k| !k.valid()) || (key.is_none() && interval_ms.is_none()) {
                    return;
                }
                self.engine.change(|rows| rows.edit(&id, key, interval_ms));
            }
            Message::Remove { id } => self.engine.change(|rows| rows.remove(&id)),
            Message::OpenAccessSettings => {
                keys::open_settings();
                return;
            }
        }
        self.dirty();
        self.state();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.show_at.is_some_and(|t| t <= now) {
            self.reveal();
        }
        let mut state_changed = false;
        if self.geometry_changed {
            self.geometry_changed = false;
            state_changed = self.capture();
        }
        if self.access_at.is_some_and(|t| t <= now) {
            self.access_at = Some(now + ACCESS);
            state_changed |= self.refresh_access();
        }
        if state_changed {
            self.state();
        }
        if self.counts_at.is_some_and(|t| t <= now) {
            self.counts_at = Some(now + COUNTS);
            let counts = self.engine.rows().counts();
            if !counts.is_empty() {
                let _ = self.window.emit_to("main", "host", json!({
                    "type": "counts", "counts": counts
                }));
            }
        }
        if self.save_at.is_some_and(|t| t <= now) {
            self.save_at = if self.save() { None } else { Some(now + Duration::from_secs(1)) };
        }
        self.alarm.arm(
            [self.show_at, self.counts_at, self.save_at, self.access_at]
                .into_iter().flatten().min()
        );
    }

    fn dispatch(&mut self, receiver: &Receiver<Message>) {
        for message in receiver.try_iter() {
            self.message(message);
            if self.closed {
                return;
            }
        }
        self.tick();
    }
}

fn main() {
    #[cfg(windows)]
    std::env::set_var("WEBVIEW2_DEFAULT_BACKGROUND_COLOR", "FF0A0A0A");

    let (sender, receiver) = mpsc::channel();
    let (wake, wakeups) = mpsc::channel::<()>();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            enqueue(app, Message::Activate);
        }))
        .manage(Inbox { messages: sender, wake })
        .invoke_handler(tauri::generate_handler![page_message])
        .build(tauri::generate_context!())
        .expect("could not initialize F-7WHISPER");

    let handle = app.handle().clone();
    thread::spawn(move || {
        while wakeups.recv().is_ok() {
            while wakeups.try_recv().is_ok() {}
            if handle.run_on_main_thread(|| {}).is_err() {
                return;
            }
        }
    });

    let mut host: Option<Host> = None;
    app.run(move |app, event| {
        if matches!(&event, RunEvent::Ready) {
            host = Some(Host::new(app));
        }
        let Some(host) = host.as_mut() else { return };
        if host.closed {
            return;
        }
        match event {
            RunEvent::Ready | RunEvent::MainEventsCleared => host.dispatch(&receiver),
            RunEvent::WindowEvent { label, event, .. } if label == "main" => match event {
                WindowEvent::Focused(focused) => host.engine.focus(focused),
                WindowEvent::Moved(_) | WindowEvent::Resized(_) |
                WindowEvent::ScaleFactorChanged { .. } => host.geometry_changed = true,
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    host.close();
                }
                _ => {}
            },
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => host.message(Message::Activate),
            RunEvent::ExitRequested { .. } => host.finish(app.get_webview_window("main").is_some()),
            RunEvent::Exit => host.finish(false),
            _ => {}
        }
    });
}

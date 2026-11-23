#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod hotkeys;
mod mouse;
mod settings;
#[cfg(target_os = "macos")]
mod update;
mod wait;
mod window;

#[cfg_attr(windows, path = "windows_keys.rs")]
#[cfg_attr(target_os = "macos", path = "macos_keys.rs")]
mod keys;

#[cfg_attr(windows, path = "desktop_windows.rs")]
#[cfg_attr(target_os = "macos", path = "desktop_macos.rs")]
mod desktop;

use engine::Engine;
use hotkeys::{Bindings, Hit};
use serde::Deserialize;
use serde_json::{json, Value};
use settings::{Hotkey, Hotkeys, Input, Position, Settings};
use std::{
    collections::BTreeMap,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewWindow, WindowEvent};

const COUNTS: Duration = Duration::from_millis(200);
const SAVE: Duration = Duration::from_millis(500);
const ACCESS: Duration = Duration::from_millis(1500);
const PICK: Duration = Duration::from_millis(8);

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Message {
    Ready,
    Window { action: String },
    Pin { on: bool },
    Toggle { id: String },
    StartAll,
    StopAll,
    Add { input: Value },
    Edit { id: String, input: Value },
    Remove { id: String },
    Settings {
        hotkeys: Hotkeys,
        #[serde(rename = "switchStop")]
        switch_stop: bool,
        #[serde(default, rename = "twoKey")]
        two_key: Option<bool>,
    },
    PauseHotkeys { on: bool },
    ListApps,
    PickPoint,
    PickCancel,
    Fit {
        height: i64,
        #[serde(default)]
        width: Option<i64>,
    },
    OpenAccessSettings,
    #[cfg(target_os = "macos")]
    InstallUpdate,
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    Update(update::Event),
    #[serde(skip)]
    Activate,
    #[serde(skip)]
    Publish,
    #[serde(skip)]
    ShortcutEdge { token: u64, pressed: bool },
    #[serde(skip)]
    MouseOffline { generation: u64 },
    #[cfg(windows)]
    #[serde(skip)]
    NativeHotkey { id: usize, signature: u32 },
    #[cfg(windows)]
    #[serde(skip)]
    Released { token: u64 },
}

#[derive(Clone)]
struct Inbox {
    messages: Sender<Message>,
    wake: Sender<()>,
}

impl Inbox {
    fn post(&self, message: Message) {
        if self.messages.send(message).is_ok() {
            // run_on_main_thread executes inline for a main-thread caller.
            let _ = self.wake.send(());
        }
    }
}

#[tauri::command]
fn page_message(app: AppHandle, msg: Value) {
    if let Ok(message) = serde_json::from_value(msg) {
        app.state::<Inbox>().post(message);
    }
}

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

struct Picker {
    was_down: bool,
    due: Instant,
}

struct Host {
    window: WebviewWindow,
    engine: Engine,
    alarm: Alarm,
    #[cfg(target_os = "macos")]
    updater: update::Updater,
    position: Option<Position>,
    width: i64,
    height: i64,
    refit: bool,
    geometry_changed: bool,
    pinned: bool,
    hotkeys: Hotkeys,
    bindings: Bindings,
    paused: bool,
    switch_stop: bool,
    two_key: bool,
    picker: Option<Picker>,
    access_ok: bool,
    prompted: bool,
    ready: bool,
    shown: bool,
    closed: bool,
    show_at: Option<Instant>,
    counts_at: Option<Instant>,
    save_at: Option<Instant>,
    access_at: Option<Instant>,
}

impl Host {
    fn new(app: &AppHandle) -> Self {
        let window = app.get_webview_window("main").expect("configured main window");
        let settings = Settings::load();
        let height = window::initial_height(settings.rows.len());
        window::initialize(&window, settings.window, window::WIDTH, height);
        let position = window::position(&window).or(settings.window);
        let _ = window.set_always_on_top(settings.pinned);
        let pinned = window.is_always_on_top().unwrap_or(settings.pinned);
        let inbox = app.state::<Inbox>().inner().clone();
        let engine = Engine::new(
            settings.rows, keys::Keyboard::new(&window), window.is_focused().unwrap_or(true),
            settings.switch_stop, move || inbox.post(Message::Publish),
        );
        let access_ok = keys::trusted();
        let mut bindings = Bindings::new(&window);
        let triggers = engine.rows().trigger_keys();
        bindings.apply(app, &settings.hotkeys, &triggers, settings.two_key, false, access_ok, false);
        let now = Instant::now();
        Self {
            window,
            engine,
            alarm: Alarm::new(app.clone()),
            #[cfg(target_os = "macos")]
            updater: update::Updater::new(app),
            position,
            width: window::WIDTH,
            height,
            refit: false,
            geometry_changed: true,
            pinned,
            hotkeys: settings.hotkeys,
            bindings,
            paused: false,
            switch_stop: settings.switch_stop,
            two_key: settings.two_key,
            picker: None,
            access_ok,
            prompted: false,
            ready: false,
            shown: false,
            closed: false,
            show_at: Some(now + Duration::from_secs(3)),
            counts_at: None,
            save_at: None,
            access_at: cfg!(target_os = "macos").then_some(now + ACCESS),
        }
    }

    fn emit(&self, value: Value) {
        if self.ready && !self.closed {
            let _ = self.window.emit_to("main", "host", value);
        }
    }

    fn reveal(&mut self) {
        if self.shown {
            return;
        }
        if self.window.show().is_err() {
            self.show_at = Some(Instant::now() + Duration::from_millis(250));
            return;
        }
        self.shown = true;
        self.show_at = None;
        let _ = self.window.set_focus();
        self.engine.focus(self.window.is_focused().unwrap_or(true));
        self.geometry_changed = true;
    }

    fn activate(&mut self) {
        self.reveal();
        if self.window.is_minimized().unwrap_or(false) {
            let _ = self.window.unminimize();
        }
        let _ = self.window.show();
        let _ = self.window.set_focus();
    }

    fn dirty(&mut self) {
        self.save_at = Some(Instant::now() + SAVE);
    }

    fn capture(&mut self) {
        if self.shown {
            if let Some(p) = window::position(&self.window) {
                if self.position != Some(p) {
                    self.position = Some(p);
                    self.dirty();
                }
            }
        }
    }

    fn state(&mut self) {
        if !self.ready || self.closed {
            return;
        }
        let bindings = &self.bindings;
        let (rows, running) = {
            let mut rows = self.engine.rows();
            (rows.snapshot(|key| bindings.trigger_ok(key)), rows.any_running())
        };
        let (start_ok, stop_ok) = bindings.ok();
        if running {
            self.counts_at.get_or_insert_with(|| Instant::now() + COUNTS);
        } else {
            self.counts_at = None;
        }
        self.emit(json!({
            "type": "state",
            "version": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS,
            "accessOk": self.access_ok,
            "pinned": self.pinned,
            "rows": rows,
            "hotkeys": self.hotkeys,
            "hotkeyOk": { "start": start_ok, "stop": stop_ok },
            "switchStop": self.switch_stop,
            "twoKey": self.two_key
        }));
    }

    fn refresh_access(&mut self) -> bool {
        let trusted = keys::trusted();
        if trusted == self.access_ok {
            return false;
        }
        self.access_ok = trusted;
        if !trusted {
            self.engine.change(|rows| rows.set_running(None, false));
        }
        true
    }

    fn allow_start(&mut self) -> bool {
        if self.refresh_access() {
            self.rebind(false);
        }
        if !self.access_ok && !self.prompted {
            self.prompted = true;
            keys::prompt();
        }
        self.access_ok
    }

    fn all(&mut self, start: bool) {
        if !start || self.allow_start() {
            self.engine.change(|rows| rows.set_running(None, start));
        }
    }

    fn rebind(&mut self, refresh_plugin: bool) -> bool {
        let triggers = self.engine.rows().trigger_keys();
        let released = self.bindings.apply(
            self.window.app_handle(), &self.hotkeys, &triggers,
            self.two_key, self.paused, self.access_ok, refresh_plugin,
        );
        for key in &released {
            self.engine.change(|rows| rows.trigger_up(key));
        }
        !released.is_empty()
    }

    fn trigger(&mut self, key: &Hotkey) {
        let starts = self.engine.rows().trigger_starts(key);
        if !starts || self.allow_start() {
            self.engine.change(|rows| rows.trigger_down(key));
        }
    }

    fn hit(&mut self, hit: Option<Hit>) -> bool {
        match hit {
            None => return false,
            Some(Hit::StartAll) => self.all(true),
            Some(Hit::StopAll) => self.all(false),
            Some(Hit::Down(key)) => self.trigger(&key),
            Some(Hit::Up(key)) => self.engine.change(|rows| rows.trigger_up(&key)),
        }
        true
    }

    fn save(&self) -> bool {
        let settings = Settings {
            rows: self.engine.rows().configuration(),
            pinned: self.pinned,
            window: self.position,
            hotkeys: self.hotkeys.clone(),
            switch_stop: self.switch_stop,
            two_key: self.two_key,
        };
        if let Err(error) = settings.save() {
            eprintln!("Could not save F-7WHISPER settings: {error}");
            return false;
        }
        true
    }

    fn cancel_pick(&mut self) {
        if self.picker.take().is_some() {
            let _ = self.window.set_always_on_top(self.pinned);
        }
    }

    fn poll_pick(&mut self, now: Instant) {
        let Some(picker) = &mut self.picker else { return };
        if now < picker.due {
            return;
        }
        picker.due = now + PICK;
        let down = desktop::left_down();
        let pressed = down && !picker.was_down;
        picker.was_down = down;
        if pressed {
            if let Some(point) = desktop::outside_point().filter(|p| p.valid()) {
                self.cancel_pick();
                self.activate();
                self.emit(json!({ "type": "point", "x": point.x, "y": point.y }));
            }
        }
    }

    fn finish(&mut self, capture: bool) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.cancel_pick();
        self.engine.shutdown();
        self.paused = true;
        self.rebind(false);
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
                self.cancel_pick();
                self.paused = false;
                self.refresh_access();
                self.rebind(true);
                self.ready = true;
                self.reveal();
                self.capture();
                self.state();
                #[cfg(target_os = "macos")]
                if let Some(message) = self.updater.ready() {
                    self.emit(message);
                }
                return;
            }
            #[cfg(target_os = "macos")]
            Message::InstallUpdate => {
                if let Some(message) = self.updater.install() {
                    self.emit(message);
                }
                return;
            }
            #[cfg(target_os = "macos")]
            Message::Update(event) => {
                if let Some(message) = self.updater.handle(event) {
                    self.emit(message);
                }
                return;
            }
            Message::Activate => {
                self.activate();
                return;
            }
            Message::Publish => {}
            Message::Window { action } => {
                match action.as_str() {
                    "minimize" => { let _ = self.window.minimize(); }
                    "close" => self.close(),
                    "drag" => { let _ = self.window.start_dragging(); }
                    _ => {}
                }
                return;
            }
            Message::Pin { on } => {
                if self.window.set_always_on_top(on || self.picker.is_some()).is_ok() {
                    self.pinned = on;
                    self.dirty();
                }
            }
            Message::Toggle { id } => {
                let Some(running) = self.engine.rows().running(&id) else { return };
                if running || self.allow_start() {
                    self.engine.change(|rows| rows.set_running(Some(&id), !running));
                }
            }
            Message::StartAll => self.all(true),
            Message::StopAll => self.all(false),
            Message::ShortcutEdge { token, pressed } => {
                let hit = self.bindings.edge(token, pressed);
                if !self.hit(hit) {
                    return;
                }
            }
            Message::MouseOffline { generation } => {
                if !self.bindings.mouse_offline(generation) {
                    return;
                }
                self.refresh_access();
                self.rebind(false);
            }
            #[cfg(windows)]
            Message::NativeHotkey { id, signature } => {
                let hit = self.bindings.native_event(id, signature);
                if !self.hit(hit) {
                    return;
                }
            }
            #[cfg(windows)]
            Message::Released { token } => {
                let hit = self.bindings.released(token);
                if !self.hit(hit) {
                    return;
                }
            }
            Message::Add { input } => {
                let Some(input) = Input::from_page(&input) else { return };
                self.engine.change(|rows| rows.add(input));
                self.rebind(false);
                self.dirty();
            }
            Message::Edit { id, input } => {
                let Some(input) = Input::from_page(&input) else { return };
                self.engine.change(|rows| rows.edit(&id, input));
                self.rebind(false);
                self.dirty();
            }
            Message::Remove { id } => {
                self.engine.change(|rows| rows.remove(&id));
                self.rebind(false);
                self.dirty();
            }
            Message::Settings { hotkeys, switch_stop, two_key } => {
                self.hotkeys = hotkeys.normalized();
                self.switch_stop = switch_stop;
                self.two_key = two_key.unwrap_or(self.two_key);
                self.engine.change(|rows| rows.set_switch_stop(switch_stop));
                self.refresh_access();
                self.rebind(true);
                self.capture();
                self.save_at = (!self.save()).then(|| Instant::now() + SAVE);
            }
            Message::PauseHotkeys { on } => {
                self.paused = on;
                self.refresh_access();
                self.rebind(!on);
            }
            Message::ListApps => {
                let mut names = BTreeMap::new();
                for name in desktop::apps() {
                    if let Some(name) = settings::app_name(&name) {
                        names.entry(name.to_lowercase()).or_insert(name);
                    }
                }
                self.emit(json!({ "type": "apps", "apps": names.into_values().collect::<Vec<_>>() }));
                return;
            }
            Message::PickPoint => {
                let was_down = desktop::left_down();
                let _ = self.window.set_always_on_top(true);
                self.picker = Some(Picker { was_down, due: Instant::now() });
                return;
            }
            Message::PickCancel => {
                self.cancel_pick();
                return;
            }
            Message::Fit { height, width } => {
                self.height = height.max(1);
                if let Some(width) = width {
                    self.width = width.max(1);
                }
                self.refit = true;
                return;
            }
            Message::OpenAccessSettings => {
                keys::open_settings();
                return;
            }
        }
        self.state();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.refit && !self.window.is_minimized().unwrap_or(true) {
            self.refit = false;
            window::fit(&self.window, self.width, self.height);
            self.geometry_changed = true;
        }
        if self.show_at.is_some_and(|t| t <= now) {
            self.reveal();
        }
        if self.geometry_changed {
            self.geometry_changed = false;
            self.capture();
        }
        self.poll_pick(now);
        if self.access_at.is_some_and(|t| t <= now) {
            self.access_at = Some(now + ACCESS);
            if self.refresh_access() {
                self.rebind(false);
                self.state();
            }
        }
        if self.counts_at.is_some_and(|t| t <= now) {
            self.counts_at = Some(now + COUNTS);
            let counts = self.engine.rows().counts();
            if !counts.is_empty() {
                self.emit(json!({ "type": "counts", "counts": counts }));
            }
        }
        if self.save_at.is_some_and(|t| t <= now) {
            self.save_at = (!self.save()).then(|| now + Duration::from_secs(1));
        }
        #[cfg(target_os = "macos")]
        self.updater.tick(now);
        self.alarm.arm([
            #[cfg(target_os = "macos")]
            self.updater.due,
            self.show_at, self.counts_at, self.save_at, self.access_at,
            self.picker.as_ref().map(|p| p.due),
        ].into_iter().flatten().min());
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

    let (messages, receiver) = mpsc::channel();
    let (wake, wakeups) = mpsc::channel::<()>();
    let app = tauri::Builder::default()
        .manage(Inbox { messages, wake })
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            app.state::<Inbox>().post(Message::Activate);
        }))
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
                WindowEvent::Moved(_) | WindowEvent::Resized(_) => host.geometry_changed = true,
                WindowEvent::ScaleFactorChanged { .. } => {
                    host.refit = true;
                    host.geometry_changed = true;
                }
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
